//! Provider-free Git custody judges using only synthetic local repositories.

use std::{
    fs,
    path::{Path, PathBuf},
    process::{Command, Stdio},
    sync::atomic::{AtomicU64, Ordering},
};

use factory_kernel::git::{
    CandidateRefName, CommitMessage, CommitProvenance, ConstructCandidateCommit, DefaultBranchName,
    GitCustody, GitCustodyError, GitIdentity, GitTreeId, WorktreeKind, WorktreeName,
};
use factory_protocol::{
    ApplicationRevisionId, CampaignId, CandidateId, ContentDigest, KernelBuildId, TicketId,
    ValidationId,
};

static NEXT_ROOT: AtomicU64 = AtomicU64::new(1);

struct Fixture {
    root: PathBuf,
    repository: PathBuf,
    git: PathBuf,
    custody: GitCustody,
}

impl Fixture {
    fn new() -> Self {
        let suffix = NEXT_ROOT.fetch_add(1, Ordering::Relaxed);
        let root = std::env::temp_dir().join(format!(
            "factory-kernel-git-custody-{}-{suffix}",
            std::process::id()
        ));
        fs::create_dir(&root).expect("unique test root");
        let repository = root.join("product");
        let git = system_git();
        run_in(&root, &git, &["init", "--initial-branch=main", "product"]);
        run_in(
            &repository,
            &git,
            &["config", "user.name", "Synthetic Tester"],
        );
        run_in(
            &repository,
            &git,
            &["config", "user.email", "synthetic@example.test"],
        );
        fs::write(repository.join("README.md"), b"base\n").expect("base file");
        fs::write(repository.join("target.txt"), b"target\n").expect("target file");
        run_in(&repository, &git, &["add", "--all"]);
        run_in(&repository, &git, &["commit", "-m", "base"]);
        let custody = GitCustody::new(&git, root.join("runtime")).expect("Git custody");
        Self {
            root,
            repository,
            git,
            custody,
        }
    }

    fn qualify(&self) -> factory_kernel::git::QualifiedRepository {
        self.custody
            .qualify_repository(&self.repository, DefaultBranchName::parse("main").unwrap())
            .expect("qualify repository")
    }

    fn commit(&self, message: &str) {
        fs::write(self.repository.join("README.md"), format!("{message}\n")).unwrap();
        run_in(&self.repository, &self.git, &["add", "README.md"]);
        run_in(&self.repository, &self.git, &["commit", "-m", message]);
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.root);
    }
}

#[test]
fn qualification_rejects_dirty_and_moved_primary_head() {
    let fixture = Fixture::new();
    fs::write(fixture.repository.join("untracked"), b"dirty").unwrap();
    assert!(matches!(
        fixture.custody.qualify_repository(
            &fixture.repository,
            DefaultBranchName::parse("main").unwrap()
        ),
        Err(GitCustodyError::DirtyCheckout)
    ));
    fs::remove_file(fixture.repository.join("untracked")).unwrap();

    let qualified = fixture.qualify();
    fixture.commit("head moved");
    assert!(matches!(
        fixture.custody.assert_snapshot_current(&qualified),
        Err(GitCustodyError::RepositoryHeadMoved { .. })
    ));
}

