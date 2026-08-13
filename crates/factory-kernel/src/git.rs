//! Closed Git custody for disposable candidate trees and local delivery.
//!
//! The module has no arbitrary-Git or remote-operation entry point. Every
//! invocation is assembled below with an exact executable, direct argv,
//! deterministic environment, bounded streams, and a wall deadline.

use std::{
    collections::BTreeSet,
    fs,
    io::{self, Read, Write},
    os::unix::process::CommandExt as _,
    path::{Component, Path, PathBuf},
    process::{Command, ExitStatus, Stdio},
    sync::{
        Arc,
        atomic::{AtomicBool, AtomicU64, Ordering},
    },
    thread,
    time::{Duration, Instant},
};

use factory_protocol::{
    ApplicationRevisionId, CampaignId, CandidateId, ContentDigest, KernelBuildId, TicketId,
    ValidationId,
};
use rustix::process::{Pid, Signal, kill_process_group};
use thiserror::Error;

/// Default bounded Git process contract.
pub const DEFAULT_STREAM_LIMIT: u64 = 4 * 1024 * 1024;
pub const DEFAULT_DEADLINE: Duration = Duration::from_secs(30);
const TERMINATION_GRACE: Duration = Duration::from_secs(1);

/// A safe default-branch component, never an arbitrary ref expression.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DefaultBranchName(String);

impl DefaultBranchName {
    pub fn parse(value: impl Into<String>) -> Result<Self, GitCustodyError> {
        let value = value.into();
        if value.is_empty()
            || value.len() > 240
            || value.starts_with('.')
            || value.ends_with('.')
            || value.starts_with('/')
            || value.ends_with('/')
            || value.contains("..")
            || value.contains("//")
            || value.contains("@{")
            || value.bytes().any(|byte| {
                byte.is_ascii_whitespace()
                    || matches!(byte, b'~' | b'^' | b':' | b'?' | b'*' | b'[' | b'\\' | 0)
            })
        {
            return Err(GitCustodyError::InvalidDefaultBranch);
        }
        Ok(Self(value))
    }

    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }

    fn reference(&self) -> String {
        format!("refs/heads/{}", self.0)
    }
}

/// A kernel-owned runtime directory component for a disposable worktree.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct WorktreeName(String);

impl WorktreeName {
    pub fn parse(value: impl Into<String>) -> Result<Self, GitCustodyError> {
        let value = value.into();
        if value.is_empty()
            || value.len() > 120
            || !value.bytes().all(|byte| {
                byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'-' || byte == b'_'
            })
        {
            return Err(GitCustodyError::InvalidWorktreeName);
        }
        Ok(Self(value))
    }

    fn as_str(&self) -> &str {
        &self.0
    }
}

/// Runtime partitions only; applications cannot select an arbitrary path.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum WorktreeKind {
    Actor,
    Review,
    Validation,
}

impl WorktreeKind {
    const fn path_component(self) -> &'static str {
        match self {
            Self::Actor => "actor",
            Self::Review => "review",
            Self::Validation => "validation",
        }
    }
}

macro_rules! object_id {
    ($name:ident) => {
        #[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
        pub struct $name(String);

        impl $name {
            pub fn parse(value: impl Into<String>) -> Result<Self, GitCustodyError> {
                let value = value.into();
                if !matches!(value.len(), 40 | 64)
                    || !value
                        .bytes()
                        .all(|byte| byte.is_ascii_digit() || matches!(byte, b'a'..=b'f'))
                {
                    return Err(GitCustodyError::InvalidObjectId { value });
                }
                Ok(Self(value))
            }

            #[must_use]
            pub fn as_str(&self) -> &str {
                &self.0
            }
        }

        impl std::fmt::Display for $name {
            fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                formatter.write_str(&self.0)
            }
        }
    };
}

object_id!(GitCommitId);
object_id!(GitTreeId);

/// Exact base evidence from clean repository qualification.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RepositorySnapshot {
    base_commit: GitCommitId,
    base_tree: GitTreeId,
}

impl RepositorySnapshot {
    #[must_use]
    pub fn base_commit(&self) -> &GitCommitId {
        &self.base_commit
    }

    #[must_use]
    pub fn base_tree(&self) -> &GitTreeId {
        &self.base_tree
    }
}

/// A clean local checkout that passed Git safety qualification.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct QualifiedRepository {
    root: PathBuf,
    branch: DefaultBranchName,
    snapshot: RepositorySnapshot,
}

impl QualifiedRepository {
    #[must_use]
    pub fn root(&self) -> &Path {
        &self.root
    }

    /// The exact default branch proved during qualification.  Runtime
    /// authorities use this to bind an admitted application repository policy
    /// to the snapshot they receive; it does not permit ref mutation.
    #[must_use]
    pub fn default_branch(&self) -> &DefaultBranchName {
        &self.branch
    }

    #[must_use]
    pub fn snapshot(&self) -> &RepositorySnapshot {
        &self.snapshot
    }
}

/// A detached worktree whose path is known to be below the runtime root.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct OwnedWorktree {
    path: PathBuf,
    repository_root: PathBuf,
    branch: DefaultBranchName,
    base_commit: GitCommitId,
    base_tree: GitTreeId,
    materialized_tree: GitTreeId,
}

impl OwnedWorktree {
    #[must_use]
    pub fn path(&self) -> &Path {
        &self.path
    }

    #[must_use]
    pub fn materialized_tree(&self) -> &GitTreeId {
        &self.materialized_tree
    }
}

/// A complete tree capture produced through a temporary index. The caller can
/// store the patch bytes in CAS without having to derive them again.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TreeCapture {
    base_tree: GitTreeId,
    tree: GitTreeId,
    changed_paths: Vec<String>,
    binary_patch: Vec<u8>,
    patch_digest: ContentDigest,
}

/// Exact `git diff --check` evidence for one qualified base/candidate pair.
/// A nonzero check result is an observed candidate defect rather than a Git
/// transport failure; bounded diagnostics are retained for higher-level
/// validation evidence.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CandidateWhitespaceCheck {
    base_tree: GitTreeId,
    candidate_tree: GitTreeId,
    clean: bool,
    stdout: Vec<u8>,
    stderr: Vec<u8>,
}

impl CandidateWhitespaceCheck {
    #[must_use]
    pub fn base_tree(&self) -> &GitTreeId {
        &self.base_tree
    }

    #[must_use]
    pub fn candidate_tree(&self) -> &GitTreeId {
        &self.candidate_tree
    }

    #[must_use]
    pub const fn is_clean(&self) -> bool {
        self.clean
    }

    #[must_use]
    pub fn stdout(&self) -> &[u8] {
        &self.stdout
    }

    #[must_use]
    pub fn stderr(&self) -> &[u8] {
        &self.stderr
    }
}

impl TreeCapture {
    #[must_use]
    pub fn base_tree(&self) -> &GitTreeId {
        &self.base_tree
    }

    #[must_use]
    pub fn tree(&self) -> &GitTreeId {
        &self.tree
    }

    #[must_use]
    pub fn changed_paths(&self) -> &[String] {
        &self.changed_paths
    }

    #[must_use]
    pub fn binary_patch(&self) -> &[u8] {
        &self.binary_patch
    }

    #[must_use]
    pub fn patch_digest(&self) -> ContentDigest {
        self.patch_digest
    }
}

/// Identity selected by kernel policy, used for both author and committer.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct GitIdentity {
    name: String,
    email: String,
}

impl GitIdentity {
    pub fn new(name: impl Into<String>, email: impl Into<String>) -> Result<Self, GitCustodyError> {
        let name = name.into();
        let email = email.into();
        if name.is_empty()
            || email.is_empty()
            || name.len() > 240
            || email.len() > 240
            || !email.contains('@')
            || [name.as_str(), email.as_str()]
                .iter()
                .any(|value| value.bytes().any(|byte| matches!(byte, b'\n' | b'\r' | 0)))
        {
            return Err(GitCustodyError::InvalidIdentity);
        }
        Ok(Self { name, email })
    }
}

/// Normalized, bounded Engineering text. Provenance trailers are appended by
/// the kernel and are not actor-controlled text.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CommitMessage {
    subject: String,
    body: String,
}

