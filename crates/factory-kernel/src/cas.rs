//! Append-only, content-addressed artifact custody.
//!
//! `CasStore` is the physical boundary for bytes which have already been
//! produced in an assigned staging directory.  The store never updates or
//! removes an installed object.  A successful adoption computes the BLAKE3
//! identity while streaming through a bounded temporary file, then installs
//! that file with one same-filesystem, absent-only publication. Temporary
//! names and fault plans are operational concerns; neither affects content
//! identity or durable provenance.

use std::fs::{self, File, OpenOptions};
use std::io::{self, Read, Write};
use std::path::{Component, Path, PathBuf};
use std::sync::Mutex;
use std::time::{SystemTime, UNIX_EPOCH};

use factory_protocol::{
    ContentDigest, KernelBuildId, MAX_SESSION_OUTPUT_BYTES, RuntimeRelativePath,
};
use thiserror::Error;

/// A non-zero upper bound for one adopted object is required by the custody
/// boundary.  The application supplies the role-specific value.
pub const DEFAULT_MAX_OBJECT_BYTES: u64 = MAX_SESSION_OUTPUT_BYTES;

/// Errors raised at the physical CAS boundary.
#[derive(Debug, Error)]
pub enum CasError {
    #[error("CAS object limit must be greater than zero")]
    InvalidLimit,

    #[error("{path} is not a directory: {reason}")]
    InvalidDirectory { path: PathBuf, reason: &'static str },

    #[error("source path {path} is unsafe: {reason}")]
    UnsafeSourcePath { path: PathBuf, reason: &'static str },

    #[error("source path {path} escapes its assigned staging root")]
    SourcePathEscape { path: PathBuf },

    #[error("source path {path} contains a symbolic link")]
    SourceSymlink { path: PathBuf },

    #[error("source path {path} is not a regular file")]
    SourceNotRegularFile { path: PathBuf },

    #[error("CAS object {digest} is missing")]
    MissingObject { digest: ContentDigest },

    #[error("CAS object {digest} is not a regular file")]
    ObjectNotRegularFile { digest: ContentDigest },

    #[error(
        "source {path} exceeds the maximum object size of {maximum} bytes (observed {observed})"
    )]
    SizeLimitExceeded {
        path: PathBuf,
        maximum: u64,
        observed: u64,
    },

    #[error("source {path} changed while it was being adopted")]
    SourceChanged { path: PathBuf },

    #[error("deterministic CAS fault at {operation}")]
    InjectedFault { operation: &'static str },

    #[error("the daemon startup timestamp predates the Unix epoch")]
    InvalidStartupTimestamp,

    #[error(
        "CAS object {digest} is corrupted ({reason}); expected {expected_length} bytes, found {actual_length}"
    )]
    CorruptObject {
        digest: ContentDigest,
        expected_length: u64,
        actual_length: u64,
        actual_digest: Option<ContentDigest>,
        reason: &'static str,
    },

    #[error("I/O while {operation} at {path}: {source}")]
    Io {
        operation: &'static str,
        path: PathBuf,
        #[source]
        source: io::Error,
    },
}

/// Metadata returned after an object has been physically sealed in CAS.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct CasArtifact {
    digest: ContentDigest,
    byte_length: u64,
}

/// Deterministic physical-failure plan used by provider-free custody tests.
///
/// These faults model failures at the byte-copy, temporary-file fsync, and
/// absent-only publication boundaries. They are deliberately finite and
/// structural rather than callbacks, so production code cannot inject
/// arbitrary behavior into custody.
#[cfg(test)]
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct CasFaultPlan {
    /// Fail after a temporary copy has reached at least this many bytes.
    pub fail_after_copied_bytes: Option<u64>,
    /// Fail before syncing the temporary object.
    pub fail_temporary_fsync: bool,
    /// Fail before publishing the temporary object under its digest name.
    pub fail_install: bool,
    /// Fail before syncing the containing object directory.
    pub fail_directory_fsync: bool,
}

impl CasArtifact {
    #[must_use]
    pub const fn digest(self) -> ContentDigest {
        self.digest
    }

    #[must_use]
    pub const fn byte_length(self) -> u64 {
        self.byte_length
    }

    #[must_use]
    pub const fn length(self) -> u64 {
        self.byte_length
    }
}

/// Append-only filesystem CAS rooted below one kernel runtime directory.
#[derive(Debug)]
pub struct CasStore {
    runtime_root: PathBuf,
    objects_root: PathBuf,
    maximum_object_bytes: u64,
    temp_suffixes: Mutex<fastrand::Rng>,
    #[cfg(test)]
    faults: CasFaultPlan,
}

