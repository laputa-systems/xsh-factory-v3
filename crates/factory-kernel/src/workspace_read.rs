//! Daemon-owned exact workspace reads and required-read evidence.
//!
//! An actor can request a repository-relative path, but it cannot report that
//! a read happened. Only this authority opens the pinned workspace file,
//! returns its exact bytes, and records the resulting BLAKE3 observation in a
//! session-bound in-memory ledger. The ledger is sealed once at terminal; no
//! read or tool-call row is written to PostgreSQL.

use std::{
    collections::BTreeMap,
    fs::{self, File, OpenOptions},
    io::{self, Read as _, Write as _},
    os::unix::fs::MetadataExt as _,
    path::{Path, PathBuf},
};

use factory_protocol::{
    ContentDigest, OP_WORKSPACE_READ, PROTOCOL_VERSION_V1, RESPONSE_FRAME_MAX_BYTES,
    ReadExactFileV1, ReadObservationV1, RepositoryRelativePath, WorkspaceReadRequest,
    WorkspaceReadResponse, decode_operation_request,
};
use miniserde::{Serialize, json};
use thiserror::Error;

use crate::{
    cas::{CasArtifact, CasStore},
    local_transport::{ActorConnectionBinding, BoundActorFrame},
};

/// The read response must fit in a 4 MiB frame after base64 and JSON overhead.
pub const WORKSPACE_READ_MAX_BYTES: u64 = 2 * 1024 * 1024;

#[derive(Debug)]
pub struct WorkspaceReadAuthority {
    binding: ActorConnectionBinding,
    workspace_root: PathBuf,
    expected_manifest_artifact_id: factory_protocol::ArtifactId,
    required: Vec<ReadExactFileV1>,
    observed: BTreeMap<RepositoryRelativePath, ContentDigest>,
}

impl WorkspaceReadAuthority {
    /// Only daemon assignment admission can construct a read authority because
    /// only it possesses the non-forgeable actor connection binding.
    pub(crate) fn from_admitted_assignment(
        binding: ActorConnectionBinding,
        workspace_root: &Path,
        expected_manifest_artifact_id: factory_protocol::ArtifactId,
        mut required: Vec<ReadExactFileV1>,
    ) -> Result<Self, WorkspaceReadError> {
        let workspace_root =
            fs::canonicalize(workspace_root).map_err(|source| WorkspaceReadError::Io {
                operation: "canonicalize workspace root",
                path: workspace_root.to_owned(),
                source,
            })?;
        if !fs::metadata(&workspace_root)
            .map_err(|source| WorkspaceReadError::Io {
                operation: "inspect workspace root",
                path: workspace_root.clone(),
                source,
            })?
            .is_dir()
        {
            return Err(WorkspaceReadError::InvalidWorkspaceRoot(workspace_root));
        }
        required.sort_by(|left, right| left.path.cmp(&right.path));
        if required.windows(2).any(|pair| pair[0].path == pair[1].path) {
            return Err(WorkspaceReadError::DuplicateRequiredRead);
        }
        Ok(Self {
            binding,
            workspace_root,
            expected_manifest_artifact_id,
            required,
            observed: BTreeMap::new(),
        })
    }

    /// Creates the only assertion a restarted daemon may honestly make about
    /// required reads: it observed none.  The original workspace may already
    /// have been removed by the time crash recovery acquires the singleton
    /// locks, so recovery must not canonicalize, recreate, or inspect it just
    /// to manufacture a terminal manifest.
    ///
    /// This constructor is intentionally crate-private and named for the
    /// restart path.  Live actor work must use [`Self::from_admitted_assignment`]
    /// so that every non-empty observation came from the wrapped exact-read
    /// boundary.
    pub(crate) fn empty_after_daemon_restart(
        binding: ActorConnectionBinding,
        expected_manifest_artifact_id: factory_protocol::ArtifactId,
        mut required: Vec<ReadExactFileV1>,
    ) -> Result<Self, WorkspaceReadError> {
        required.sort_by(|left, right| left.path.cmp(&right.path));
        if required.windows(2).any(|pair| pair[0].path == pair[1].path) {
            return Err(WorkspaceReadError::DuplicateRequiredRead);
        }
        Ok(Self {
            binding,
            // This path is never opened by the recovery terminal path.  Keep
            // it empty rather than inventing a host path that could later be
            // mistaken for a recovered worktree.
            workspace_root: PathBuf::new(),
            expected_manifest_artifact_id,
            required,
            observed: BTreeMap::new(),
        })
    }