impl CommitMessage {
    pub fn normalize(
        subject: impl Into<String>,
        body: impl Into<String>,
    ) -> Result<Self, GitCustodyError> {
        let subject = normalize_newlines(&subject.into());
        let mut body = normalize_newlines(&body.into());
        while body.ends_with('\n') {
            body.pop();
        }
        if subject.is_empty()
            || subject.len() > 120
            || subject.contains('\n')
            || subject.contains('\0')
            || body.len() > 8 * 1024
            || body.contains('\0')
        {
            return Err(GitCustodyError::InvalidCommitMessage);
        }
        Ok(Self { subject, body })
    }
}

/// The exact pre-commit evidence that Git trailers must bind.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CommitProvenance {
    pub campaign_id: CampaignId,
    pub ticket_id: TicketId,
    pub ticket_revision_digest: ContentDigest,
    pub kernel_build_id: KernelBuildId,
    pub application_revision_id: ApplicationRevisionId,
    pub regression_tree: GitTreeId,
    pub patch_digest: ContentDigest,
    pub engineering_session_digest: ContentDigest,
    pub validation_id: ValidationId,
}

/// Candidate refs are structurally local and cannot name a remote namespace.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CandidateRefName(String);

impl CandidateRefName {
    #[must_use]
    pub fn new(ticket_id: TicketId, candidate_id: CandidateId) -> Self {
        Self(format!(
            "refs/heads/factory/{}/{}",
            ticket_id.get(),
            candidate_id.get()
        ))
    }

    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// Candidate commit construction input. The exact base is intentionally read
/// only from `QualifiedRepository`, preventing a mixed-base commit.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ConstructCandidateCommit {
    pub candidate_tree: GitTreeId,
    pub candidate_ref: CandidateRefName,
    pub message: CommitMessage,
    pub author: GitIdentity,
    pub committer: GitIdentity,
    pub timestamp_unix_seconds: i64,
    pub provenance: CommitProvenance,
}

/// A one-parent, local-ref-bound kernel commit for a later Architect decision.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CandidateCommit {
    commit: GitCommitId,
    candidate_ref: CandidateRefName,
    base_commit: GitCommitId,
    candidate_tree: GitTreeId,
    ref_was_present: bool,
}

impl CandidateCommit {
    #[must_use]
    pub fn commit(&self) -> &GitCommitId {
        &self.commit
    }

    #[must_use]
    pub fn candidate_ref(&self) -> &CandidateRefName {
        &self.candidate_ref
    }

    #[must_use]
    pub fn base_commit(&self) -> &GitCommitId {
        &self.base_commit
    }

    #[must_use]
    pub fn candidate_tree(&self) -> &GitTreeId {
        &self.candidate_tree
    }

    #[must_use]
    pub fn ref_was_present(&self) -> bool {
        self.ref_was_present
    }
}

/// Exact local delivery receipt; database insertion stays with the caller.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LocalDeliveryReceipt {
    pub previous_commit: GitCommitId,
    pub delivered_commit: GitCommitId,
    pub delivered_tree: GitTreeId,
}

/// The generic kernel's physical Git authority.
#[derive(Debug)]
pub struct GitCustody {
    executable: PathBuf,
    runtime_root: PathBuf,
    worktrees_root: PathBuf,
    indexes_root: PathBuf,
    stream_limit: u64,
    deadline: Duration,
    next_index: AtomicU64,
}

impl GitCustody {
    pub fn new(
        executable: impl AsRef<Path>,
        runtime_root: impl AsRef<Path>,
    ) -> Result<Self, GitCustodyError> {
        Self::with_limits(
            executable,
            runtime_root,
            DEFAULT_STREAM_LIMIT,
            DEFAULT_DEADLINE,
        )
    }

    pub fn with_limits(
        executable: impl AsRef<Path>,
        runtime_root: impl AsRef<Path>,
        stream_limit: u64,
        deadline: Duration,
    ) -> Result<Self, GitCustodyError> {
        if stream_limit == 0 || deadline.is_zero() {
            return Err(GitCustodyError::InvalidExecutionLimits);
        }
        let executable = canonical_file(executable.as_ref(), "Git executable")?;
        let runtime_root = create_directory(runtime_root.as_ref(), "runtime root")?;
        let worktrees_root = create_directory(&runtime_root.join("worktrees"), "worktree root")?;
        let indexes_root =
            create_directory(&runtime_root.join("git-indexes"), "temporary-index root")?;
        Ok(Self {
            executable,
            runtime_root,
            worktrees_root,
            indexes_root,
            stream_limit,
            deadline,
            next_index: AtomicU64::new(1),
        })
    }

    /// Canonical kernel-owned root containing only disposable worktrees and
    /// temporary indexes. The product checkout is deliberately never below it.
    #[must_use]
    pub fn runtime_root(&self) -> &Path {
        &self.runtime_root
    }

    /// Qualifies the configured primary checkout before a claim or delivery.
    pub fn qualify_repository(
        &self,
        root: impl AsRef<Path>,
        branch: DefaultBranchName,
    ) -> Result<QualifiedRepository, GitCustodyError> {
        let root = canonical_directory(root.as_ref(), "repository root")?;
        let observed_root = canonical_directory(
            Path::new(&self.line(
                "resolve repository top level",
                &root,
                &["rev-parse", "--show-toplevel"],
            )?),
            "Git repository top level",
        )?;
        if observed_root != root {
            return Err(GitCustodyError::RepositoryRootMismatch {
                configured: root,
                observed: observed_root,
            });
        }
        self.reject_unsafe_config(&root)?;
        self.reject_replace_refs(&root)?;
        self.reject_submodules(&root)?;
        self.reject_unsafe_attributes(&root)?;
        self.assert_no_operation(&root)?;
        self.assert_clean(&root)?;
        let default_ref = branch.reference();
        let base_commit = self.commit(&root, &default_ref)?;
        let current_branch = self.line(
            "read checkout symbolic HEAD",
            &root,
            &["symbolic-ref", "--quiet", "--short", "HEAD"],
        )?;
        if current_branch != branch.as_str() {
            return Err(GitCustodyError::DefaultBranchNotCheckedOut {
                expected: branch.as_str().to_owned(),
                observed: current_branch,
            });
        }
        let head = self.commit(&root, "HEAD")?;
        if head != base_commit {
            return Err(GitCustodyError::CheckoutHeadDiffersFromDefaultBranch {
                branch: branch.as_str().to_owned(),
                branch_commit: base_commit.to_string(),
                head_commit: head.to_string(),
            });
        }
        let base_tree = self.tree_for_commit(&root, &base_commit)?;
        Ok(QualifiedRepository {
            root,
            branch,
            snapshot: RepositorySnapshot {
                base_tree,
                base_commit,
            },
        })
    }

    /// Requalification is the move fence used immediately before an actor
    /// worktree is allocated and immediately before local delivery.
    pub fn assert_snapshot_current(
        &self,
        repository: &QualifiedRepository,
    ) -> Result<(), GitCustodyError> {
        let current = self.qualify_repository(&repository.root, repository.branch.clone())?;
        if current.snapshot != repository.snapshot {
            return Err(GitCustodyError::RepositoryHeadMoved {
                expected: repository.snapshot.base_commit.to_string(),
                observed: current.snapshot.base_commit.to_string(),
            });
        }
        Ok(())
    }

    /// Allocates a detached base worktree beneath the runtime root.
    pub fn create_detached_worktree(
        &self,
        repository: &QualifiedRepository,
        kind: WorktreeKind,
        name: WorktreeName,
    ) -> Result<OwnedWorktree, GitCustodyError> {
        self.assert_snapshot_current(repository)?;
        let worktree = self.add_no_checkout(repository, kind, name)?;
        let checkout = self.run(
            "checkout detached worktree base",
            &worktree.path,
            &[
                "checkout",
                "--detach",
                "--force",
                worktree.base_commit.as_str(),
            ],
            None,
            None,
        );
        if let Err(error) = checkout.and_then(require_success) {
            self.cleanup_failed_worktree(&worktree.repository_root, &worktree.path);
            return Err(error);
        }
        if let Err(error) = self
            .assert_worktree_head(&worktree)
            .and_then(|_| self.assert_clean(&worktree.path))
        {
            self.cleanup_failed_worktree(&worktree.repository_root, &worktree.path);
            return Err(error);
        }
        Ok(worktree)
    }