impl Clone for CasStore {
    fn clone(&self) -> Self {
        Self {
            runtime_root: self.runtime_root.clone(),
            objects_root: self.objects_root.clone(),
            maximum_object_bytes: self.maximum_object_bytes,
            temp_suffixes: Mutex::new(
                self.temp_suffixes
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner)
                    .clone(),
            ),
            #[cfg(test)]
            faults: self.faults,
        }
    }
}

/// Short name retained for callers that refer to the physical store as CAS.
pub type Cas = CasStore;

/// Short name for the sealed object metadata.
pub type Artifact = CasArtifact;

impl CasStore {
    /// Creates a store beneath `runtime_root` using an explicit operational
    /// temporary-name seed.
    ///
    /// The caller must derive this seed from the qualified kernel build, PID,
    /// and daemon startup timestamp with [`Self::temporary_name_seed`]. That
    /// operational entropy is intentionally not content identity and is never
    /// generated silently without the installing kernel build.
    pub fn new(
        runtime_root: impl AsRef<Path>,
        maximum_object_bytes: u64,
        temp_seed: u64,
    ) -> Result<Self, CasError> {
        Self::new_with_seed(runtime_root, maximum_object_bytes, temp_seed)
    }

    /// Mixes the exact kernel build identity with resident-process identity
    /// and the caller-captured startup timestamp for temporary CAS names.
    /// This value changes publication temporary names only, never the digest
    /// or database-visible identity of a sealed artifact.
    pub fn temporary_name_seed(
        kernel_build_id: KernelBuildId,
        process_id: u32,
        startup_at: SystemTime,
    ) -> Result<u64, CasError> {
        let startup_nanos = startup_at
            .duration_since(UNIX_EPOCH)
            .map_err(|_| CasError::InvalidStartupTimestamp)?
            .as_nanos()
            .to_be_bytes();
        let mut hasher = blake3::Hasher::new();
        hasher.update(&kernel_build_id.digest().as_bytes());
        hasher.update(&process_id.to_be_bytes());
        hasher.update(&startup_nanos);
        let digest = hasher.finalize();
        Ok(u64::from_be_bytes(
            digest.as_bytes()[..8]
                .try_into()
                .expect("BLAKE3 has 32 bytes"),
        ))
    }

    /// Creates a store with an explicit operational suffix seed.
    ///
    /// The seed affects temporary names only; it never contributes to the
    /// content identity or durable artifact identity. The daemon should mix
    /// its kernel build identity, PID, and startup timestamp before passing
    /// the result here. Tests use this constructor to make temporary-name
    /// generation deterministic without relying on ambient randomness.
    pub fn new_with_seed(
        runtime_root: impl AsRef<Path>,
        maximum_object_bytes: u64,
        temp_seed: u64,
    ) -> Result<Self, CasError> {
        if maximum_object_bytes == 0 {
            return Err(CasError::InvalidLimit);
        }

        let runtime_root = runtime_root.as_ref();
        if runtime_root.as_os_str().is_empty() {
            return Err(CasError::InvalidDirectory {
                path: runtime_root.to_owned(),
                reason: "path is empty",
            });
        }

        fs::create_dir_all(runtime_root)
            .map_err(|source| io_error("create runtime root", runtime_root, source))?;
        let runtime_root = fs::canonicalize(runtime_root)
            .map_err(|source| io_error("canonicalize runtime root", runtime_root, source))?;
        ensure_real_directory(&runtime_root)?;

        let objects_root = runtime_root.join("objects").join("blake3");
        fs::create_dir_all(&objects_root)
            .map_err(|source| io_error("create CAS root", &objects_root, source))?;
        let objects_root = fs::canonicalize(&objects_root)
            .map_err(|source| io_error("canonicalize CAS root", &objects_root, source))?;
        ensure_real_directory(&objects_root)?;

        Ok(Self {
            runtime_root,
            objects_root,
            maximum_object_bytes,
            temp_suffixes: Mutex::new(fastrand::Rng::with_seed(temp_seed)),
            #[cfg(test)]
            faults: CasFaultPlan::default(),
        })
    }

    /// Creates a store with deterministic physical-failure injection for the
    /// in-crate custody tests. Fault injection is not part of the production
    /// API: failure points remain structural and cannot be supplied by an
    /// actor or application process.
    #[cfg(test)]
    pub fn new_with_seed_and_faults(
        runtime_root: impl AsRef<Path>,
        maximum_object_bytes: u64,
        temp_seed: u64,
        faults: CasFaultPlan,
    ) -> Result<Self, CasError> {
        let mut store = Self::new_with_seed(runtime_root, maximum_object_bytes, temp_seed)?;
        store.faults = faults;
        Ok(store)
    }