    /// Parses and serves the one closed actor operation. The bound connection
    /// identity remains outside JSON and must match this ledger's session.
    pub(crate) fn handle_frame(
        &mut self,
        frame: &BoundActorFrame,
    ) -> Result<Vec<u8>, WorkspaceReadError> {
        if frame.binding() != &self.binding {
            return Err(WorkspaceReadError::ConnectionIdentityMismatch);
        }
        let request: WorkspaceReadRequest = decode_operation_request(
            frame.frame(),
            factory_protocol::REQUEST_FRAME_MAX_BYTES,
            OP_WORKSPACE_READ,
        )?;
        validate_request_id(&request.request_id)?;
        let path = RepositoryRelativePath::parse(request.repository_relative_path)?;
        let binding = self.binding;
        let result = self.read_exact_for_binding(&binding, path)?;
        let response = WorkspaceReadResponse {
            protocol_version: PROTOCOL_VERSION_V1,
            request_id: request.request_id,
            operation: OP_WORKSPACE_READ.to_owned(),
            canonical_path: result.path.as_str().to_owned(),
            blake3: result.digest.to_hex(),
            byte_length: result.bytes.len() as u64,
            content_base64: encode_base64(&result.bytes),
        };
        let bytes = json::to_string(&response).into_bytes();
        if bytes.len() > RESPONSE_FRAME_MAX_BYTES {
            return Err(WorkspaceReadError::ResponseTooLarge);
        }
        Ok(bytes)
    }

    fn read_exact_for_binding(
        &mut self,
        binding: &ActorConnectionBinding,
        path: RepositoryRelativePath,
    ) -> Result<WorkspaceReadResult, WorkspaceReadError> {
        if binding != &self.binding {
            return Err(WorkspaceReadError::ConnectionIdentityMismatch);
        }
        let result = read_exact_workspace_file(&self.workspace_root, path)?;
        self.observed.insert(result.path.clone(), result.digest);
        Ok(result)
    }

    /// Hashes a daemon-materialized required-read file before its assignment
    /// packet is sealed. This is deliberately the same symlink, regular-file,
    /// byte-bound, and change-detection path the actor gets after admission;
    /// it creates no actor read claim or session ledger.
    pub(crate) fn digest_materialized_required_read(
        workspace_root: &Path,
        path: RepositoryRelativePath,
    ) -> Result<ContentDigest, WorkspaceReadError> {
        Ok(read_exact_workspace_file(workspace_root, path)?.digest)
    }

    /// Executes the same exact read path used by the actor frame dispatcher.
    /// This is exposed for daemon composition and provider-free integration
    /// tests; obtaining the authority still requires a kernel-bound actor
    /// connection, and the returned observation cannot be supplied at
    /// terminal independently of this ledger.
    pub fn read_exact(
        &mut self,
        path: RepositoryRelativePath,
    ) -> Result<WorkspaceReadResponse, WorkspaceReadError> {
        let binding = self.binding;
        let result = self.read_exact_for_binding(&binding, path)?;
        Ok(WorkspaceReadResponse {
            protocol_version: PROTOCOL_VERSION_V1,
            request_id: "daemon-direct-read".to_owned(),
            operation: OP_WORKSPACE_READ.to_owned(),
            canonical_path: result.path.as_str().to_owned(),
            blake3: result.digest.to_hex(),
            byte_length: result.bytes.len() as u64,
            content_base64: encode_base64(&result.bytes),
        })
    }