    /// Recovers the custody handle for the Engineering workspace allocated by
    /// the daemon before the actor host was launched.  This never accepts an
    /// arbitrary checkout: the path must be one of this runtime's actor
    /// worktrees, point at the qualified repository's common Git directory,
    /// and still be detached at the exact qualified base.  Uncommitted actor
    /// edits are intentionally permitted; later capture owns their tree.
    pub fn adopt_actor_worktree(
        &self,
        repository: &QualifiedRepository,
        path: impl AsRef<Path>,
    ) -> Result<OwnedWorktree, GitCustodyError> {
        self.assert_snapshot_current(repository)?;
        let path = canonical_directory(path.as_ref(), "Engineering actor worktree")?;
        self.assert_owned_worktree_path(&path)?;
        if !path.starts_with(
            self.worktrees_root
                .join(WorktreeKind::Actor.path_component()),
        ) {
            return Err(GitCustodyError::WorktreeOutsideRuntimeRoot);
        }
        let observed_root = canonical_directory(
            Path::new(&self.line(
                "resolve Engineering worktree top level",
                &path,
                &["rev-parse", "--show-toplevel"],
            )?),
            "Engineering actor Git top level",
        )?;
        if observed_root != path {
            return Err(GitCustodyError::RepositoryRootMismatch {
                configured: path,
                observed: observed_root,
            });
        }
        let worktree = OwnedWorktree {
            path,
            repository_root: repository.root.clone(),
            branch: repository.branch.clone(),
            base_commit: repository.snapshot.base_commit.clone(),
            base_tree: repository.snapshot.base_tree.clone(),
            materialized_tree: repository.snapshot.base_tree.clone(),
        };
        self.assert_worktree_head(&worktree)?;
        Ok(worktree)
    }

    /// Materializes an exact tree into a fresh detached workspace without
    /// changing `HEAD` away from the qualified base commit.
    pub fn rematerialize_tree(
        &self,
        repository: &QualifiedRepository,
        tree: GitTreeId,
        kind: WorktreeKind,
        name: WorktreeName,
    ) -> Result<OwnedWorktree, GitCustodyError> {
        self.assert_snapshot_current(repository)?;
        let mut worktree = self.add_no_checkout(repository, kind, name)?;
        let result = (|| {
            require_success(self.run(
                "materialize exact tree",
                &worktree.path,
                &["read-tree", "--reset", "-u", tree.as_str()],
                None,
                None,
            )?)?;
            self.assert_worktree_head(&worktree)?;
            let observed = self.write_tree("verify materialized tree", &worktree.path, None)?;
            if observed != tree {
                return Err(GitCustodyError::MaterializedTreeMismatch {
                    expected: tree.to_string(),
                    observed: observed.to_string(),
                });
            }
            let diff = self.run(
                "verify materialized worktree",
                &worktree.path,
                &["diff", "--quiet", "--no-ext-diff", "--no-textconv", "--"],
                None,
                None,
            )?;
            if !diff.status.success() {
                return Err(GitCustodyError::MaterializedWorktreeDirty {
                    path: worktree.path.clone(),
                });
            }
            Ok(())
        })();
        if let Err(error) = result {
            self.cleanup_failed_worktree(&worktree.repository_root, &worktree.path);
            return Err(error);
        }
        worktree.materialized_tree = tree;
        Ok(worktree)
    }

    /// Captures a regression or candidate tree with a fresh temporary index;
    /// the actor's own index stays untouched.
    pub fn capture_tree(&self, worktree: &OwnedWorktree) -> Result<TreeCapture, GitCustodyError> {
        self.assert_worktree_head(worktree)?;
        let default_commit =
            self.commit(&worktree.repository_root, &worktree.branch.reference())?;
        if default_commit != worktree.base_commit {
            return Err(GitCustodyError::RepositoryHeadMoved {
                expected: worktree.base_commit.to_string(),
                observed: default_commit.to_string(),
            });
        }
        let index = self.temporary_index()?;
        let result = (|| {
            require_success(self.run(
                "read base tree into temporary index",
                &worktree.path,
                &["read-tree", worktree.base_tree.as_str()],
                Some(&index.path),
                None,
            )?)?;
            require_success(self.run(
                "add worktree changes to temporary index",
                &worktree.path,
                &["add", "--all", "--", "."],
                Some(&index.path),
                None,
            )?)?;
            let tree = self.write_tree("write captured tree", &worktree.path, Some(&index.path))?;
            if tree == worktree.base_tree {
                return Err(GitCustodyError::EmptyTreeCapture);
            }
            let changed_paths = self.changed_paths(&worktree.path, &worktree.base_tree, &tree)?;
            let binary_patch = self.binary_patch(&worktree.path, &worktree.base_tree, &tree)?;
            if changed_paths.is_empty() || binary_patch.is_empty() {
                return Err(GitCustodyError::EmptyTreeCapture);
            }
            self.verify_tree_patch(worktree, &tree, &binary_patch)?;
            Ok(TreeCapture {
                base_tree: worktree.base_tree.clone(),
                tree,
                changed_paths,
                patch_digest: ContentDigest::of_bytes(&binary_patch),
                binary_patch,
            })
        })();
        drop(index);
        result
    }

    /// Proves a supplied portable patch reconstructs exactly `candidate_tree`.
    pub fn verify_tree_patch(
        &self,
        worktree: &OwnedWorktree,
        candidate_tree: &GitTreeId,
        patch: &[u8],
    ) -> Result<(), GitCustodyError> {
        if patch.is_empty() {
            return Err(GitCustodyError::TreePatchMismatch {
                expected: candidate_tree.to_string(),
                observed: worktree.base_tree.to_string(),
            });
        }
        let index = self.temporary_index()?;
        let result = (|| {
            require_success(self.run(
                "read patch base tree into temporary index",
                &worktree.repository_root,
                &["read-tree", worktree.base_tree.as_str()],
                Some(&index.path),
                None,
            )?)?;
            let apply = self.run(
                "apply binary tree patch",
                &worktree.repository_root,
                &["apply", "--cached", "--binary", "--whitespace=nowarn"],
                Some(&index.path),
                Some(patch),
            )?;
            if !apply.status.success() {
                return Err(GitCustodyError::TreePatchRejected);
            }
            let observed = self.write_tree(
                "write patch-applied tree",
                &worktree.repository_root,
                Some(&index.path),
            )?;
            if &observed != candidate_tree {
                return Err(GitCustodyError::TreePatchMismatch {
                    expected: candidate_tree.to_string(),
                    observed: observed.to_string(),
                });
            }
            Ok(())
        })();
        drop(index);
        result
    }

    /// Checks candidate whitespace with exact Git argv before candidate
    /// acceptance.  The base comes only from the qualified snapshot, so an
    /// actor cannot choose a comparison point.  This operation only inspects
    /// local objects and has no remote path.
    pub fn check_candidate_whitespace(
        &self,
        repository: &QualifiedRepository,
        candidate_tree: &GitTreeId,
    ) -> Result<CandidateWhitespaceCheck, GitCustodyError> {
        self.assert_snapshot_current(repository)?;
        self.assert_tree(&repository.root, candidate_tree)?;
        let output = self.run(
            "check candidate whitespace",
            &repository.root,
            &[
                "diff",
                "--check",
                "--no-ext-diff",
                "--no-textconv",
                repository.snapshot.base_tree.as_str(),
                candidate_tree.as_str(),
            ],
            None,
            None,
        )?;
        Ok(CandidateWhitespaceCheck {
            base_tree: repository.snapshot.base_tree.clone(),
            candidate_tree: candidate_tree.clone(),
            clean: output.status.success(),
            stdout: output.stdout,
            stderr: output.stderr,
        })
    }