#[test]
fn qualification_rejects_includes_hooks_filters_replace_refs_and_submodules() {
    let fixture = Fixture::new();
    for (key, value) in [
        ("include.path", "/definitely/not/a/factory/config"),
        ("core.hooksPath", "/tmp/unsafe-hooks"),
        ("filter.synthetic.clean", "cat"),
    ] {
        run_in(&fixture.repository, &fixture.git, &["config", key, value]);
        assert!(matches!(
            fixture.custody.qualify_repository(
                &fixture.repository,
                DefaultBranchName::parse("main").unwrap()
            ),
            Err(GitCustodyError::UnsafeRepositoryConfiguration { .. })
        ));
        run_in(
            &fixture.repository,
            &fixture.git,
            &["config", "--unset-all", key],
        );
    }

    let base = git_stdout(&fixture.repository, &fixture.git, &["rev-parse", "HEAD"]);
    fixture.commit("replacement target");
    run_in(
        &fixture.repository,
        &fixture.git,
        &["replace", base.trim(), "HEAD"],
    );
    assert!(matches!(
        fixture.custody.qualify_repository(
            &fixture.repository,
            DefaultBranchName::parse("main").unwrap()
        ),
        Err(GitCustodyError::ReplacementRefsPresent)
    ));
    run_in(
        &fixture.repository,
        &fixture.git,
        &["replace", "-d", base.trim()],
    );

    let target = git_stdout(&fixture.repository, &fixture.git, &["rev-parse", "HEAD"]);
    run_in(
        &fixture.repository,
        &fixture.git,
        &[
            "update-index",
            "--add",
            "--cacheinfo",
            &format!("160000,{},nested", target.trim()),
        ],
    );
    run_in(
        &fixture.repository,
        &fixture.git,
        &["commit", "-m", "gitlink"],
    );
    assert!(matches!(
        fixture.custody.qualify_repository(
            &fixture.repository,
            DefaultBranchName::parse("main").unwrap()
        ),
        Err(GitCustodyError::SubmodulesPresent)
    ));
}

#[test]
fn qualification_rejects_tracked_filter_attributes_before_any_worktree_exists() {
    let fixture = Fixture::new();
    fs::write(
        fixture.repository.join(".gitattributes"),
        b"*.generated filter=synthetic\n",
    )
    .unwrap();
    run_in(
        &fixture.repository,
        &fixture.git,
        &["add", ".gitattributes"],
    );
    run_in(
        &fixture.repository,
        &fixture.git,
        &["commit", "-m", "unsafe attributes"],
    );
    assert!(matches!(
        fixture.custody.qualify_repository(
            &fixture.repository,
            DefaultBranchName::parse("main").unwrap()
        ),
        Err(GitCustodyError::UnsafeGitAttributes { .. })
    ));
    assert!(
        !fixture
            .root
            .join("runtime/worktrees")
            .join("actor")
            .exists(),
        "qualification must fail before allocating an actor worktree"
    );
}

#[test]
fn detached_owned_worktree_rejects_actor_head_change_and_cleans_exactly() {
    let fixture = Fixture::new();
    let repository = fixture.qualify();
    let worktree = fixture
        .custody
        .create_detached_worktree(
            &repository,
            WorktreeKind::Actor,
            WorktreeName::parse("actor-head").unwrap(),
        )
        .unwrap();
    let path = worktree.path().to_owned();
    run_in(&path, &fixture.git, &["checkout", "-b", "actor-moved-head"]);
    assert!(matches!(
        fixture.custody.capture_tree(&worktree),
        Err(GitCustodyError::ActorHeadChanged { .. })
    ));
    fixture.custody.cleanup_worktree(worktree).unwrap();
    assert!(!path.exists());
    assert!(
        !git_stdout(
            &fixture.repository,
            &fixture.git,
            &["worktree", "list", "--porcelain"]
        )
        .contains(path.to_str().unwrap())
    );
}