    /// Creates a store with the conservative default bound.
    pub fn with_default_limit(
        runtime_root: impl AsRef<Path>,
        temp_seed: u64,
    ) -> Result<Self, CasError> {
        Self::new(runtime_root, DEFAULT_MAX_OBJECT_BYTES, temp_seed)
    }

    #[must_use]
    pub fn runtime_root(&self) -> &Path {
        &self.runtime_root
    }

    #[must_use]
    pub const fn maximum_object_bytes(&self) -> u64 {
        self.maximum_object_bytes
    }

    /// Returns the canonical CAS path for a digest without touching the file.
    #[must_use]
    pub fn object_path(&self, digest: ContentDigest) -> PathBuf {
        let hex = digest.to_hex();
        self.objects_root.join(&hex[..2]).join(&hex[2..])
    }

    /// Returns the only runtime-relative path that can name a CAS object.
    /// Callers cannot supply or persist an unrelated physical path.
    pub fn object_relative_path(
        &self,
        digest: ContentDigest,
    ) -> Result<RuntimeRelativePath, CasError> {
        RuntimeRelativePath::parse(format!(
            "objects/blake3/{}/{}",
            &digest.to_hex()[..2],
            &digest.to_hex()[2..]
        ))
        .map_err(|_| CasError::InvalidDirectory {
            path: self.object_path(digest),
            reason: "CAS digest path is not a safe runtime-relative path",
        })
    }

    /// Adopts one regular file beneath an assigned staging root.
    ///
    /// `relative_path` is intentionally a path, rather than an arbitrary
    /// host path, so the staging-root boundary is explicit at every call.
    pub fn adopt(
        &self,
        staging_root: impl AsRef<Path>,
        relative_path: impl AsRef<Path>,
    ) -> Result<CasArtifact, CasError> {
        let (source_path, source_file) =
            self.open_staged_file(staging_root.as_ref(), relative_path.as_ref())?;
        self.adopt_open_file(&source_path, source_file)
    }

    /// Alias making the source-file operation explicit at call sites.
    pub fn adopt_file(
        &self,
        staging_root: impl AsRef<Path>,
        relative_path: impl AsRef<Path>,
    ) -> Result<CasArtifact, CasError> {
        self.adopt(staging_root, relative_path)
    }

    /// Adopts bounded bytes produced by the trusted kernel itself.
    ///
    /// Actor-controlled files must continue through [`Self::adopt`], which
    /// proves the staging-root and regular-file boundary. This seam exists for
    /// canonical kernel receipts and proposal envelopes that never have a
    /// source pathname; it uses the same append-only publication path.
    pub(crate) fn adopt_kernel_bytes(&self, bytes: &[u8]) -> Result<CasArtifact, CasError> {
        let byte_length = u64::try_from(bytes.len()).map_err(|_| CasError::SizeLimitExceeded {
            path: self.runtime_root.join("kernel-generated-bytes"),
            maximum: self.maximum_object_bytes,
            observed: u64::MAX,
        })?;
        if byte_length > self.maximum_object_bytes {
            return Err(CasError::SizeLimitExceeded {
                path: self.runtime_root.join("kernel-generated-bytes"),
                maximum: self.maximum_object_bytes,
                observed: byte_length,
            });
        }
        let (temporary, mut temp) = self.create_temporary()?;
        if let Err(source) = temp.write_all(bytes) {
            drop(temp);
            let _ = fs::remove_file(&temporary);
            return Err(io_error(
                "write kernel-generated CAS temporary object",
                &temporary,
                source,
            ));
        }
        self.publish_temporary(temporary, temp, ContentDigest::of_bytes(bytes), byte_length)
    }