    /// Constructs the deterministic one-parent kernel commit then creates its
    /// local candidate ref with expected-absent compare-and-swap semantics.
    pub fn construct_candidate_commit(
        &self,
        repository: &QualifiedRepository,
        request: &ConstructCandidateCommit,
    ) -> Result<CandidateCommit, GitCustodyError> {
        self.assert_snapshot_current(repository)?;
        self.assert_tree(&repository.root, &request.candidate_tree)?;
        self.assert_tree(&repository.root, &request.provenance.regression_tree)?;
        if request.candidate_tree == repository.snapshot.base_tree {
            return Err(GitCustodyError::CandidateTreeEqualsBase);
        }
        let index = self.temporary_index()?;
        let result = (|| {
            require_success(self.run(
                "read candidate tree into commit index",
                &repository.root,
                &["read-tree", request.candidate_tree.as_str()],
                Some(&index.path),
                None,
            )?)?;
            let observed = self.write_tree(
                "verify candidate commit tree",
                &repository.root,
                Some(&index.path),
            )?;
            if observed != request.candidate_tree {
                return Err(GitCustodyError::CandidateCommitTreeMismatch {
                    expected: request.candidate_tree.to_string(),
                    observed: observed.to_string(),
                });
            }
            let environment = commit_environment(request);
            let message = commit_message(repository, request);
            let output = require_success(self.run_with_environment(
                "construct candidate commit",
                &repository.root,
                &[
                    "commit-tree",
                    request.candidate_tree.as_str(),
                    "-p",
                    repository.snapshot.base_commit.as_str(),
                ],
                Some(&index.path),
                Some(message.as_bytes()),
                &environment,
            )?)?;
            let commit =
                GitCommitId::parse(single_line("constructed candidate commit", &output.stdout)?)?;
            if self.tree_for_commit(&repository.root, &commit)? != request.candidate_tree {
                return Err(GitCustodyError::CandidateCommitTreeMismatch {
                    expected: request.candidate_tree.to_string(),
                    observed: self.tree_for_commit(&repository.root, &commit)?.to_string(),
                });
            }
            let existed = self.bind_candidate_ref(repository, &request.candidate_ref, &commit)?;
            Ok(CandidateCommit {
                commit,
                candidate_ref: request.candidate_ref.clone(),
                base_commit: repository.snapshot.base_commit.clone(),
                candidate_tree: request.candidate_tree.clone(),
                ref_was_present: existed,
            })
        })();
        drop(index);
        result
    }

    /// Recovers the persisted, locally bound candidate commit for delivery.
    /// This is deliberately narrower than construction: all four identities
    /// come from trusted durable state, then the same one-parent/tree/ref
    /// proof used by delivery is repeated before the merge can begin.
    pub fn recover_candidate_commit(
        &self,
        repository: &QualifiedRepository,
        candidate_ref: CandidateRefName,
        commit: GitCommitId,
        candidate_tree: GitTreeId,
    ) -> Result<CandidateCommit, GitCustodyError> {
        self.assert_snapshot_current(repository)?;
        let candidate = CandidateCommit {
            commit,
            candidate_ref,
            base_commit: repository.snapshot.base_commit.clone(),
            candidate_tree,
            ref_was_present: true,
        };
        self.assert_candidate(repository, &candidate)?;
        Ok(candidate)
    }

    /// Builds a candidate commit only when its bounded local ref is absent;
    /// otherwise re-proves the existing ref's exact one-parent/tree identity.
    /// This makes crash recovery between `commit-tree` and durable commit
    /// attachment idempotent without trusting a timestamp reconstruction.
    pub fn construct_or_recover_candidate_commit(
        &self,
        repository: &QualifiedRepository,
        request: &ConstructCandidateCommit,
    ) -> Result<CandidateCommit, GitCustodyError> {
        self.assert_snapshot_current(repository)?;
        if let Some(commit) = self.optional_ref(&repository.root, request.candidate_ref.as_str())? {
            return self.recover_candidate_commit(
                repository,
                request.candidate_ref.clone(),
                commit,
                request.candidate_tree.clone(),
            );
        }
        self.construct_candidate_commit(repository, request)
    }

    /// Performs the only delivery mutation: a local `merge --ff-only` after
    /// every precondition is proved. This module never fetches, pulls, pushes,
    /// or otherwise invokes a remote Git subcommand.
    pub fn guarded_local_fast_forward(
        &self,
        repository: &QualifiedRepository,
        candidate: &CandidateCommit,
    ) -> Result<LocalDeliveryReceipt, GitCustodyError> {
        if candidate.base_commit != repository.snapshot.base_commit {
            return Err(GitCustodyError::DeliveryBaseMismatch);
        }
        self.assert_snapshot_current(repository)?;
        self.assert_only_primary_worktree(repository)?;
        self.assert_candidate(repository, candidate)?;
        let merge = self.run(
            "guarded local fast-forward",
            &repository.root,
            &[
                "merge",
                "--ff-only",
                "--no-edit",
                "--no-stat",
                "--no-verify",
                candidate.commit.as_str(),
            ],
            None,
            None,
        )?;
        if !merge.status.success() {
            return Err(GitCustodyError::LocalFastForwardFailed);
        }
        let delivered = self.qualify_repository(&repository.root, repository.branch.clone())?;
        self.assert_only_primary_worktree(&delivered)?;
        if delivered.snapshot.base_commit != candidate.commit
            || delivered.snapshot.base_tree != candidate.candidate_tree
        {
            return Err(GitCustodyError::DeliveryPostconditionMismatch);
        }
        Ok(LocalDeliveryReceipt {
            previous_commit: repository.snapshot.base_commit.clone(),
            delivered_commit: delivered.snapshot.base_commit,
            delivered_tree: delivered.snapshot.base_tree,
        })
    }

    /// Recovers the physical half of a delivery interrupted after the local
    /// fast-forward but before its durable receipt was recorded. The current
    /// checkout must already be the exact persisted candidate commit/tree;
    /// any other ref movement remains a hard custody failure. No Git mutation
    /// is performed on this path.
    pub fn recover_completed_local_fast_forward(
        &self,
        repository: &QualifiedRepository,
        expected_old_commit: GitCommitId,
        candidate_ref: CandidateRefName,
        candidate_commit: GitCommitId,
        candidate_tree: GitTreeId,
    ) -> Result<LocalDeliveryReceipt, GitCustodyError> {
        self.assert_only_primary_worktree(repository)?;
        if repository.snapshot.base_commit != candidate_commit
            || repository.snapshot.base_tree != candidate_tree
        {
            return Err(GitCustodyError::DeliveryPostconditionMismatch);
        }
        self.assert_candidate(
            repository,
            &CandidateCommit {
                commit: candidate_commit.clone(),
                candidate_ref,
                base_commit: expected_old_commit.clone(),
                candidate_tree: candidate_tree.clone(),
                ref_was_present: true,
            },
        )?;
        Ok(LocalDeliveryReceipt {
            previous_commit: expected_old_commit,
            delivered_commit: candidate_commit,
            delivered_tree: candidate_tree,
        })
    }

    fn add_no_checkout(
        &self,
        repository: &QualifiedRepository,
        kind: WorktreeKind,
        name: WorktreeName,
    ) -> Result<OwnedWorktree, GitCustodyError> {
        let parent = create_directory(
            &self.worktrees_root.join(kind.path_component()),
            "worktree class root",
        )?;
        let path = parent.join(name.as_str());
        if path.exists() {
            return Err(GitCustodyError::WorktreePathAlreadyExists { path });
        }
        self.assert_owned_worktree_path(&path)?;
        let path_text = path.to_str().ok_or(GitCustodyError::NonUtf8Path)?;
        let add = self.run(
            "create detached worktree",
            &repository.root,
            &[
                "worktree",
                "add",
                "--detach",
                "--no-checkout",
                path_text,
                repository.snapshot.base_commit.as_str(),
            ],
            None,
            None,
        )?;
        if !add.status.success() {
            self.cleanup_failed_worktree(&repository.root, &path);
            return Err(GitCustodyError::WorktreeCreationFailed);
        }
        Ok(OwnedWorktree {
            path,
            repository_root: repository.root.clone(),
            branch: repository.branch.clone(),
            base_commit: repository.snapshot.base_commit.clone(),
            base_tree: repository.snapshot.base_tree.clone(),
            materialized_tree: repository.snapshot.base_tree.clone(),
        })
    }

    /// Removes exactly one recorded worktree. No `worktree prune` is used,
    /// so unrelated worktree registrations are never touched.
    pub fn cleanup_worktree(&self, worktree: OwnedWorktree) -> Result<(), GitCustodyError> {
        self.assert_owned_worktree_path(&worktree.path)?;
        let path = worktree.path.to_str().ok_or(GitCustodyError::NonUtf8Path)?;
        require_success(self.run(
            "remove owned worktree",
            &worktree.repository_root,
            &["worktree", "remove", "--force", path],
            None,
            None,
        )?)?;
        if worktree.path.exists() {
            return Err(GitCustodyError::WorktreeCleanupIncomplete);
        }
        let registrations = require_success(self.run(
            "verify owned worktree cleanup",
            &worktree.repository_root,
            &["worktree", "list", "--porcelain"],
            None,
            None,
        )?)?;
        let expected = worktree.path.to_str().ok_or(GitCustodyError::NonUtf8Path)?;
        if registrations
            .stdout
            .split(|byte| *byte == b'\n')
            .filter_map(|line| line.strip_prefix(b"worktree "))
            .any(|registered| registered == expected.as_bytes())
        {
            return Err(GitCustodyError::WorktreeCleanupIncomplete);
        }
        Ok(())
    }