#[test]
fn temporary_index_capture_preserves_binary_symlink_and_exact_tree() {
    let fixture = Fixture::new();
    let repository = fixture.qualify();
    let actor = fixture
        .custody
        .create_detached_worktree(
            &repository,
            WorktreeKind::Actor,
            WorktreeName::parse("binary-symlink").unwrap(),
        )
        .unwrap();
    fs::write(
        actor.path().join("binary.bin"),
        (0_u8..=255).cycle().take(4096).collect::<Vec<_>>(),
    )
    .unwrap();
    #[cfg(unix)]
    std::os::unix::fs::symlink("target.txt", actor.path().join("linked-target")).unwrap();

    let capture = fixture.custody.capture_tree(&actor).unwrap();
    assert!(capture.changed_paths().contains(&"binary.bin".to_owned()));
    #[cfg(unix)]
    assert!(
        capture
            .changed_paths()
            .contains(&"linked-target".to_owned())
    );
    assert_eq!(
        capture.patch_digest(),
        ContentDigest::of_bytes(capture.binary_patch())
    );
    fixture
        .custody
        .verify_tree_patch(&actor, capture.tree(), capture.binary_patch())
        .unwrap();

    let exact = fixture
        .custody
        .rematerialize_tree(
            &repository,
            capture.tree().clone(),
            WorktreeKind::Validation,
            WorktreeName::parse("materialized-capture").unwrap(),
        )
        .unwrap();
    assert_eq!(
        fs::read(exact.path().join("binary.bin")).unwrap().len(),
        4096
    );
    #[cfg(unix)]
    assert_eq!(
        fs::read_link(exact.path().join("linked-target")).unwrap(),
        PathBuf::from("target.txt")
    );
    fixture.custody.cleanup_worktree(exact).unwrap();
    fixture.custody.cleanup_worktree(actor).unwrap();
}

#[test]
fn empty_tree_and_tree_patch_mismatch_fail_closed() {
    let fixture = Fixture::new();
    let repository = fixture.qualify();
    let actor = fixture
        .custody
        .create_detached_worktree(
            &repository,
            WorktreeKind::Actor,
            WorktreeName::parse("empty-tree").unwrap(),
        )
        .unwrap();
    assert!(matches!(
        fixture.custody.capture_tree(&actor),
        Err(GitCustodyError::EmptyTreeCapture)
    ));
    fs::write(actor.path().join("changed"), b"changed\n").unwrap();
    let capture = fixture.custody.capture_tree(&actor).unwrap();
    assert!(matches!(
        fixture
            .custody
            .verify_tree_patch(&actor, capture.tree(), b""),
        Err(GitCustodyError::TreePatchMismatch { .. })
    ));
    fixture.custody.cleanup_worktree(actor).unwrap();
}

#[test]
fn materialization_checkout_failure_cleans_owned_path() {
    let fixture = Fixture::new();
    let repository = fixture.qualify();
    let blob = git_stdout_with_input(
        &fixture.repository,
        &fixture.git,
        &["hash-object", "-w", "--stdin"],
        b"this is a blob, not a tree",
    );
    let expected_path = fixture.root.join("runtime/worktrees/validation/bad-tree");
    assert!(
        fixture
            .custody
            .rematerialize_tree(
                &repository,
                GitTreeId::parse(blob.trim()).unwrap(),
                WorktreeKind::Validation,
                WorktreeName::parse("bad-tree").unwrap(),
            )
            .is_err()
    );
    assert!(
        !expected_path.exists(),
        "failed materialization left a worktree"
    );
}

#[test]
fn kernel_commit_is_idempotent_and_binds_every_provenance_input() {
    let fixture = Fixture::new();
    let repository = fixture.qualify();
    let actor = fixture
        .custody
        .create_detached_worktree(
            &repository,
            WorktreeKind::Actor,
            WorktreeName::parse("candidate-commit").unwrap(),
        )
        .unwrap();
    fs::write(actor.path().join("fix.txt"), b"fixed\n").unwrap();
    let capture = fixture.custody.capture_tree(&actor).unwrap();
    fixture.custody.cleanup_worktree(actor).unwrap();
    let request = commit_request(&capture, 1);
    let first = fixture
        .custody
        .construct_candidate_commit(&repository, &request)
        .unwrap();
    assert!(!first.ref_was_present());
    let second = fixture
        .custody
        .construct_candidate_commit(&repository, &request)
        .unwrap();
    assert!(second.ref_was_present());
    assert_eq!(first.commit(), second.commit());
    let message = git_stdout(
        &fixture.repository,
        &fixture.git,
        &["show", "-s", "--format=%B", first.commit().as_str()],
    );
    for trailer in [
        "Factory-Campaign: 1",
        "Factory-Ticket: 2",
        "Factory-Ticket-Revision-Digest:",
        "Factory-Kernel-Build:",
        "Factory-Application-Revision: 3",
        "Factory-Base:",
        "Factory-Regression-Tree:",
        "Factory-Candidate-Tree:",
        "Factory-Patch-BLAKE3:",
        "Factory-Engineering-Session-BLAKE3:",
        "Factory-Validation: 4",
    ] {
        assert!(message.contains(trailer), "missing {trailer}");
    }
}