    /// Verifies and reads an object, including its digest and byte length.
    pub fn read_verified(&self, digest: ContentDigest) -> Result<Vec<u8>, CasError> {
        let path = self.checked_object_path(digest, false)?;
        let mut file = open_regular_file(&path).map_err(|error| match error {
            OpenError::Missing => CasError::MissingObject { digest },
            OpenError::NotRegular => CasError::ObjectNotRegularFile { digest },
            OpenError::Io(source) => io_error("open CAS object", &path, source),
        })?;

        let declared_length = file
            .metadata()
            .map_err(|source| io_error("stat CAS object", &path, source))?
            .len();
        if declared_length > self.maximum_object_bytes {
            return Err(CasError::CorruptObject {
                digest,
                expected_length: self.maximum_object_bytes,
                actual_length: declared_length,
                actual_digest: None,
                reason: "object exceeds configured size limit",
            });
        }

        // The metadata length is only a hint: a same-user writer can extend
        // the file after the first stat.  `take(max + 1)` keeps allocation and
        // I/O bounded even in that race; the post-read stat catches both an
        // extension and a truncation, while hashing catches same-length edits.
        let read_limit = self.maximum_object_bytes.saturating_add(1);
        let mut bytes = Vec::new();
        let mut bounded = (&mut file).take(read_limit);
        bounded
            .read_to_end(&mut bytes)
            .map_err(|source| io_error("read CAS object", &path, source))?;
        if bytes.len() as u64 > self.maximum_object_bytes {
            return Err(CasError::CorruptObject {
                digest,
                expected_length: declared_length,
                actual_length: bytes.len() as u64,
                actual_digest: None,
                reason: "object exceeds configured size limit",
            });
        }
        let final_length = file
            .metadata()
            .map_err(|source| io_error("restat CAS object", &path, source))?
            .len();
        if final_length != declared_length || final_length != bytes.len() as u64 {
            return Err(CasError::CorruptObject {
                digest,
                expected_length: declared_length,
                actual_length: final_length,
                actual_digest: None,
                reason: "object changed while it was being read",
            });
        }
        self.verify_bytes(digest, declared_length, &bytes)?;
        Ok(bytes)
    }

    /// Verifies an object without returning its bytes.
    pub fn verify(&self, digest: ContentDigest) -> Result<CasArtifact, CasError> {
        let bytes = self.read_verified(digest)?;
        Ok(CasArtifact {
            digest,
            byte_length: bytes.len() as u64,
        })
    }

    /// Alias for callers that prefer the operation's read-oriented name.
    pub fn read(&self, digest: ContentDigest) -> Result<Vec<u8>, CasError> {
        self.read_verified(digest)
    }

    fn open_staged_file(
        &self,
        staging_root: &Path,
        relative_path: &Path,
    ) -> Result<(PathBuf, File), CasError> {
        validate_relative_source(relative_path)?;

        let root_metadata = fs::symlink_metadata(staging_root)
            .map_err(|source| io_error("inspect staging root", staging_root, source))?;
        if root_metadata.file_type().is_symlink() {
            return Err(CasError::SourceSymlink {
                path: staging_root.to_owned(),
            });
        }
        let staging_root = fs::canonicalize(staging_root)
            .map_err(|source| io_error("canonicalize staging root", staging_root, source))?;
        ensure_real_directory(&staging_root)?;

        let lexical_path = staging_root.join(relative_path);
        let mut component_path = staging_root.clone();
        for component in relative_path.components() {
            if let Component::Normal(value) = component {
                component_path.push(value);
            }
            let metadata = fs::symlink_metadata(&component_path)
                .map_err(|source| io_error("inspect staged path", &component_path, source))?;
            if metadata.file_type().is_symlink() {
                return Err(CasError::SourceSymlink {
                    path: component_path,
                });
            }
        }

        let canonical_path = fs::canonicalize(&lexical_path)
            .map_err(|source| io_error("canonicalize staged file", &lexical_path, source))?;
        if !canonical_path.starts_with(&staging_root) {
            return Err(CasError::SourcePathEscape { path: lexical_path });
        }

        let metadata = fs::symlink_metadata(&canonical_path)
            .map_err(|source| io_error("inspect staged file", &canonical_path, source))?;
        if metadata.file_type().is_symlink() {
            return Err(CasError::SourceSymlink {
                path: canonical_path,
            });
        }
        if !metadata.file_type().is_file() {
            return Err(CasError::SourceNotRegularFile {
                path: canonical_path,
            });
        }

        let file = open_source_file(&canonical_path)
            .map_err(|source| io_error("open staged file", &canonical_path, source))?;
        Ok((canonical_path, file))
    }