    fn cleanup_failed_worktree(&self, root: &Path, path: &Path) {
        if let Some(path) = path.to_str() {
            let _ = self.run(
                "clean failed worktree",
                root,
                &["worktree", "remove", "--force", path],
                None,
                None,
            );
        }
        if path.is_dir() {
            let _ = fs::remove_dir(path);
        }
    }

    fn assert_worktree_head(&self, worktree: &OwnedWorktree) -> Result<(), GitCustodyError> {
        let symbolic = self.run(
            "prove detached worktree HEAD",
            &worktree.path,
            &["symbolic-ref", "--quiet", "HEAD"],
            None,
            None,
        )?;
        if symbolic.status.success() {
            return Err(GitCustodyError::ActorHeadChanged {
                expected: worktree.base_commit.to_string(),
                observed: single_line("attached worktree HEAD", &symbolic.stdout)?,
            });
        }
        let head = self.commit(&worktree.path, "HEAD")?;
        if head != worktree.base_commit {
            return Err(GitCustodyError::ActorHeadChanged {
                expected: worktree.base_commit.to_string(),
                observed: head.to_string(),
            });
        }
        Ok(())
    }

    fn assert_candidate(
        &self,
        repository: &QualifiedRepository,
        candidate: &CandidateCommit,
    ) -> Result<(), GitCustodyError> {
        let parents = self.line(
            "read candidate parents",
            &repository.root,
            &[
                "rev-list",
                "--parents",
                "-n",
                "1",
                candidate.commit.as_str(),
            ],
        )?;
        let fields: Vec<_> = parents.split_ascii_whitespace().collect();
        if fields.len() != 2
            || fields[0] != candidate.commit.as_str()
            || fields[1] != candidate.base_commit.as_str()
        {
            return Err(GitCustodyError::CandidateIsNotExactOneParent);
        }
        if self.tree_for_commit(&repository.root, &candidate.commit)? != candidate.candidate_tree {
            return Err(GitCustodyError::CandidateCommitTreeMismatch {
                expected: candidate.candidate_tree.to_string(),
                observed: self
                    .tree_for_commit(&repository.root, &candidate.commit)?
                    .to_string(),
            });
        }
        let bound = self.optional_ref(&repository.root, candidate.candidate_ref.as_str())?;
        if bound.as_ref() != Some(&candidate.commit) {
            return Err(GitCustodyError::CandidateRefDoesNotBindCommit);
        }
        Ok(())
    }

    fn bind_candidate_ref(
        &self,
        repository: &QualifiedRepository,
        reference: &CandidateRefName,
        commit: &GitCommitId,
    ) -> Result<bool, GitCustodyError> {
        if let Some(existing) = self.optional_ref(&repository.root, reference.as_str())? {
            return if existing == *commit {
                Ok(true)
            } else {
                Err(GitCustodyError::CandidateRefConflict)
            };
        }
        let zero = "0".repeat(repository.snapshot.base_commit.as_str().len());
        let update = self.run(
            "compare-and-swap candidate ref",
            &repository.root,
            &["update-ref", reference.as_str(), commit.as_str(), &zero],
            None,
            None,
        )?;
        if update.status.success() {
            return Ok(false);
        }
        if self.optional_ref(&repository.root, reference.as_str())? == Some(commit.clone()) {
            Ok(true)
        } else {
            Err(GitCustodyError::CandidateRefConflict)
        }
    }

    fn optional_ref(
        &self,
        root: &Path,
        reference: &str,
    ) -> Result<Option<GitCommitId>, GitCustodyError> {
        let output = self.run(
            "read candidate ref",
            root,
            &["rev-parse", "--verify", "--quiet", reference],
            None,
            None,
        )?;
        if output.status.success() {
            Ok(Some(GitCommitId::parse(single_line(
                "candidate ref",
                &output.stdout,
            )?)?))
        } else if output.status.code() == Some(1) {
            Ok(None)
        } else {
            Err(GitCustodyError::GitCommandFailed {
                operation: "read candidate ref",
            })
        }
    }

    fn assert_only_primary_worktree(
        &self,
        repository: &QualifiedRepository,
    ) -> Result<(), GitCustodyError> {
        let output = require_success(self.run(
            "list delivery worktrees",
            &repository.root,
            &["worktree", "list", "--porcelain"],
            None,
            None,
        )?)?;
        let paths: Vec<String> = output
            .stdout
            .split(|byte| *byte == b'\n')
            .filter_map(|line| line.strip_prefix(b"worktree "))
            .map(|path| String::from_utf8(path.to_vec()).map_err(|_| GitCustodyError::NonUtf8Path))
            .collect::<Result<_, _>>()?;
        if paths.len() != 1
            || canonical_directory(Path::new(&paths[0]), "registered worktree")? != repository.root
        {
            return Err(GitCustodyError::UnexpectedWorktrees { paths });
        }
        Ok(())
    }

    fn assert_tree(&self, root: &Path, tree: &GitTreeId) -> Result<(), GitCustodyError> {
        let expression = format!("{}^{{tree}}", tree.as_str());
        require_success(self.run(
            "verify tree object",
            root,
            &["rev-parse", "--verify", &expression],
            None,
            None,
        )?)?;
        Ok(())
    }

    fn commit(&self, root: &Path, reference: &str) -> Result<GitCommitId, GitCustodyError> {
        let expression = format!("{reference}^{{commit}}");
        let output = require_success(self.run(
            "resolve commit",
            root,
            &["rev-parse", "--verify", &expression],
            None,
            None,
        )?)?;
        GitCommitId::parse(single_line("resolved commit", &output.stdout)?)
    }

    fn tree_for_commit(
        &self,
        root: &Path,
        commit: &GitCommitId,
    ) -> Result<GitTreeId, GitCustodyError> {
        let expression = format!("{}^{{tree}}", commit.as_str());
        let output = require_success(self.run(
            "resolve commit tree",
            root,
            &["rev-parse", "--verify", &expression],
            None,
            None,
        )?)?;
        GitTreeId::parse(single_line("resolved commit tree", &output.stdout)?)
    }

    fn write_tree(
        &self,
        operation: &'static str,
        root: &Path,
        index: Option<&Path>,
    ) -> Result<GitTreeId, GitCustodyError> {
        let output = require_success(self.run(operation, root, &["write-tree"], index, None)?)?;
        GitTreeId::parse(single_line(operation, &output.stdout)?)
    }

    fn changed_paths(
        &self,
        root: &Path,
        base: &GitTreeId,
        candidate: &GitTreeId,
    ) -> Result<Vec<String>, GitCustodyError> {
        let output = require_success(self.run(
            "derive changed paths",
            root,
            &[
                "diff",
                "--no-ext-diff",
                "--no-textconv",
                "--name-only",
                "-z",
                base.as_str(),
                candidate.as_str(),
            ],
            None,
            None,
        )?)?;
        let mut paths = BTreeSet::new();
        for raw in output
            .stdout
            .split(|byte| *byte == 0)
            .filter(|path| !path.is_empty())
        {
            let path = String::from_utf8(raw.to_vec()).map_err(|_| GitCustodyError::NonUtf8Path)?;
            valid_repository_path(&path)?;
            paths.insert(path);
        }
        Ok(paths.into_iter().collect())
    }

    fn binary_patch(
        &self,
        root: &Path,
        base: &GitTreeId,
        candidate: &GitTreeId,
    ) -> Result<Vec<u8>, GitCustodyError> {
        Ok(require_success(self.run(
            "derive binary patch",
            root,
            &[
                "diff",
                "--binary",
                "--full-index",
                "--no-ext-diff",
                "--no-textconv",
                base.as_str(),
                candidate.as_str(),
            ],
            None,
            None,
        )?)?
        .stdout)
    }