#[test]
fn candidate_ref_compare_and_swap_conflict_is_not_overwritten() {
    let fixture = Fixture::new();
    let repository = fixture.qualify();
    let actor = fixture
        .custody
        .create_detached_worktree(
            &repository,
            WorktreeKind::Actor,
            WorktreeName::parse("ref-cas").unwrap(),
        )
        .unwrap();
    fs::write(actor.path().join("candidate"), b"candidate\n").unwrap();
    let capture = fixture.custody.capture_tree(&actor).unwrap();
    fixture.custody.cleanup_worktree(actor).unwrap();
    let request = commit_request(&capture, 2);
    run_in(
        &fixture.repository,
        &fixture.git,
        &[
            "update-ref",
            request.candidate_ref.as_str(),
            repository.snapshot().base_commit().as_str(),
        ],
    );
    assert!(matches!(
        fixture
            .custody
            .construct_candidate_commit(&repository, &request),
        Err(GitCustodyError::CandidateRefConflict)
    ));
    assert_eq!(
        git_stdout(
            &fixture.repository,
            &fixture.git,
            &["rev-parse", request.candidate_ref.as_str()],
        )
        .trim(),
        repository.snapshot().base_commit().as_str()
    );
}

#[test]
fn guarded_fast_forward_is_local_and_refuses_moved_base() {
    let fixture = Fixture::new();
    let repository = fixture.qualify();
    let actor = fixture
        .custody
        .create_detached_worktree(
            &repository,
            WorktreeKind::Actor,
            WorktreeName::parse("deliver").unwrap(),
        )
        .unwrap();
    fs::write(actor.path().join("delivery.txt"), b"deliver\n").unwrap();
    let capture = fixture.custody.capture_tree(&actor).unwrap();
    fixture.custody.cleanup_worktree(actor).unwrap();
    let candidate = fixture
        .custody
        .construct_candidate_commit(&repository, &commit_request(&capture, 3))
        .unwrap();
    let receipt = fixture
        .custody
        .guarded_local_fast_forward(&repository, &candidate)
        .unwrap();
    assert_eq!(receipt.delivered_commit, *candidate.commit());
    assert_eq!(
        git_stdout(&fixture.repository, &fixture.git, &["rev-parse", "HEAD"]).trim(),
        candidate.commit().as_str()
    );

    let moved_fixture = Fixture::new();
    let moved_repository = moved_fixture.qualify();
    moved_fixture.commit("external main move");
    assert!(matches!(
        moved_fixture
            .custody
            .assert_snapshot_current(&moved_repository),
        Err(GitCustodyError::RepositoryHeadMoved { .. })
    ));
}