    fn adopt_open_file(
        &self,
        source_path: &Path,
        mut source: File,
    ) -> Result<CasArtifact, CasError> {
        let initial_length = source
            .metadata()
            .map_err(|error| io_error("stat staged file", source_path, error))?
            .len();
        let initial_modified = source
            .metadata()
            .ok()
            .and_then(|metadata| metadata.modified().ok());
        if initial_length > self.maximum_object_bytes {
            return Err(CasError::SizeLimitExceeded {
                path: source_path.to_owned(),
                maximum: self.maximum_object_bytes,
                observed: initial_length,
            });
        }

        let (temporary, mut temp) = self.create_temporary()?;

        let streamed = stream_to_temp(
            &mut source,
            &mut temp,
            source_path,
            initial_length,
            initial_modified,
            self.maximum_object_bytes,
            self.fail_after_copied_bytes(),
        );
        let (digest, byte_length) = match streamed {
            Ok(value) => value,
            Err(error) => {
                drop(temp);
                let _ = fs::remove_file(&temporary);
                return Err(error);
            }
        };

        self.publish_temporary(temporary, temp, digest, byte_length)
    }

    fn publish_temporary(
        &self,
        temporary: PathBuf,
        temp: File,
        digest: ContentDigest,
        byte_length: u64,
    ) -> Result<CasArtifact, CasError> {
        if self.fail_temporary_fsync() {
            drop(temp);
            let _ = fs::remove_file(&temporary);
            return Err(CasError::InjectedFault {
                operation: "temporary fsync",
            });
        }
        if let Err(source) = temp.sync_all() {
            drop(temp);
            let _ = fs::remove_file(&temporary);
            return Err(io_error("fsync CAS temporary object", &temporary, source));
        }
        drop(temp);

        let destination = self.object_path(digest);
        let destination_parent = match self.prepare_object_parent(digest) {
            Ok(parent) => parent,
            Err(error) => {
                let _ = fs::remove_file(&temporary);
                return Err(error);
            }
        };
        // A normal rename replaces an existing path on Unix, which would
        // violate append-only custody in a concurrent adoption. A hard link
        // gives atomic absent-only publication on the same filesystem: either
        // the digest name appears fully formed, or it does not. Both names
        // are under `objects_root`, so cross-filesystem linking is impossible.
        // The temporary file is fsynced first; the destination directory is
        // synced after linking, and only then is the temporary name unlinked.
        if self.fail_install() {
            let _ = fs::remove_file(&temporary);
            return Err(CasError::InjectedFault {
                operation: "CAS install",
            });
        }
        match fs::hard_link(&temporary, &destination) {
            Ok(()) => {
                if let Err(error) = self.sync_directory(&destination_parent) {
                    let _ = fs::remove_file(&temporary);
                    return Err(error);
                }
                // The prefix directory is a new directory entry in the CAS
                // root on first use. Syncing both levels makes that entry and
                // the digest entry durable across a crash. Hard-link
                // publication remains absent-only and same-filesystem.
                if let Err(error) = self.sync_directory(&self.objects_root) {
                    let _ = fs::remove_file(&temporary);
                    return Err(error);
                }
                fs::remove_file(&temporary).map_err(|source| {
                    io_error("remove CAS temporary object", &temporary, source)
                })?;
            }
            Err(source) if source.kind() == io::ErrorKind::AlreadyExists => {
                let _ = fs::remove_file(&temporary);
                self.verify_object(digest, byte_length)?;
            }
            Err(source) => {
                let _ = fs::remove_file(&temporary);
                return Err(io_error(
                    "atomically install CAS object",
                    &destination,
                    source,
                ));
            }
        }

        Ok(CasArtifact {
            digest,
            byte_length,
        })
    }

    fn verify_object(&self, digest: ContentDigest, expected_length: u64) -> Result<(), CasError> {
        let path = self.checked_object_path(digest, false)?;
        let file = match open_regular_file(&path) {
            Ok(file) => file,
            Err(OpenError::Missing) => return Err(CasError::MissingObject { digest }),
            Err(OpenError::NotRegular) => return Err(CasError::ObjectNotRegularFile { digest }),
            Err(OpenError::Io(source)) => return Err(io_error("open CAS object", &path, source)),
        };
        let declared_length = file
            .metadata()
            .map_err(|source| io_error("stat CAS object", &path, source))?
            .len();
        if declared_length > self.maximum_object_bytes {
            return Err(CasError::CorruptObject {
                digest,
                expected_length,
                actual_length: declared_length,
                actual_digest: None,
                reason: "object exceeds configured size limit",
            });
        }
        let bytes = self.read_verified(digest)?;
        if bytes.len() as u64 != expected_length {
            return Err(CasError::CorruptObject {
                digest,
                expected_length,
                actual_length: bytes.len() as u64,
                actual_digest: Some(ContentDigest::of_bytes(&bytes)),
                reason: "length does not match the adopted object",
            });
        }
        Ok(())
    }