    fn reject_unsafe_config(&self, root: &Path) -> Result<(), GitCustodyError> {
        self.reject_unsafe_config_scope(root, "--local")?;
        let worktree_config = self.run(
            "inspect worktree-config extension",
            root,
            &[
                "config",
                "--no-includes",
                "--local",
                "--bool",
                "--get",
                "extensions.worktreeConfig",
            ],
            None,
            None,
        )?;
        match worktree_config.status.code() {
            Some(0) if worktree_config.stdout == b"true\n" => {
                self.reject_unsafe_config_scope(root, "--worktree")?;
            }
            Some(0) if worktree_config.stdout == b"false\n" => {}
            Some(1) => {}
            _ => {
                return Err(GitCustodyError::GitCommandFailed {
                    operation: "inspect worktree-config extension",
                });
            }
        }
        Ok(())
    }

    fn reject_unsafe_config_scope(
        &self,
        root: &Path,
        scope: &'static str,
    ) -> Result<(), GitCustodyError> {
        let output = self.run(
            "inspect repository config",
            root,
            &[
                "config",
                "--list",
                "--no-includes",
                "--null",
                "--name-only",
                scope,
            ],
            None,
            None,
        )?;
        if !output.status.success() {
            return Err(GitCustodyError::GitCommandFailed {
                operation: "inspect repository config",
            });
        }
        for raw in output
            .stdout
            .split(|byte| *byte == 0)
            .filter(|key| !key.is_empty())
        {
            let key = String::from_utf8(raw.to_vec())
                .map_err(|_| GitCustodyError::NonUtf8ConfigurationKey)?;
            if unsafe_config_key(&key) {
                return Err(GitCustodyError::UnsafeRepositoryConfiguration { key });
            }
        }
        Ok(())
    }

    fn reject_replace_refs(&self, root: &Path) -> Result<(), GitCustodyError> {
        let output = require_success(self.run(
            "inspect replacement refs",
            root,
            &["for-each-ref", "--format=%(refname)", "refs/replace"],
            None,
            None,
        )?)?;
        if !output.stdout.is_empty() {
            return Err(GitCustodyError::ReplacementRefsPresent);
        }
        Ok(())
    }

    fn reject_submodules(&self, root: &Path) -> Result<(), GitCustodyError> {
        let output = require_success(self.run(
            "inspect Gitlink entries",
            root,
            &["ls-files", "--stage", "-z"],
            None,
            None,
        )?)?;
        let mut paths = Vec::new();
        for record in output
            .stdout
            .split(|byte| *byte == 0)
            .filter(|record| !record.is_empty())
        {
            if record.starts_with(b"160000 ") {
                let Some(tab) = record.iter().position(|byte| *byte == b'\t') else {
                    return Err(GitCustodyError::MalformedGitIndexRecord);
                };
                paths.push(&record[tab + 1..]);
            }
        }
        if paths.is_empty() {
            return Ok(());
        }
        Err(GitCustodyError::SubmodulesPresent)
    }

    fn reject_unsafe_attributes(&self, root: &Path) -> Result<(), GitCustodyError> {
        let output = require_success(self.run(
            "inspect tracked attributes",
            root,
            &[
                "ls-files",
                "-z",
                "--",
                ".gitattributes",
                ":(glob)**/.gitattributes",
            ],
            None,
            None,
        )?)?;
        for raw in output
            .stdout
            .split(|byte| *byte == 0)
            .filter(|path| !path.is_empty())
        {
            let path = String::from_utf8(raw.to_vec()).map_err(|_| GitCustodyError::NonUtf8Path)?;
            valid_repository_path(&path)?;
            let bytes = fs::read(root.join(&path)).map_err(|source| GitCustodyError::Io {
                operation: "read tracked attributes",
                path: root.join(&path),
                source,
            })?;
            if unsafe_attributes(&bytes) {
                return Err(GitCustodyError::UnsafeGitAttributes { path });
            }
        }
        Ok(())
    }

    fn assert_no_operation(&self, root: &Path) -> Result<(), GitCustodyError> {
        for state in [
            "MERGE_HEAD",
            "CHERRY_PICK_HEAD",
            "REVERT_HEAD",
            "BISECT_LOG",
            "rebase-apply",
            "rebase-merge",
        ] {
            let output = self.line(
                "resolve Git operation state",
                root,
                &["rev-parse", "--git-path", state],
            )?;
            let path = PathBuf::from(output);
            let path = if path.is_absolute() {
                path
            } else {
                root.join(path)
            };
            if path.exists() {
                return Err(GitCustodyError::GitOperationInProgress {
                    state: state.to_owned(),
                });
            }
        }
        Ok(())
    }

    fn assert_clean(&self, root: &Path) -> Result<(), GitCustodyError> {
        let output = require_success(self.run(
            "inspect checkout cleanliness",
            root,
            &[
                "status",
                "--porcelain=v1",
                "--untracked-files=all",
                "--ignore-submodules=none",
            ],
            None,
            None,
        )?)?;
        if output.stdout.is_empty() {
            Ok(())
        } else {
            Err(GitCustodyError::DirtyCheckout)
        }
    }

    fn line(
        &self,
        operation: &'static str,
        root: &Path,
        args: &[&str],
    ) -> Result<String, GitCustodyError> {
        let output = require_success(self.run(operation, root, args, None, None)?)?;
        single_line(operation, &output.stdout)
    }

    fn temporary_index(&self) -> Result<TemporaryIndex, GitCustodyError> {
        for _ in 0..1024 {
            let sequence = self.next_index.fetch_add(1, Ordering::Relaxed);
            let path = self.indexes_root.join(format!("index-{sequence}"));
            if !path.exists() && !path.with_extension("lock").exists() {
                return Ok(TemporaryIndex { path });
            }
        }
        Err(GitCustodyError::TemporaryIndexExhausted)
    }

    fn assert_owned_worktree_path(&self, path: &Path) -> Result<(), GitCustodyError> {
        let relative = path
            .strip_prefix(&self.worktrees_root)
            .map_err(|_| GitCustodyError::WorktreeOutsideRuntimeRoot)?;
        if relative.components().count() != 2
            || relative
                .components()
                .any(|part| !matches!(part, Component::Normal(_)))
        {
            return Err(GitCustodyError::WorktreeOutsideRuntimeRoot);
        }
        Ok(())
    }

    fn run(
        &self,
        operation: &'static str,
        root: &Path,
        args: &[&str],
        index: Option<&Path>,
        stdin: Option<&[u8]>,
    ) -> Result<GitOutput, GitCustodyError> {
        self.run_with_environment(operation, root, args, index, stdin, &[])
    }

    fn run_with_environment(
        &self,
        operation: &'static str,
        root: &Path,
        args: &[&str],
        index: Option<&Path>,
        stdin: Option<&[u8]>,
        environment: &[(&str, String)],
    ) -> Result<GitOutput, GitCustodyError> {
        let mut command = Command::new(&self.executable);
        command.current_dir(root).env_clear();
        command.envs([
            ("HOME", "/nonexistent"),
            ("PATH", "/usr/bin:/bin"),
            ("LANG", "C"),
            ("LC_ALL", "C"),
            ("TZ", "UTC"),
            ("GIT_CONFIG_NOSYSTEM", "1"),
            ("GIT_CONFIG_GLOBAL", "/dev/null"),
            ("GIT_ATTR_NOSYSTEM", "1"),
            ("GIT_OPTIONAL_LOCKS", "0"),
            ("GIT_NO_REPLACE_OBJECTS", "1"),
            ("GIT_TERMINAL_PROMPT", "0"),
        ]);
        if let Some(index) = index {
            command.env("GIT_INDEX_FILE", index);
        }
        command.envs(environment.iter().map(|(key, value)| (*key, value)));
        command.args([
            "--no-pager",
            "--no-replace-objects",
            "-c",
            "core.hooksPath=/dev/null",
            "-c",
            "core.attributesFile=/dev/null",
            "-c",
            "core.autocrlf=false",
            "-c",
            "core.eol=lf",
            "-c",
            "core.fsmonitor=false",
            "-c",
            "diff.external=",
        ]);
        command
            .args(args)
            .stdin(if stdin.is_some() {
                Stdio::piped()
            } else {
                Stdio::null()
            })
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .process_group(0);
        let mut child = command
            .spawn()
            .map_err(|source| GitCustodyError::Spawn { operation, source })?;
        let pid = child.id();
        if let Some(bytes) = stdin {
            let mut pipe = child
                .stdin
                .take()
                .ok_or(GitCustodyError::MissingChildStdin)?;
            if let Err(source) = pipe.write_all(bytes) {
                let _ = signal_process_group(pid, Signal::KILL, operation);
                let _ = child.wait();
                return Err(GitCustodyError::StdinWrite { operation, source });
            }
        }
        let stdout = child
            .stdout
            .take()
            .ok_or(GitCustodyError::MissingChildStream {
                operation,
                stream: "stdout",
            })?;
        let stderr = child
            .stderr
            .take()
            .ok_or(GitCustodyError::MissingChildStream {
                operation,
                stream: "stderr",
            })?;
        let out_limited = Arc::new(AtomicBool::new(false));
        let err_limited = Arc::new(AtomicBool::new(false));
        let out = capture(stdout, self.stream_limit, Arc::clone(&out_limited));
        let err = capture(stderr, self.stream_limit, Arc::clone(&err_limited));
        let started = Instant::now();
        let (status, timed_out, stream_limited) = loop {
            match child
                .try_wait()
                .map_err(|source| GitCustodyError::Wait { operation, source })?
            {
                Some(status) => break (status, false, false),
                None if out_limited.load(Ordering::Acquire)
                    || err_limited.load(Ordering::Acquire) =>
                {
                    let status = terminate_process_group(&mut child, pid, operation)?;
                    break (status, false, true);
                }
                None if started.elapsed() >= self.deadline => {
                    let status = terminate_process_group(&mut child, pid, operation)?;
                    break (status, true, false);
                }
                None => thread::sleep(Duration::from_millis(2)),
            }
        };
        let out = join_capture(out, operation, "stdout")?;
        let err = join_capture(err, operation, "stderr")?;
        if timed_out {
            return Err(GitCustodyError::DeadlineExceeded { operation });
        }
        if stream_limited
            || out_limited.load(Ordering::Acquire)
            || err_limited.load(Ordering::Acquire)
        {
            return Err(GitCustodyError::StreamLimitExceeded { operation });
        }
        Ok(GitOutput {
            status,
            stdout: out,
            stderr: err,
        })
    }
}