#[cfg(unix)]
#[test]
fn custody_flow_has_no_remote_git_operation_path() {
    use std::os::unix::fs::PermissionsExt;

    let fixture = Fixture::new();
    let log = fixture.root.join("git-argv.log");
    let wrapper = fixture.root.join("logged-git");
    fs::write(
        &wrapper,
        format!(
            "#!/bin/sh\nprintf '%s\\n' \"$@\" >> '{}'\nexec '{}' \"$@\"\n",
            log.display(),
            fixture.git.display()
        ),
    )
    .unwrap();
    fs::set_permissions(&wrapper, fs::Permissions::from_mode(0o755)).unwrap();
    let custody = GitCustody::new(&wrapper, fixture.root.join("logged-runtime")).unwrap();
    let repository = custody
        .qualify_repository(
            &fixture.repository,
            DefaultBranchName::parse("main").unwrap(),
        )
        .unwrap();
    let actor = custody
        .create_detached_worktree(
            &repository,
            WorktreeKind::Actor,
            WorktreeName::parse("no-remote").unwrap(),
        )
        .unwrap();
    fs::write(actor.path().join("no-remote.txt"), b"local\n").unwrap();
    let capture = custody.capture_tree(&actor).unwrap();
    custody.cleanup_worktree(actor).unwrap();
    let candidate = custody
        .construct_candidate_commit(&repository, &commit_request(&capture, 4))
        .unwrap();
    custody
        .guarded_local_fast_forward(&repository, &candidate)
        .unwrap();
    let argv = fs::read_to_string(log).unwrap();
    for remote_operation in ["push", "fetch", "pull", "remote", "ls-remote", "clone"] {
        assert!(
            !argv.lines().any(|line| line == remote_operation),
            "remote operation reached Git executable: {remote_operation}\n{argv}"
        );
    }
}

fn commit_request(
    capture: &factory_kernel::git::TreeCapture,
    candidate: i64,
) -> ConstructCandidateCommit {
    ConstructCandidateCommit {
        candidate_tree: capture.tree().clone(),
        candidate_ref: CandidateRefName::new(
            TicketId::new(2).unwrap(),
            CandidateId::new(candidate).unwrap(),
        ),
        message: CommitMessage::normalize(
            "Fix synthetic behavior",
            "Regression captured by kernel",
        )
        .unwrap(),
        author: GitIdentity::new("Factory Kernel", "factory@example.test").unwrap(),
        committer: GitIdentity::new("Factory Kernel", "factory@example.test").unwrap(),
        timestamp_unix_seconds: 1_700_000_000,
        provenance: CommitProvenance {
            campaign_id: CampaignId::new(1).unwrap(),
            ticket_id: TicketId::new(2).unwrap(),
            ticket_revision_digest: ContentDigest::of_bytes(b"ticket revision"),
            kernel_build_id: KernelBuildId::new(ContentDigest::of_bytes(b"kernel build")),
            application_revision_id: ApplicationRevisionId::new(3).unwrap(),
            regression_tree: capture.tree().clone(),
            patch_digest: capture.patch_digest(),
            engineering_session_digest: ContentDigest::of_bytes(b"engineering transcript"),
            validation_id: ValidationId::new(4).unwrap(),
        },
    }
}

fn system_git() -> PathBuf {
    for candidate in ["/usr/bin/git", "/usr/local/bin/git"] {
        if let Ok(path) = fs::canonicalize(candidate) {
            return path;
        }
    }
    panic!("synthetic Git tests require Git at /usr/bin/git or /usr/local/bin/git");
}

fn run_in(directory: &Path, git: &Path, args: &[&str]) {
    let output = Command::new(git)
        .current_dir(directory)
        .args(args)
        .output()
        .unwrap_or_else(|error| panic!("run Git {args:?}: {error}"));
    assert!(
        output.status.success(),
        "Git {args:?} failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
}

fn git_stdout(directory: &Path, git: &Path, args: &[&str]) -> String {
    let output = Command::new(git)
        .current_dir(directory)
        .args(args)
        .output()
        .unwrap_or_else(|error| panic!("run Git {args:?}: {error}"));
    assert!(
        output.status.success(),
        "Git {args:?} failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8(output.stdout).unwrap()
}

fn git_stdout_with_input(directory: &Path, git: &Path, args: &[&str], input: &[u8]) -> String {
    let mut child = Command::new(git)
        .current_dir(directory)
        .args(args)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .spawn()
        .unwrap();
    use std::io::Write as _;
    child.stdin.take().unwrap().write_all(input).unwrap();
    let output = child.wait_with_output().unwrap();
    assert!(output.status.success());
    String::from_utf8(output.stdout).unwrap()
}