    /// Proves the daemon's own exact-read ledger contains every assigned
    /// path/digest before an actor operation can create ticket, candidate,
    /// validation, or review authority. Actor payload cannot satisfy this:
    /// only `read_exact_for_binding` writes `observed`.
    pub(crate) fn assert_required_reads_satisfied(&self) -> Result<(), WorkspaceReadError> {
        if self.required.iter().all(|required| {
            self.observed
                .get(&required.path)
                .is_some_and(|digest| *digest == required.digest)
        }) {
            Ok(())
        } else {
            Err(WorkspaceReadError::RequiredReadsIncomplete)
        }
    }
}

fn read_exact_workspace_file(
    workspace_root: &Path,
    path: RepositoryRelativePath,
) -> Result<WorkspaceReadResult, WorkspaceReadError> {
    if path.as_str() == "." {
        return Err(WorkspaceReadError::NotRegularFile(path));
    }
    let candidate = workspace_root.join(path.as_str());
    reject_symlink_components(workspace_root, &path)?;
    let canonical = fs::canonicalize(&candidate).map_err(|source| WorkspaceReadError::Io {
        operation: "canonicalize workspace file",
        path: candidate.clone(),
        source,
    })?;
    let relative = canonical
        .strip_prefix(workspace_root)
        .map_err(|_| WorkspaceReadError::PathEscape(path.clone()))?;
    if relative != Path::new(path.as_str()) {
        return Err(WorkspaceReadError::NonCanonicalPath(path));
    }

    let mut file = File::open(&canonical).map_err(|source| WorkspaceReadError::Io {
        operation: "open workspace file",
        path: canonical.clone(),
        source,
    })?;
    let before = file.metadata().map_err(|source| WorkspaceReadError::Io {
        operation: "inspect workspace file",
        path: canonical.clone(),
        source,
    })?;
    if !before.is_file() {
        return Err(WorkspaceReadError::NotRegularFile(path));
    }
    if before.len() > WORKSPACE_READ_MAX_BYTES {
        return Err(WorkspaceReadError::FileTooLarge {
            path,
            observed: before.len(),
        });
    }
    let mut bytes = Vec::with_capacity(before.len() as usize);
    (&mut file)
        .take(WORKSPACE_READ_MAX_BYTES + 1)
        .read_to_end(&mut bytes)
        .map_err(|source| WorkspaceReadError::Io {
            operation: "read workspace file",
            path: canonical.clone(),
            source,
        })?;
    if bytes.len() as u64 > WORKSPACE_READ_MAX_BYTES {
        return Err(WorkspaceReadError::FileTooLarge {
            path,
            observed: bytes.len() as u64,
        });
    }
    let after = file.metadata().map_err(|source| WorkspaceReadError::Io {
        operation: "reinspect workspace file",
        path: canonical,
        source,
    })?;
    if before.dev() != after.dev()
        || before.ino() != after.ino()
        || before.len() != after.len()
        || before.modified().ok() != after.modified().ok()
        || after.len() != bytes.len() as u64
    {
        return Err(WorkspaceReadError::FileChanged(path));
    }

    let digest = ContentDigest::of_bytes(&bytes);
    Ok(WorkspaceReadResult {
        path,
        digest,
        bytes,
    })
}