struct TemporaryIndex {
    path: PathBuf,
}
impl Drop for TemporaryIndex {
    fn drop(&mut self) {
        let _ = fs::remove_file(&self.path);
        let _ = fs::remove_file(self.path.with_extension("lock"));
    }
}

struct GitOutput {
    status: ExitStatus,
    stdout: Vec<u8>,
    stderr: Vec<u8>,
}

fn capture<R: Read + Send + 'static>(
    mut reader: R,
    limit: u64,
    exceeded: Arc<AtomicBool>,
) -> thread::JoinHandle<Result<Vec<u8>, io::Error>> {
    thread::spawn(move || {
        let mut bytes = Vec::new();
        let mut buffer = [0_u8; 16384];
        loop {
            let count = reader.read(&mut buffer)?;
            if count == 0 {
                break;
            }
            let remaining = limit.saturating_sub(bytes.len() as u64) as usize;
            let kept = count.min(remaining);
            bytes.extend_from_slice(&buffer[..kept]);
            if kept != count {
                exceeded.store(true, Ordering::Release);
            }
        }
        Ok(bytes)
    })
}

fn join_capture(
    handle: thread::JoinHandle<Result<Vec<u8>, io::Error>>,
    operation: &'static str,
    stream: &'static str,
) -> Result<Vec<u8>, GitCustodyError> {
    handle
        .join()
        .map_err(|_| GitCustodyError::CaptureThreadPanicked { operation, stream })?
        .map_err(|source| GitCustodyError::StreamRead {
            operation,
            stream,
            source,
        })
}

fn signal_process_group(
    raw_pid: u32,
    signal: Signal,
    operation: &'static str,
) -> Result<(), GitCustodyError> {
    let raw_pid =
        i32::try_from(raw_pid).map_err(|_| GitCustodyError::InvalidProcessId { pid: raw_pid })?;
    let pid = Pid::from_raw(raw_pid).ok_or(GitCustodyError::InvalidProcessId {
        pid: u32::try_from(raw_pid).unwrap_or_default(),
    })?;
    match kill_process_group(pid, signal) {
        Ok(()) | Err(rustix::io::Errno::SRCH) => Ok(()),
        Err(source) => Err(GitCustodyError::Signal {
            operation,
            signal,
            source,
        }),
    }
}

fn terminate_process_group(
    child: &mut std::process::Child,
    pid: u32,
    operation: &'static str,
) -> Result<ExitStatus, GitCustodyError> {
    signal_process_group(pid, Signal::TERM, operation)?;
    let grace_started = Instant::now();
    loop {
        match child
            .try_wait()
            .map_err(|source| GitCustodyError::Wait { operation, source })?
        {
            Some(status) => return Ok(status),
            None if grace_started.elapsed() >= TERMINATION_GRACE => {
                signal_process_group(pid, Signal::KILL, operation)?;
                return child
                    .wait()
                    .map_err(|source| GitCustodyError::Wait { operation, source });
            }
            None => thread::sleep(Duration::from_millis(2)),
        }
    }
}

fn require_success(output: GitOutput) -> Result<GitOutput, GitCustodyError> {
    if output.status.success() {
        Ok(output)
    } else {
        Err(GitCustodyError::GitCommandFailed {
            operation: "Git plumbing command",
        })
    }
}

fn single_line(operation: &'static str, bytes: &[u8]) -> Result<String, GitCustodyError> {
    let text =
        std::str::from_utf8(bytes).map_err(|_| GitCustodyError::NonUtf8GitOutput { operation })?;
    let text = text.strip_suffix('\n').unwrap_or(text);
    if text.is_empty() || text.contains('\n') || text.contains('\r') {
        Err(GitCustodyError::UnexpectedGitOutput { operation })
    } else {
        Ok(text.to_owned())
    }
}

fn canonical_file(path: &Path, field: &'static str) -> Result<PathBuf, GitCustodyError> {
    let canonical = fs::canonicalize(path).map_err(|source| GitCustodyError::Io {
        operation: "canonicalize path",
        path: path.to_owned(),
        source,
    })?;
    if !fs::metadata(&canonical)
        .map_err(|source| GitCustodyError::Io {
            operation: "inspect path",
            path: canonical.clone(),
            source,
        })?
        .is_file()
    {
        return Err(GitCustodyError::NotRegularFile {
            field,
            path: canonical,
        });
    }
    Ok(canonical)
}

fn canonical_directory(path: &Path, field: &'static str) -> Result<PathBuf, GitCustodyError> {
    let canonical = fs::canonicalize(path).map_err(|source| GitCustodyError::Io {
        operation: "canonicalize directory",
        path: path.to_owned(),
        source,
    })?;
    if !fs::metadata(&canonical)
        .map_err(|source| GitCustodyError::Io {
            operation: "inspect directory",
            path: canonical.clone(),
            source,
        })?
        .is_dir()
    {
        return Err(GitCustodyError::NotDirectory {
            field,
            path: canonical,
        });
    }
    Ok(canonical)
}

fn create_directory(path: &Path, field: &'static str) -> Result<PathBuf, GitCustodyError> {
    fs::create_dir_all(path).map_err(|source| GitCustodyError::Io {
        operation: "create directory",
        path: path.to_owned(),
        source,
    })?;
    canonical_directory(path, field)
}

fn valid_repository_path(path: &str) -> Result<(), GitCustodyError> {
    if path.is_empty()
        || path.starts_with('/')
        || path.contains('\\')
        || path
            .split('/')
            .any(|part| part.is_empty() || matches!(part, "." | ".."))
    {
        Err(GitCustodyError::UnsafeChangedPath {
            path: path.to_owned(),
        })
    } else {
        Ok(())
    }
}

fn unsafe_config_key(key: &str) -> bool {
    let key = key.to_ascii_lowercase();
    key == "include.path"
        || key.starts_with("includeif.")
        || key == "core.hookspath"
        || key == "core.attributesfile"
        || key == "core.autocrlf"
        || key == "core.eol"
        || key == "core.fsmonitor"
        || key == "diff.external"
        || key.starts_with("filter.")
        || (key.starts_with("diff.") && key.ends_with(".command"))
}

fn unsafe_attributes(bytes: &[u8]) -> bool {
    String::from_utf8_lossy(bytes).lines().any(|line| {
        line.split('#')
            .next()
            .unwrap_or_default()
            .split_ascii_whitespace()
            .skip(1)
            .any(|attribute| {
                attribute == "ident"
                    || attribute.starts_with("filter=")
                    || attribute.starts_with("working-tree-encoding=")
            })
    })
}