    fn verify_bytes(
        &self,
        digest: ContentDigest,
        expected_length: u64,
        bytes: &[u8],
    ) -> Result<(), CasError> {
        let actual_length = bytes.len() as u64;
        let actual_digest = ContentDigest::of_bytes(bytes);
        if actual_length != expected_length || actual_digest != digest {
            return Err(CasError::CorruptObject {
                digest,
                expected_length,
                actual_length,
                actual_digest: Some(actual_digest),
                reason: if actual_length != expected_length {
                    "length does not match the adopted object"
                } else {
                    "digest does not match the object path"
                },
            });
        }
        Ok(())
    }

    fn checked_object_path(
        &self,
        digest: ContentDigest,
        create_parent: bool,
    ) -> Result<PathBuf, CasError> {
        let path = self.object_path(digest);
        if create_parent {
            self.prepare_object_parent(digest)?;
        } else {
            let parent = path.parent().expect("CAS object path has a parent");
            let metadata = match fs::symlink_metadata(parent) {
                Ok(metadata) => metadata,
                Err(source) if source.kind() == io::ErrorKind::NotFound => {
                    return Err(CasError::MissingObject { digest });
                }
                Err(source) => {
                    return Err(io_error("inspect CAS object directory", parent, source));
                }
            };
            if metadata.file_type().is_symlink() || !metadata.file_type().is_dir() {
                return Err(CasError::SourcePathEscape {
                    path: parent.to_owned(),
                });
            }
            let canonical_parent = fs::canonicalize(parent)
                .map_err(|source| io_error("canonicalize CAS object directory", parent, source))?;
            if !canonical_parent.starts_with(&self.objects_root) {
                return Err(CasError::SourcePathEscape {
                    path: parent.to_owned(),
                });
            }
        }
        Ok(path)
    }

    fn prepare_object_parent(&self, digest: ContentDigest) -> Result<PathBuf, CasError> {
        let path = self.object_path(digest);
        let parent = path.parent().expect("CAS object path has a parent");
        fs::create_dir_all(parent)
            .map_err(|source| io_error("create CAS object directory", parent, source))?;
        let metadata = fs::symlink_metadata(parent)
            .map_err(|source| io_error("inspect CAS object directory", parent, source))?;
        if metadata.file_type().is_symlink() || !metadata.file_type().is_dir() {
            return Err(CasError::SourcePathEscape {
                path: parent.to_owned(),
            });
        }
        let canonical_parent = fs::canonicalize(parent)
            .map_err(|source| io_error("canonicalize CAS object directory", parent, source))?;
        if !canonical_parent.starts_with(&self.objects_root) {
            return Err(CasError::SourcePathEscape {
                path: parent.to_owned(),
            });
        }
        Ok(canonical_parent)
    }

    fn new_temporary_path(&self) -> PathBuf {
        let mut rng = self
            .temp_suffixes
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let suffix = rng.u64(..);
        // A stale path from a prior crash is handled by `create_temporary`,
        // which advances this seeded source and retries. The suffix does not
        // carry authority or content identity.
        self.objects_root
            .join(format!(".tmp-{}-{suffix:016x}", std::process::id()))
    }

    fn create_temporary(&self) -> Result<(PathBuf, File), CasError> {
        loop {
            let path = self.new_temporary_path();
            match OpenOptions::new().write(true).create_new(true).open(&path) {
                Ok(file) => return Ok((path, file)),
                Err(source) if source.kind() == io::ErrorKind::AlreadyExists => continue,
                Err(source) => return Err(io_error("create CAS temporary object", &path, source)),
            }
        }
    }

    fn fail_after_copied_bytes(&self) -> Option<u64> {
        #[cfg(test)]
        {
            self.faults.fail_after_copied_bytes
        }
        #[cfg(not(test))]
        {
            None
        }
    }

    fn fail_temporary_fsync(&self) -> bool {
        #[cfg(test)]
        {
            self.faults.fail_temporary_fsync
        }
        #[cfg(not(test))]
        {
            false
        }
    }

    fn fail_install(&self) -> bool {
        #[cfg(test)]
        {
            self.faults.fail_install
        }
        #[cfg(not(test))]
        {
            false
        }
    }

    fn sync_directory(&self, path: &Path) -> Result<(), CasError> {
        #[cfg(test)]
        if self.faults.fail_directory_fsync {
            return Err(CasError::InjectedFault {
                operation: "CAS directory fsync",
            });
        }
        sync_directory(path)
    }
}