impl WorkspaceReadAuthority {
    /// Writes and seals the daemon-derived terminal assertion exactly once.
    /// The resulting capability's fields are private and cannot be fabricated
    /// from an actor-supplied manifest or read observation.
    pub fn seal_assertion(
        self,
        cas: &CasStore,
        staging_root: &Path,
    ) -> Result<SealedRequiredReadAssertion, WorkspaceReadError> {
        let evidence = RequiredReadEvidence::from_authority(self);
        let filename = format!(
            "required-read-assertion-{}.json",
            evidence.binding.session_id().get()
        );
        let path = staging_root.join(&filename);
        let mut file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&path)
            .map_err(|source| WorkspaceReadError::Io {
                operation: "create required-read assertion",
                path: path.clone(),
                source,
            })?;
        file.write_all(&evidence.canonical_bytes)
            .and_then(|()| file.sync_all())
            .map_err(|source| WorkspaceReadError::Io {
                operation: "write required-read assertion",
                path: path.clone(),
                source,
            })?;
        let artifact = cas.adopt(staging_root, &filename)?;
        if artifact.digest() != ContentDigest::of_bytes(&evidence.canonical_bytes) {
            return Err(WorkspaceReadError::AssertionChanged);
        }
        Ok(SealedRequiredReadAssertion { evidence, artifact })
    }

    /// Seals a restart-recovery assertion idempotently. Unlike a live actor,
    /// recovery may run again after an I/O failure between artifact adoption
    /// and terminal admission. Reusing only byte-identical daemon-derived
    /// evidence is safe; a changed pre-existing file is rejected.
    pub(crate) fn seal_assertion_after_daemon_restart(
        self,
        cas: &CasStore,
        staging_root: &Path,
    ) -> Result<SealedRequiredReadAssertion, WorkspaceReadError> {
        let evidence = RequiredReadEvidence::from_authority(self);
        let filename = format!(
            "required-read-assertion-{}.json",
            evidence.binding.session_id().get()
        );
        let path = staging_root.join(&filename);
        match OpenOptions::new().write(true).create_new(true).open(&path) {
            Ok(mut file) => file
                .write_all(&evidence.canonical_bytes)
                .and_then(|()| file.sync_all())
                .map_err(|source| WorkspaceReadError::Io {
                    operation: "write recovery required-read assertion",
                    path: path.clone(),
                    source,
                })?,
            Err(source) if source.kind() == io::ErrorKind::AlreadyExists => {}
            Err(source) => {
                return Err(WorkspaceReadError::Io {
                    operation: "create recovery required-read assertion",
                    path,
                    source,
                });
            }
        }
        let artifact = cas.adopt(staging_root, &filename)?;
        if cas.read(artifact.digest())? != evidence.canonical_bytes {
            return Err(WorkspaceReadError::AssertionChanged);
        }
        Ok(SealedRequiredReadAssertion { evidence, artifact })
    }
}

#[derive(Debug)]
struct WorkspaceReadResult {
    path: RepositoryRelativePath,
    digest: ContentDigest,
    bytes: Vec<u8>,
}

#[derive(Clone, Debug)]
struct RequiredReadEvidence {
    binding: ActorConnectionBinding,
    expected_manifest_artifact_id: factory_protocol::ArtifactId,
    expected: Vec<ReadExactFileV1>,
    observed: Vec<ReadObservationV1>,
    satisfied_count: u32,
    canonical_bytes: Vec<u8>,
}

impl RequiredReadEvidence {
    fn from_authority(authority: WorkspaceReadAuthority) -> Self {
        let observed = authority
            .observed
            .into_iter()
            .map(|(path, digest)| ReadObservationV1 { path, digest })
            .collect::<Vec<_>>();
        let expected = authority.required;
        let satisfied_count = expected
            .iter()
            .filter(|item| {
                observed
                    .iter()
                    .any(|seen| seen.path == item.path && seen.digest == item.digest)
            })
            .count() as u32;
        let canonical_bytes = canonical_assertion_bytes(
            authority.binding,
            authority.expected_manifest_artifact_id,
            &expected,
            &observed,
        );
        Self {
            binding: authority.binding,
            expected_manifest_artifact_id: authority.expected_manifest_artifact_id,
            expected,
            observed,
            satisfied_count,
            canonical_bytes,
        }
    }
}

/// Private terminal capability created only by [`WorkspaceReadAuthority`].
#[derive(Clone, Debug)]
pub struct SealedRequiredReadAssertion {
    evidence: RequiredReadEvidence,
    artifact: CasArtifact,
}

impl SealedRequiredReadAssertion {
    pub(crate) const fn binding(&self) -> ActorConnectionBinding {
        self.evidence.binding
    }

    pub(crate) const fn expected_manifest_artifact_id(&self) -> factory_protocol::ArtifactId {
        self.evidence.expected_manifest_artifact_id
    }

    pub(crate) fn expected(&self) -> &[ReadExactFileV1] {
        &self.evidence.expected
    }