fn normalize_newlines(value: &str) -> String {
    value.replace("\r\n", "\n").replace('\r', "\n")
}

fn commit_environment(request: &ConstructCandidateCommit) -> Vec<(&'static str, String)> {
    let timestamp = format!("{} +0000", request.timestamp_unix_seconds);
    vec![
        ("GIT_AUTHOR_NAME", request.author.name.clone()),
        ("GIT_AUTHOR_EMAIL", request.author.email.clone()),
        ("GIT_AUTHOR_DATE", timestamp.clone()),
        ("GIT_COMMITTER_NAME", request.committer.name.clone()),
        ("GIT_COMMITTER_EMAIL", request.committer.email.clone()),
        ("GIT_COMMITTER_DATE", timestamp),
    ]
}

fn commit_message(repository: &QualifiedRepository, request: &ConstructCandidateCommit) -> String {
    let provenance = &request.provenance;
    let body = if request.message.body.is_empty() {
        String::new()
    } else {
        format!("{}\n\n", request.message.body)
    };
    format!(
        "{}\n\n{body}Factory-Campaign: {}\nFactory-Ticket: {}\nFactory-Ticket-Revision-Digest: {}\nFactory-Kernel-Build: {}\nFactory-Application-Revision: {}\nFactory-Base: {}\nFactory-Regression-Tree: {}\nFactory-Candidate-Tree: {}\nFactory-Patch-BLAKE3: {}\nFactory-Engineering-Session-BLAKE3: {}\nFactory-Validation: {}\n",
        request.message.subject,
        provenance.campaign_id,
        provenance.ticket_id,
        provenance.ticket_revision_digest,
        provenance.kernel_build_id,
        provenance.application_revision_id,
        repository.snapshot.base_commit,
        provenance.regression_tree,
        request.candidate_tree,
        provenance.patch_digest,
        provenance.engineering_session_digest,
        provenance.validation_id
    )
}

/// Exact physical-boundary rejections, never a generic policy failure.
#[derive(Debug, Error)]
pub enum GitCustodyError {
    #[error("Git stream limit and deadline must be non-zero")]
    InvalidExecutionLimits,
    #[error("default branch is not a safe Git branch component")]
    InvalidDefaultBranch,
    #[error("worktree name is not safe lowercase ASCII")]
    InvalidWorktreeName,
    #[error("Git object ID is not 40 or 64 lowercase hexadecimal bytes: {value}")]
    InvalidObjectId { value: String },
    #[error("Git identity is invalid")]
    InvalidIdentity,
    #[error("commit message is invalid")]
    InvalidCommitMessage,
    #[error("{field} is not a regular file at {path:?}")]
    NotRegularFile { field: &'static str, path: PathBuf },
    #[error("{field} is not a directory at {path:?}")]
    NotDirectory { field: &'static str, path: PathBuf },
    #[error("configured root {configured:?} differs from Git root {observed:?}")]
    RepositoryRootMismatch {
        configured: PathBuf,
        observed: PathBuf,
    },
    #[error("default branch {expected} is not checked out; observed {observed}")]
    DefaultBranchNotCheckedOut { expected: String, observed: String },
    #[error("checkout HEAD {head_commit} differs from {branch} at {branch_commit}")]
    CheckoutHeadDiffersFromDefaultBranch {
        branch: String,
        branch_commit: String,
        head_commit: String,
    },
    #[error("repository head moved from {expected} to {observed}")]
    RepositoryHeadMoved { expected: String, observed: String },
    #[error("checkout is dirty")]
    DirtyCheckout,
    #[error("Git operation {state} is in progress")]
    GitOperationInProgress { state: String },
    #[error("unsafe repository configuration key {key}")]
    UnsafeRepositoryConfiguration { key: String },
    #[error("unsafe tracked Git attributes in {path}")]
    UnsafeGitAttributes { path: String },
    #[error("replacement refs are present")]
    ReplacementRefsPresent,
    #[error("submodules are present")]
    SubmodulesPresent,
    #[error("Git index record is malformed")]
    MalformedGitIndexRecord,
    #[error("worktree path already exists: {path:?}")]
    WorktreePathAlreadyExists { path: PathBuf },
    #[error("worktree is outside the owned runtime root")]
    WorktreeOutsideRuntimeRoot,
    #[error("could not create detached owned worktree")]
    WorktreeCreationFailed,
    #[error("actor worktree HEAD changed from {expected} to {observed}")]
    ActorHeadChanged { expected: String, observed: String },
    #[error("worktree cleanup left files behind")]
    WorktreeCleanupIncomplete,
    #[error("exact-tree materialization wrote {observed}, expected {expected}")]
    MaterializedTreeMismatch { expected: String, observed: String },
    #[error("exact-tree materialization left its worktree dirty at {path:?}")]
    MaterializedWorktreeDirty { path: PathBuf },
    #[error("captured tree is empty")]
    EmptyTreeCapture,
    #[error("changed path is unsafe: {path}")]
    UnsafeChangedPath { path: String },
    #[error("binary patch was rejected")]
    TreePatchRejected,
    #[error("binary patch reconstructed {observed}, expected {expected}")]
    TreePatchMismatch { expected: String, observed: String },
    #[error("candidate tree equals base")]
    CandidateTreeEqualsBase,
    #[error("candidate commit wrote {observed}, expected {expected}")]
    CandidateCommitTreeMismatch { expected: String, observed: String },
    #[error("candidate ref compare-and-swap conflicts")]
    CandidateRefConflict,
    #[error("candidate ref no longer names candidate commit")]
    CandidateRefDoesNotBindCommit,
    #[error("candidate is not a one-parent child of the qualified base")]
    CandidateIsNotExactOneParent,
    #[error("candidate base differs from delivery base")]
    DeliveryBaseMismatch,
    #[error("guarded local fast-forward failed")]
    LocalFastForwardFailed,
    #[error("delivery postcondition differs from candidate")]
    DeliveryPostconditionMismatch,
    #[error("delivery has unexpected worktrees: {paths:?}")]
    UnexpectedWorktrees { paths: Vec<String> },
    #[error("temporary index names are exhausted")]
    TemporaryIndexExhausted,
    #[error("could not spawn Git for {operation}: {source}")]
    Spawn {
        operation: &'static str,
        #[source]
        source: io::Error,
    },
    #[error("Git child has no stdin")]
    MissingChildStdin,
    #[error("Git child has no {stream} for {operation}")]
    MissingChildStream {
        operation: &'static str,
        stream: &'static str,
    },
    #[error("could not write Git stdin for {operation}: {source}")]
    StdinWrite {
        operation: &'static str,
        #[source]
        source: io::Error,
    },
    #[error("could not wait for Git {operation}: {source}")]
    Wait {
        operation: &'static str,
        #[source]
        source: io::Error,
    },
    #[error("could not kill Git {operation}: {source}")]
    Kill {
        operation: &'static str,
        #[source]
        source: io::Error,
    },
    #[error("Git child PID {pid} cannot be represented for process-group custody")]
    InvalidProcessId { pid: u32 },
    #[error("could not send {signal:?} to Git process group for {operation}: {source}")]
    Signal {
        operation: &'static str,
        signal: Signal,
        source: rustix::io::Errno,
    },
    #[error("Git {operation} exceeded its bound")]
    StreamLimitExceeded { operation: &'static str },
    #[error("Git {operation} exceeded its deadline")]
    DeadlineExceeded { operation: &'static str },
    #[error("Git capture thread panicked for {operation} {stream}")]
    CaptureThreadPanicked {
        operation: &'static str,
        stream: &'static str,
    },
    #[error("could not read Git {stream} for {operation}: {source}")]
    StreamRead {
        operation: &'static str,
        stream: &'static str,
        #[source]
        source: io::Error,
    },
    #[error("Git command failed at {operation}")]
    GitCommandFailed { operation: &'static str },
    #[error("Git {operation} output is not UTF-8")]
    NonUtf8GitOutput { operation: &'static str },
    #[error("Git {operation} output is not one line")]
    UnexpectedGitOutput { operation: &'static str },
    #[error("Git path is not UTF-8")]
    NonUtf8Path,
    #[error("Git config key is not UTF-8")]
    NonUtf8ConfigurationKey,
    #[error("I/O while {operation} at {path:?}: {source}")]
    Io {
        operation: &'static str,
        path: PathBuf,
        #[source]
        source: io::Error,
    },
}