fn stream_to_temp(
    source: &mut File,
    destination: &mut File,
    source_path: &Path,
    initial_length: u64,
    initial_modified: Option<SystemTime>,
    maximum: u64,
    fail_after_copied_bytes: Option<u64>,
) -> Result<(ContentDigest, u64), CasError> {
    let mut hasher = blake3::Hasher::new();
    let mut byte_length = 0_u64;
    let mut buffer = [0_u8; 64 * 1024];

    loop {
        let count = source
            .read(&mut buffer)
            .map_err(|source| io_error("read staged file", source_path, source))?;
        if count == 0 {
            break;
        }
        let count = count as u64;
        let next_length = byte_length
            .checked_add(count)
            .ok_or(CasError::SizeLimitExceeded {
                path: source_path.to_owned(),
                maximum,
                observed: u64::MAX,
            })?;
        if next_length > maximum {
            return Err(CasError::SizeLimitExceeded {
                path: source_path.to_owned(),
                maximum,
                observed: next_length,
            });
        }
        destination
            .write_all(&buffer[..count as usize])
            .map_err(|source| io_error("write CAS temporary object", source_path, source))?;
        hasher.update(&buffer[..count as usize]);
        byte_length = next_length;
        if fail_after_copied_bytes.is_some_and(|limit| byte_length >= limit) {
            return Err(CasError::InjectedFault {
                operation: "staged-byte copy",
            });
        }
    }

    let final_length = source
        .metadata()
        .map_err(|source| io_error("restat staged file", source_path, source))?
        .len();
    let final_modified = source
        .metadata()
        .ok()
        .and_then(|metadata| metadata.modified().ok());
    if final_length != initial_length
        || final_length != byte_length
        || initial_modified != final_modified
    {
        return Err(CasError::SourceChanged {
            path: source_path.to_owned(),
        });
    }

    Ok((
        ContentDigest::from_bytes(*hasher.finalize().as_bytes()),
        byte_length,
    ))
}

fn validate_relative_source(path: &Path) -> Result<(), CasError> {
    if path.as_os_str().is_empty() {
        return Err(CasError::UnsafeSourcePath {
            path: path.to_owned(),
            reason: "path is empty",
        });
    }
    if path.is_absolute() {
        return Err(CasError::UnsafeSourcePath {
            path: path.to_owned(),
            reason: "path is absolute",
        });
    }
    for component in path.components() {
        match component {
            Component::Normal(_) => {}
            Component::CurDir => {
                return Err(CasError::UnsafeSourcePath {
                    path: path.to_owned(),
                    reason: "path contains '.'",
                });
            }
            Component::ParentDir => {
                return Err(CasError::UnsafeSourcePath {
                    path: path.to_owned(),
                    reason: "path contains '..'",
                });
            }
            Component::RootDir | Component::Prefix(_) => {
                return Err(CasError::UnsafeSourcePath {
                    path: path.to_owned(),
                    reason: "path is not a relative path",
                });
            }
        }
    }
    Ok(())
}

fn ensure_real_directory(path: &Path) -> Result<(), CasError> {
    let metadata =
        fs::symlink_metadata(path).map_err(|source| io_error("inspect directory", path, source))?;
    if metadata.file_type().is_symlink() || !metadata.file_type().is_dir() {
        return Err(CasError::InvalidDirectory {
            path: path.to_owned(),
            reason: "must be a real directory",
        });
    }
    Ok(())
}

fn sync_directory(path: &Path) -> Result<(), CasError> {
    let directory = File::open(path)
        .map_err(|source| io_error("open CAS directory for fsync", path, source))?;
    directory
        .sync_all()
        .map_err(|source| io_error("fsync CAS directory", path, source))
}

fn open_source_file(path: &Path) -> io::Result<File> {
    #[cfg(unix)]
    {
        // The final component was checked above, but rustix's O_NOFOLLOW
        // closes the check/open race if a cooperative same-user process swaps
        // it before the descriptor is acquired. The owned descriptor converts
        // into `File` without an unsafe raw-fd boundary.
        let descriptor = rustix::fs::open(
            path,
            rustix::fs::OFlags::RDONLY | rustix::fs::OFlags::NOFOLLOW | rustix::fs::OFlags::CLOEXEC,
            rustix::fs::Mode::empty(),
        )?;
        Ok(File::from(descriptor))
    }
    #[cfg(not(unix))]
    {
        File::open(path)
    }
}

enum OpenError {
    Missing,
    NotRegular,
    Io(io::Error),
}

fn open_regular_file(path: &Path) -> Result<File, OpenError> {
    let metadata = match fs::symlink_metadata(path) {
        Ok(metadata) => metadata,
        Err(source) if source.kind() == io::ErrorKind::NotFound => return Err(OpenError::Missing),
        Err(source) => return Err(OpenError::Io(source)),
    };
    if metadata.file_type().is_symlink() || !metadata.file_type().is_file() {
        return Err(OpenError::NotRegular);
    }
    open_source_file(path).map_err(OpenError::Io)
}