    pub(crate) fn observed(&self) -> &[ReadObservationV1] {
        &self.evidence.observed
    }

    pub(crate) const fn satisfied_count(&self) -> u32 {
        self.evidence.satisfied_count
    }

    pub(crate) fn canonical_bytes(&self) -> &[u8] {
        &self.evidence.canonical_bytes
    }

    pub const fn artifact(&self) -> CasArtifact {
        self.artifact
    }
}

#[derive(Serialize)]
struct AssertionManifestWire {
    format: String,
    session_id: i64,
    assignment_id: i64,
    expected_manifest_artifact_id: i64,
    required: Vec<AssertionRequiredWire>,
    observed: Vec<AssertionObservedWire>,
    missing: Vec<AssertionRequiredWire>,
}

#[derive(Clone, Serialize)]
struct AssertionRequiredWire {
    canonical_path: String,
    blake3: String,
    reason: String,
}

#[derive(Serialize)]
struct AssertionObservedWire {
    canonical_path: String,
    blake3: String,
}

fn canonical_assertion_bytes(
    binding: ActorConnectionBinding,
    expected_manifest_artifact_id: factory_protocol::ArtifactId,
    required: &[ReadExactFileV1],
    observed: &[ReadObservationV1],
) -> Vec<u8> {
    let required = required
        .iter()
        .map(|item| AssertionRequiredWire {
            canonical_path: item.path.as_str().to_owned(),
            blake3: item.digest.to_hex(),
            reason: item.reason.clone(),
        })
        .collect::<Vec<_>>();
    let missing = required
        .iter()
        .filter(|item| {
            !observed.iter().any(|seen| {
                seen.path.as_str() == item.canonical_path && seen.digest.to_hex() == item.blake3
            })
        })
        .cloned()
        .collect();
    let observed = observed
        .iter()
        .map(|item| AssertionObservedWire {
            canonical_path: item.path.as_str().to_owned(),
            blake3: item.digest.to_hex(),
        })
        .collect();
    json::to_string(&AssertionManifestWire {
        format: "factory-required-read-assertion-v1".to_owned(),
        session_id: binding.session_id().get(),
        assignment_id: binding.assignment_id().get(),
        expected_manifest_artifact_id: expected_manifest_artifact_id.get(),
        required,
        observed,
        missing,
    })
    .into_bytes()
}

fn reject_symlink_components(
    root: &Path,
    relative: &RepositoryRelativePath,
) -> Result<(), WorkspaceReadError> {
    let mut current = root.to_owned();
    for component in relative.as_str().split('/') {
        current.push(component);
        let metadata = fs::symlink_metadata(&current).map_err(|source| WorkspaceReadError::Io {
            operation: "inspect workspace path component",
            path: current.clone(),
            source,
        })?;
        if metadata.file_type().is_symlink() {
            return Err(WorkspaceReadError::Symlink(relative.clone()));
        }
    }
    Ok(())
}

fn validate_request_id(value: &str) -> Result<(), WorkspaceReadError> {
    if value.is_empty()
        || value.len() > 160
        || !value
            .bytes()
            .all(|byte| byte.is_ascii_graphic() || byte == b' ')
    {
        return Err(WorkspaceReadError::InvalidRequestId);
    }
    Ok(())
}

fn encode_base64(bytes: &[u8]) -> String {
    const TABLE: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut output = String::with_capacity(bytes.len().div_ceil(3) * 4);
    for chunk in bytes.chunks(3) {
        let first = chunk[0];
        let second = chunk.get(1).copied().unwrap_or(0);
        let third = chunk.get(2).copied().unwrap_or(0);
        output.push(TABLE[(first >> 2) as usize] as char);
        output.push(TABLE[(((first & 0x03) << 4) | (second >> 4)) as usize] as char);
        output.push(if chunk.len() > 1 {
            TABLE[(((second & 0x0f) << 2) | (third >> 6)) as usize] as char
        } else {
            '='
        });
        output.push(if chunk.len() > 2 {
            TABLE[(third & 0x3f) as usize] as char
        } else {
            '='
        });
    }
    output
}