fn io_error(operation: &'static str, path: &Path, source: io::Error) -> CasError {
    CasError::Io {
        operation,
        path: path.to_owned(),
        source,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicU64, Ordering};

    static NEXT_ROOT: AtomicU64 = AtomicU64::new(1);

    struct TestRoot(PathBuf);

    impl TestRoot {
        fn new() -> Self {
            let path = std::env::temp_dir().join(format!(
                "factory-kernel-cas-unit-{}-{}",
                std::process::id(),
                NEXT_ROOT.fetch_add(1, Ordering::Relaxed)
            ));
            fs::create_dir(&path).expect("unique CAS unit root");
            Self(path)
        }
    }

    impl Drop for TestRoot {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    #[test]
    fn kernel_generated_bytes_use_append_only_custody() {
        let root = TestRoot::new();
        let cas = CasStore::new_with_seed(root.0.join("runtime"), 1024, 9).expect("CAS");
        let first = cas
            .adopt_kernel_bytes(b"canonical kernel receipt")
            .expect("first adoption");
        let second = cas
            .adopt_kernel_bytes(b"canonical kernel receipt")
            .expect("idempotent physical adoption");
        assert_eq!(first, second);
        assert_eq!(
            cas.read_verified(first.digest()).expect("verified bytes"),
            b"canonical kernel receipt"
        );
    }

    #[test]
    fn physical_faults_leave_only_safe_orphans() {
        for (seed, faults, operation) in [
            (
                1,
                CasFaultPlan {
                    fail_after_copied_bytes: Some(1),
                    ..CasFaultPlan::default()
                },
                "staged-byte copy",
            ),
            (
                2,
                CasFaultPlan {
                    fail_temporary_fsync: true,
                    ..CasFaultPlan::default()
                },
                "temporary fsync",
            ),
            (
                3,
                CasFaultPlan {
                    fail_install: true,
                    ..CasFaultPlan::default()
                },
                "CAS install",
            ),
        ] {
            let root = TestRoot::new();
            let staging = root.0.join("staging");
            fs::create_dir(&staging).expect("staging");
            fs::write(staging.join("packet"), vec![b'x'; 128 * 1024]).expect("source");
            let cas = CasStore::new_with_seed_and_faults(
                root.0.join("runtime"),
                256 * 1024,
                seed,
                faults,
            )
            .expect("CAS");
            assert!(matches!(
                cas.adopt(&staging, "packet"),
                Err(CasError::InjectedFault { operation: observed }) if observed == operation
            ));
            let objects = root.0.join("runtime/objects/blake3");
            assert!(
                fs::read_dir(objects)
                    .expect("objects root")
                    .filter_map(Result::ok)
                    .all(|entry| !entry.path().is_file())
            );
        }
    }

    #[test]
    fn directory_fsync_failure_publishes_a_verifiable_orphan() {
        let root = TestRoot::new();
        let staging = root.0.join("staging");
        fs::create_dir(&staging).expect("staging");
        fs::write(staging.join("packet"), b"directory fsync fault").expect("source");
        let cas = CasStore::new_with_seed_and_faults(
            root.0.join("runtime"),
            1024,
            4,
            CasFaultPlan {
                fail_directory_fsync: true,
                ..CasFaultPlan::default()
            },
        )
        .expect("CAS");
        let digest = ContentDigest::of_bytes(b"directory fsync fault");
        assert!(matches!(
            cas.adopt(&staging, "packet"),
            Err(CasError::InjectedFault {
                operation: "CAS directory fsync"
            })
        ));
        assert_eq!(
            cas.read_verified(digest).expect("safe orphan"),
            b"directory fsync fault"
        );
    }

    #[test]
    fn verified_read_rejects_same_length_mutation() {
        let root = TestRoot::new();
        let staging = root.0.join("staging");
        fs::create_dir(&staging).expect("staging");
        fs::write(staging.join("packet"), b"before").expect("source");
        let cas = CasStore::new_with_seed(root.0.join("runtime"), 1024, 5).expect("CAS");
        let sealed = cas.adopt(&staging, "packet").expect("adopt");
        fs::write(cas.object_path(sealed.digest()), b"mutate").expect("mutation");
        assert!(matches!(
            cas.read_verified(sealed.digest()),
            Err(CasError::CorruptObject { .. })
        ));
    }
}