#[derive(Debug, Error)]
pub enum WorkspaceReadError {
    #[error(transparent)]
    Frame(#[from] factory_protocol::FrameError),

    #[error(transparent)]
    Contract(#[from] factory_protocol::ContractError),

    #[error(transparent)]
    Cas(#[from] crate::cas::CasError),

    #[error("workspace root {0} is not a directory")]
    InvalidWorkspaceRoot(PathBuf),

    #[error("required-read paths must be unique")]
    DuplicateRequiredRead,

    #[error("workspace read actor connection does not match its session ledger")]
    ConnectionIdentityMismatch,

    #[error("workspace request ID is invalid")]
    InvalidRequestId,

    #[error("workspace read path escapes its pinned root: {0:?}")]
    PathEscape(RepositoryRelativePath),

    #[error("workspace read path is not canonical: {0:?}")]
    NonCanonicalPath(RepositoryRelativePath),

    #[error("workspace read refuses a symbolic link: {0:?}")]
    Symlink(RepositoryRelativePath),

    #[error("workspace read requires a regular file: {0:?}")]
    NotRegularFile(RepositoryRelativePath),

    #[error(
        "workspace file {path:?} exceeds the {WORKSPACE_READ_MAX_BYTES}-byte limit ({observed})"
    )]
    FileTooLarge {
        path: RepositoryRelativePath,
        observed: u64,
    },

    #[error("workspace file changed while it was read: {0:?}")]
    FileChanged(RepositoryRelativePath),

    #[error("workspace read response exceeds the protocol bound")]
    ResponseTooLarge,

    #[error("required-read assertion changed while it was sealed")]
    AssertionChanged,

    #[error("all assigned required reads must be observed before a durable actor mutation")]
    RequiredReadsIncomplete,

    #[error("I/O while {operation} at {path}: {source}")]
    Io {
        operation: &'static str,
        path: PathBuf,
        #[source]
        source: io::Error,
    },
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::local_transport::ActorConnectionIdentity;
    use factory_protocol::{
        ApplicationRevisionId, AssignmentId, AssignmentRole, CampaignId, SessionId,
    };
    use std::os::unix::fs::symlink;

    fn binding(session: i64) -> ActorConnectionBinding {
        ActorConnectionBinding::from_identity(ActorConnectionIdentity::from_admitted_assignment(
            SessionId::new(session).unwrap(),
            AssignmentId::new(2).unwrap(),
            ApplicationRevisionId::new(3).unwrap(),
            CampaignId::new(4).unwrap(),
            AssignmentRole::Engineering,
        ))
    }

    #[test]
    fn exact_read_records_daemon_observation_and_rejects_symlinks() {
        let root = std::env::temp_dir().join(format!(
            "factory-workspace-read-{}-{}",
            std::process::id(),
            fastrand::u64(..)
        ));
        fs::create_dir_all(root.join("docs")).unwrap();
        fs::write(root.join("docs/contract.md"), b"exact bytes\0\xff").unwrap();
        symlink("contract.md", root.join("docs/link.md")).unwrap();
        let digest = ContentDigest::of_bytes(b"exact bytes\0\xff");
        let actor = binding(1);
        let mut authority = WorkspaceReadAuthority::from_admitted_assignment(
            actor,
            &root,
            factory_protocol::ArtifactId::new(5).unwrap(),
            vec![ReadExactFileV1 {
                path: RepositoryRelativePath::parse("docs/contract.md").unwrap(),
                digest,
                reason: "contract".to_owned(),
            }],
        )
        .unwrap();
        let result = authority
            .read_exact_for_binding(
                &actor,
                RepositoryRelativePath::parse("docs/contract.md").unwrap(),
            )
            .unwrap();
        assert_eq!(result.bytes, b"exact bytes\0\xff");
        assert_eq!(result.digest, digest);
        assert!(matches!(
            authority.read_exact_for_binding(
                &actor,
                RepositoryRelativePath::parse("docs/link.md").unwrap()
            ),
            Err(WorkspaceReadError::Symlink(_))
        ));
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn assertion_is_distinct_typed_session_evidence() {
        let root = std::env::temp_dir().join(format!(
            "factory-workspace-assertion-{}-{}",
            std::process::id(),
            fastrand::u64(..)
        ));
        fs::create_dir_all(root.join("staging")).unwrap();
        fs::write(root.join("AGENTS.md"), b"rules").unwrap();
        let actor = binding(11);
        let mut authority = WorkspaceReadAuthority::from_admitted_assignment(
            actor,
            &root,
            factory_protocol::ArtifactId::new(7).unwrap(),
            vec![ReadExactFileV1 {
                path: RepositoryRelativePath::parse("AGENTS.md").unwrap(),
                digest: ContentDigest::of_bytes(b"rules"),
                reason: "rules".to_owned(),
            }],
        )
        .unwrap();
        authority
            .read_exact_for_binding(&actor, RepositoryRelativePath::parse("AGENTS.md").unwrap())
            .unwrap();
        let cas = CasStore::new_with_seed(root.join("cas"), 1024 * 1024, 1).unwrap();
        let sealed = authority
            .seal_assertion(&cas, &root.join("staging"))
            .unwrap();
        let bytes = cas.read(sealed.artifact().digest()).unwrap();
        let text = std::str::from_utf8(&bytes).unwrap();
        assert!(text.contains("factory-required-read-assertion-v1"));
        assert!(text.contains("\"session_id\":11"));
        assert!(text.contains("\"missing\":[]"));
        assert_eq!(sealed.satisfied_count(), 1);
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn durable_mutation_gate_requires_daemon_exact_reads() {
        let root = std::env::temp_dir().join(format!(
            "factory-workspace-mutation-gate-{}-{}",
            std::process::id(),
            fastrand::u64(..)
        ));
        fs::create_dir_all(&root).unwrap();
        fs::write(root.join("AGENTS.md"), b"rules").unwrap();
        let actor = binding(12);
        let mut authority = WorkspaceReadAuthority::from_admitted_assignment(
            actor,
            &root,
            factory_protocol::ArtifactId::new(8).unwrap(),
            vec![ReadExactFileV1 {
                path: RepositoryRelativePath::parse("AGENTS.md").unwrap(),
                digest: ContentDigest::of_bytes(b"rules"),
                reason: "rules".to_owned(),
            }],
        )
        .unwrap();
        assert!(matches!(
            authority.assert_required_reads_satisfied(),
            Err(WorkspaceReadError::RequiredReadsIncomplete)
        ));
        authority
            .read_exact_for_binding(&actor, RepositoryRelativePath::parse("AGENTS.md").unwrap())
            .unwrap();
        authority.assert_required_reads_satisfied().unwrap();
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn daemon_restart_assertion_is_honestly_empty_without_a_workspace() {
        let root = std::env::temp_dir().join(format!(
            "factory-workspace-restart-{}-{}",
            std::process::id(),
            fastrand::u64(..)
        ));
        let staging = root.join("staging");
        fs::create_dir_all(&staging).unwrap();
        let cas = CasStore::new_with_seed(root.join("cas"), 1024 * 1024, 1).unwrap();
        let sealed = WorkspaceReadAuthority::empty_after_daemon_restart(
            binding(12),
            factory_protocol::ArtifactId::new(8).unwrap(),
            vec![ReadExactFileV1 {
                path: RepositoryRelativePath::parse("AGENTS.md").unwrap(),
                digest: ContentDigest::of_bytes(b"rules"),
                reason: "rules".to_owned(),
            }],
        )
        .unwrap()
        .seal_assertion(&cas, &staging)
        .unwrap();
        let text = String::from_utf8(cas.read(sealed.artifact().digest()).unwrap()).unwrap();
        assert!(text.contains("\"missing\":[{\"canonical_path\":\"AGENTS.md\""));
        assert_eq!(sealed.satisfied_count(), 0);
        fs::remove_dir_all(root).unwrap();
    }
}
