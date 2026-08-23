//! Installed Rust host and Tea source identity at the
//! session-launch boundary.
//!
//! The assignment packet names qualified runtime facts, but a packet is not
//! proof that those bytes still exist on this host. This module retains a
//! finite installed manifest and rechecks it immediately before a host is
//! spawned. The host is one exact Rust executable; its dependencies are Cargo
//! source material from the hard-coded local Tea source checkout.
//! Qualification refuses a dirty checkout and records its exact `HEAD` plus a
//! deterministic source inventory. No interpreter, ambient home, or provider
//! request is involved.

use std::{
    collections::BTreeSet,
    env,
    ffi::{OsStr, OsString},
    fs,
    io::{self, Read},
    os::fd::RawFd,
    path::{Path, PathBuf},
    process::Command,
    sync::Arc,
};

use factory_protocol::{
    AbsoluteHostPath, AssignmentPacketV2, ContentDigest, CredentialDescriptorV2, KernelBuildId,
    RuntimeIdentityV2, RuntimeRelativePath, parse_assignment_packet_v2,
    unsigned_assignment_packet_digest_v2,
};
use factory_settings::{
    MAX_HOST_SOURCE_GRAPH_FILES, MAX_RECEIPT_BYTES, MAX_SOURCE_GRAPH_FILES,
    MAX_VERSION_OUTPUT_BYTES, OPENROUTER_PROVIDER, RUST_HOST_IDENTITY, RUST_TOOLCHAIN,
    TEA_HEAD_MAX_BYTES, TEA_SOURCE,
};
use thiserror::Error;

use crate::{
    command_supervision::{
        ApprovedToolExecutables, CommandRunner, CommandSupervisionError, DEFAULT_TERMINATION_GRACE,
        ExactExecutable,
    },
    git::{GitCustody, GitCustodyError},
    process_custody::{ProcessCustodyError, TeaHostSpawnSpec},
    session_runtime::{RuntimeVerificationError, SessionRuntimeVerifier},
};

/// The Tea host root is intentionally small and closed. Qualification inventories
/// every local file below it, so a future local import cannot be omitted from
/// the packet-visible source graph.
const SOURCE_GRAPH_DOMAIN: &[u8] = b"factory-v3-installed-source-graph-v1\0";
const TEA_SOURCE_DOMAIN: &[u8] = b"factory-v3-tea-source-v1\0";
const KERNEL_SOURCE_GRAPH_DOMAIN: &[u8] = b"factory-v3-kernel-source-graph-v1\0";
const KERNEL_BUILD_DOMAIN: &[u8] = b"factory-v3-kernel-build-v1\0";
const INSTALLED_BUILD_RECEIPT_DOMAIN: &[u8] = b"factory-v3-installed-build-receipt-rust-host-v1\0";
/// Explicit installation inputs for one immutable Rust agent runtime.
///
/// This is kernel/operator input during a stopped-daemon deployment, never an
/// actor wire request. `host_source_files` must enumerate the complete regular
/// file inventory under `host_source_root`; an omitted local import is a
/// qualification error. The executable digest and exact local core checkout
/// are the complete runtime identity; no startup probe or ambient package
/// metadata is consulted.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct InstalledRuntimeQualification {
    /// Exact `factory-tea-host` executable selected by stopped-daemon
    /// installation. The path must resolve to a regular executable file.
    pub host_executable: PathBuf,
    pub host_source_root: PathBuf,
    pub host_source_files: Vec<RuntimeRelativePath>,
}

/// Exact local source identity of Tea. The checkout is a
/// temporary bootstrap provenance mechanism and is intentionally fixed to one
/// absolute path until the project is published and can be pinned normally.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TeaQualification {
    root: PathBuf,
    head: String,
    files: Vec<InstalledSourceFile>,
    source_digest: ContentDigest,
}

impl TeaQualification {
    #[must_use]
    pub fn root(&self) -> &Path {
        &self.root
    }

    #[must_use]
    pub fn head(&self) -> &str {
        &self.head
    }

    #[must_use]
    pub fn files(&self) -> &[InstalledSourceFile] {
        &self.files
    }

    #[must_use]
    pub const fn source_digest(&self) -> ContentDigest {
        self.source_digest
    }
}

/// One exact local source file accepted by installation qualification.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct InstalledSourceFile {
    relative_path: RuntimeRelativePath,
    digest: ContentDigest,
}

impl InstalledSourceFile {
    #[must_use]
    pub fn relative_path(&self) -> &RuntimeRelativePath {
        &self.relative_path
    }

    #[must_use]
    pub const fn digest(&self) -> ContentDigest {
        self.digest
    }
}

/// Exact host identity streams retained during qualification.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct HostIdentityOutput {
    stdout: Vec<u8>,
    stderr: Vec<u8>,
}

impl HostIdentityOutput {
    #[must_use]
    pub fn stdout(&self) -> &[u8] {
        &self.stdout
    }

    #[must_use]
    pub fn stderr(&self) -> &[u8] {
        &self.stderr
    }
}

/// One closed, digest-qualified kernel source-file observation retained with
/// an installed build. It is deliberately not a generic file manifest.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct KernelSourceReceiptFileV2 {
    relative_path: RuntimeRelativePath,
    digest: ContentDigest,
}

impl KernelSourceReceiptFileV2 {
    pub fn new(
        relative_path: RuntimeRelativePath,
        digest: ContentDigest,
    ) -> Result<Self, InstalledRuntimeError> {
        if relative_path.as_str().len() > 4_096 {
            return Err(InstalledRuntimeError::ReceiptInvalid {
                reason: "kernel source path exceeds the receipt bound",
            });
        }
        Ok(Self {
            relative_path,
            digest,
        })
    }

    #[must_use]
    pub fn relative_path(&self) -> &RuntimeRelativePath {
        &self.relative_path
    }

    #[must_use]
    pub const fn digest(&self) -> ContentDigest {
        self.digest
    }
}

/// Closed source-graph qualification for the installed Rust kernel. This
/// shared kernel operation makes the exact build digest reproducible without
/// granting a daemon entrypoint direct hashing or CAS authority.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct KernelSourceQualificationV2 {
    source_root: PathBuf,
    files: Vec<KernelSourceReceiptFileV2>,
    digest: ContentDigest,
}

impl KernelSourceQualificationV2 {
    #[must_use]
    pub fn source_root(&self) -> &Path {
        &self.source_root
    }

    #[must_use]
    pub fn files(&self) -> &[KernelSourceReceiptFileV2] {
        &self.files
    }

    #[must_use]
    pub const fn digest(&self) -> ContentDigest {
        self.digest
    }
}

/// Qualifies precisely the source paths supplied by an offline installer.
/// Paths are safe-relative at the protocol boundary; this operation proves
/// their canonical bytes remain beneath the explicit source root.
pub fn qualify_kernel_source_v2(
    source_root: &Path,
    source_paths: &[RuntimeRelativePath],
) -> Result<KernelSourceQualificationV2, InstalledRuntimeError> {
    if source_paths.is_empty() || source_paths.len() > MAX_SOURCE_GRAPH_FILES {
        return Err(InstalledRuntimeError::ReceiptInvalid {
            reason: "kernel source graph count is invalid",
        });
    }
    let source_root = canonical_directory("kernel source root", source_root)?;
    let mut files = Vec::with_capacity(source_paths.len());
    for relative_path in source_paths {
        let canonical = canonical_regular_file(
            "kernel source file",
            &source_root.join(relative_path.as_str()),
        )?;
        if !canonical.starts_with(&source_root) {
            return Err(InstalledRuntimeError::SourceFileOutsideRoot {
                path: relative_path.as_str().to_owned(),
            });
        }
        files.push(KernelSourceReceiptFileV2::new(
            relative_path.clone(),
            digest_regular_file("kernel source file", &canonical)?,
        )?);
    }
    let files = normalize_kernel_source_files(files)?;
    let digest = kernel_source_graph_digest(&files)?;
    Ok(KernelSourceQualificationV2 {
        source_root,
        files,
        digest,
    })
}

/// Canonical executable and content identity of the exact installed Rust
/// kernel binary. It has no process-spawning behavior.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct KernelBinaryQualificationV2 {
    path: PathBuf,
    digest: ContentDigest,
}

/// Canonical executable and content identity for one closed kernel tool.
/// Cargo and Git are installed build material, never application-selected
/// executable paths. Cargo's compiler/documentation companions are derived
/// from Cargo's canonical directory and qualified alongside it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct InstalledExecutableQualificationV2 {
    path: PathBuf,
    digest: ContentDigest,
}

impl InstalledExecutableQualificationV2 {
    #[must_use]
    pub fn path(&self) -> &Path {
        &self.path
    }

    #[must_use]
    pub const fn digest(&self) -> ContentDigest {
        self.digest
    }
}

/// Qualifies a direct executable selected by stopped-daemon installation.
/// The label is closed at the callsite and exists only to make drift evidence
/// useful; it never becomes application or actor-controlled metadata.
pub fn qualify_installed_executable_v2(
    field: &'static str,
    path: &Path,
) -> Result<InstalledExecutableQualificationV2, InstalledRuntimeError> {
    let path = canonical_executable_file(field, path)?;
    let digest = digest_regular_file(field, &path)?;
    Ok(InstalledExecutableQualificationV2 { path, digest })
}

/// Closed qualification for the deterministic command/Git executables.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct InstalledApprovedToolsQualificationV2 {
    cargo: InstalledExecutableQualificationV2,
    cargo_rustc: InstalledExecutableQualificationV2,
    cargo_rustdoc: InstalledExecutableQualificationV2,
    git: InstalledExecutableQualificationV2,
}

impl InstalledApprovedToolsQualificationV2 {
    pub fn qualify(cargo: &Path, git: &Path) -> Result<Self, InstalledRuntimeError> {
        let cargo = qualify_installed_executable_v2("Cargo executable", cargo)?;
        let cargo_parent = cargo
            .path
            .parent()
            .ok_or(InstalledRuntimeError::ReceiptInvalid {
                reason: "Cargo executable has no parent directory",
            })?;
        let cargo_rustc = qualify_installed_executable_v2(
            "Cargo Rust compiler executable",
            &cargo_parent.join("rustc"),
        )?;
        let cargo_rustdoc = qualify_installed_executable_v2(
            "Cargo Rust documentation executable",
            &cargo_parent.join("rustdoc"),
        )?;
        let git = qualify_installed_executable_v2("Git executable", git)?;
        Ok(Self {
            cargo,
            cargo_rustc,
            cargo_rustdoc,
            git,
        })
    }

    fn identity_digest(&self) -> ContentDigest {
        let mut bytes = b"factory-v3-installed-approved-tools-v1\0".to_vec();
        for executable in [
            &self.cargo,
            &self.cargo_rustc,
            &self.cargo_rustdoc,
            &self.git,
        ] {
            bytes.extend_from_slice(executable.path.as_os_str().as_encoded_bytes());
            bytes.push(0);
            bytes.extend_from_slice(&executable.digest.as_bytes());
        }
        ContentDigest::of_bytes(&bytes)
    }

    fn encode_receipt_fields(&self, bytes: &mut Vec<u8>) -> Result<(), InstalledRuntimeError> {
        for executable in [
            &self.cargo,
            &self.cargo_rustc,
            &self.cargo_rustdoc,
            &self.git,
        ] {
            append_receipt_path(bytes, &executable.path)?;
            append_receipt_bytes(bytes, &executable.digest.as_bytes())?;
        }
        Ok(())
    }

    fn decode_receipt_fields(
        cursor: &mut ReceiptCursor<'_>,
    ) -> Result<Self, InstalledRuntimeError> {
        let mut read = |field: &'static str| -> Result<InstalledExecutableQualificationV2, InstalledRuntimeError> {
            Ok(InstalledExecutableQualificationV2 {
                path: cursor.absolute_path(field)?,
                digest: cursor.digest()?,
            })
        };
        Ok(Self {
            cargo: read("Cargo executable")?,
            cargo_rustc: read("Cargo Rust compiler executable")?,
            cargo_rustdoc: read("Cargo Rust documentation executable")?,
            git: read("Git executable")?,
        })
    }

    fn verify_installed_material(&self) -> Result<(), InstalledRuntimeError> {
        for (field, executable) in [
            ("Cargo executable", &self.cargo),
            ("Cargo Rust compiler executable", &self.cargo_rustc),
            ("Cargo Rust documentation executable", &self.cargo_rustdoc),
            ("Git executable", &self.git),
        ] {
            if canonical_executable_file(field, &executable.path)? != executable.path
                || digest_regular_file(field, &executable.path)? != executable.digest
            {
                return Err(InstalledRuntimeError::RuntimeDrift {
                    evidence: "installed approved executable identity changed",
                });
            }
        }
        let parent = self
            .cargo
            .path
            .parent()
            .ok_or(InstalledRuntimeError::RuntimeDrift {
                evidence: "Cargo executable no longer has a parent directory",
            })?;
        if parent.join("rustc") != self.cargo_rustc.path
            || parent.join("rustdoc") != self.cargo_rustdoc.path
        {
            return Err(InstalledRuntimeError::RuntimeDrift {
                evidence: "Cargo toolchain companion path changed",
            });
        }
        Ok(())
    }

    fn actor_tool_path(&self) -> Result<OsString, InstalledRuntimeError> {
        let cargo = self
            .cargo
            .path
            .parent()
            .ok_or(InstalledRuntimeError::ReceiptInvalid {
                reason: "Cargo executable has no parent directory",
            })?;
        let git = self
            .git
            .path
            .parent()
            .ok_or(InstalledRuntimeError::ReceiptInvalid {
                reason: "Git executable has no parent directory",
            })?;
        let mut directories = Vec::new();
        for directory in [cargo, git, Path::new("/usr/bin"), Path::new("/bin")] {
            if !directories.contains(&directory) {
                directories.push(directory);
            }
        }
        env::join_paths(directories).map_err(|_| InstalledRuntimeError::ReceiptInvalid {
            reason: "installed approved-tool path cannot be represented",
        })
    }

    fn command_runner(&self) -> Result<CommandRunner, InstalledRuntimeError> {
        self.verify_installed_material()?;
        let tools = ApprovedToolExecutables::new(
            ExactExecutable::discover(&self.cargo.path)?,
            ExactExecutable::discover(&self.git.path)?,
        );
        CommandRunner::new(tools, DEFAULT_TERMINATION_GRACE).map_err(Into::into)
    }

    fn git_custody(&self, runtime_root: &Path) -> Result<Arc<GitCustody>, InstalledRuntimeError> {
        self.verify_installed_material()?;
        Ok(Arc::new(GitCustody::new(&self.git.path, runtime_root)?))
    }
}

/// Exact daemon-owned deterministic execution services reconstructed only
/// from a requalified installed-build receipt.
#[derive(Clone)]
pub struct InstalledKernelExecutionTools {
    command_runner: CommandRunner,
    git_custody: Arc<GitCustody>,
}

impl InstalledKernelExecutionTools {
    #[must_use]
    pub fn command_runner(&self) -> &CommandRunner {
        &self.command_runner
    }

    #[must_use]
    pub fn git_custody(&self) -> Arc<GitCustody> {
        Arc::clone(&self.git_custody)
    }
}

impl KernelBinaryQualificationV2 {
    #[must_use]
    pub fn path(&self) -> &Path {
        &self.path
    }

    #[must_use]
    pub const fn digest(&self) -> ContentDigest {
        self.digest
    }
}

pub fn qualify_kernel_binary_v2(
    path: &Path,
) -> Result<KernelBinaryQualificationV2, InstalledRuntimeError> {
    let path = canonical_regular_file("kernel binary", path)?;
    let digest = digest_regular_file("kernel binary", &path)?;
    Ok(KernelBinaryQualificationV2 { path, digest })
}

/// Versioned, closed installed-build provenance sealed as the one kernel
/// qualification artifact. PostgreSQL keeps its immutable digest/path seal;
/// this value retains the exact local facts necessary to rebuild a typed
/// runtime identity after daemon restart without a new table or open map.
///
/// The only MVP provider source is OpenRouter's named environment variable.
/// This receipt never contains its value, an OAuth token, or any other secret.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct InstalledKernelBuildReceiptV2 {
    kernel_build_id: KernelBuildId,
    schema_identity: String,
    kernel_source_root: PathBuf,
    kernel_source_files: Vec<KernelSourceReceiptFileV2>,
    kernel_source_digest: ContentDigest,
    kernel_binary: PathBuf,
    kernel_binary_digest: ContentDigest,
    approved_tools: InstalledApprovedToolsQualificationV2,
    runtime: InstalledRuntimeManifest,
    openrouter_credential_environment: String,
}

impl InstalledKernelBuildReceiptV2 {
    /// Constructs one installed-build receipt from the two closed
    /// qualifications produced by this module. Daemon entrypoints should use
    /// this operation instead of hashing source or binary bytes themselves.
    pub fn from_qualifications(
        schema_identity: String,
        source: KernelSourceQualificationV2,
        binary: KernelBinaryQualificationV2,
        approved_tools: InstalledApprovedToolsQualificationV2,
        runtime: InstalledRuntimeManifest,
        openrouter_credential_environment: String,
    ) -> Result<Self, InstalledRuntimeError> {
        Self::qualify(
            schema_identity,
            source.source_root,
            source.files,
            source.digest,
            binary.path,
            binary.digest,
            approved_tools,
            runtime,
            openrouter_credential_environment,
        )
    }

    #[allow(clippy::too_many_arguments)]
    pub fn qualify(
        schema_identity: String,
        kernel_source_root: PathBuf,
        kernel_source_files: Vec<KernelSourceReceiptFileV2>,
        kernel_source_digest: ContentDigest,
        kernel_binary: PathBuf,
        kernel_binary_digest: ContentDigest,
        approved_tools: InstalledApprovedToolsQualificationV2,
        runtime: InstalledRuntimeManifest,
        openrouter_credential_environment: String,
    ) -> Result<Self, InstalledRuntimeError> {
        let kernel_build_id = KernelBuildId::new(kernel_build_digest(
            kernel_source_digest,
            kernel_binary_digest,
            approved_tools.identity_digest(),
            runtime.identity_digest()?,
            &schema_identity,
            &openrouter_credential_environment,
        )?);
        Self::new(
            kernel_build_id,
            schema_identity,
            kernel_source_root,
            kernel_source_files,
            kernel_source_digest,
            kernel_binary,
            kernel_binary_digest,
            approved_tools,
            runtime,
            openrouter_credential_environment,
        )
    }

    #[allow(clippy::too_many_arguments)]
    pub fn new(
        kernel_build_id: KernelBuildId,
        schema_identity: String,
        kernel_source_root: PathBuf,
        kernel_source_files: Vec<KernelSourceReceiptFileV2>,
        kernel_source_digest: ContentDigest,
        kernel_binary: PathBuf,
        kernel_binary_digest: ContentDigest,
        approved_tools: InstalledApprovedToolsQualificationV2,
        runtime: InstalledRuntimeManifest,
        openrouter_credential_environment: String,
    ) -> Result<Self, InstalledRuntimeError> {
        validate_absolute_receipt_path("kernel source root", &kernel_source_root)?;
        validate_absolute_receipt_path("kernel binary", &kernel_binary)?;
        validate_text("schema identity", &schema_identity)?;
        CredentialDescriptorV2::Environment {
            name: openrouter_credential_environment.clone(),
        }
        .validate()
        .map_err(|_| InstalledRuntimeError::ReceiptInvalid {
            reason: "OpenRouter credential environment name is invalid",
        })?;
        if matches!(
            openrouter_credential_environment.as_str(),
            "NO_COLOR" | "PATH"
        ) {
            return Err(InstalledRuntimeError::ReceiptInvalid {
                reason: "credential environment name is reserved by kernel process custody",
            });
        }
        let kernel_source_files = normalize_kernel_source_files(kernel_source_files)?;
        if kernel_source_graph_digest(&kernel_source_files)? != kernel_source_digest {
            return Err(InstalledRuntimeError::ReceiptInvalid {
                reason: "kernel source graph digest does not match receipt files",
            });
        }
        let expected = kernel_build_digest(
            kernel_source_digest,
            kernel_binary_digest,
            approved_tools.identity_digest(),
            runtime.identity_digest()?,
            &schema_identity,
            &openrouter_credential_environment,
        )?;
        if kernel_build_id.digest() != expected {
            return Err(InstalledRuntimeError::ReceiptInvalid {
                reason: "kernel build ID does not match qualified receipt material",
            });
        }
        let receipt = Self {
            kernel_build_id,
            schema_identity,
            kernel_source_root,
            kernel_source_files,
            kernel_source_digest,
            kernel_binary,
            kernel_binary_digest,
            approved_tools,
            runtime,
            openrouter_credential_environment,
        };
        // Reject an oversized closed graph during qualification, before it
        // becomes a durable build candidate or reaches CAS.
        let _ = receipt.encode()?;
        Ok(receipt)
    }

    #[must_use]
    pub const fn kernel_build_id(&self) -> KernelBuildId {
        self.kernel_build_id
    }

    #[must_use]
    pub fn runtime(&self) -> &InstalledRuntimeManifest {
        &self.runtime
    }

    #[must_use]
    pub fn schema_identity(&self) -> &str {
        &self.schema_identity
    }

    /// Canonical binary whose bytes were included in this build identity.
    #[must_use]
    pub fn kernel_binary(&self) -> &Path {
        &self.kernel_binary
    }

    #[must_use]
    pub const fn kernel_source_digest(&self) -> ContentDigest {
        self.kernel_source_digest
    }

    #[must_use]
    pub const fn kernel_binary_digest(&self) -> ContentDigest {
        self.kernel_binary_digest
    }

    /// Reconstructs the only approved deterministic command and Git custody
    /// services. Their executable paths are sealed build material; callers
    /// provide only the kernel-owned Git runtime directory.
    pub fn execution_tools(
        &self,
        git_runtime_root: &Path,
    ) -> Result<InstalledKernelExecutionTools, InstalledRuntimeError> {
        self.approved_tools.verify_installed_material()?;
        Ok(InstalledKernelExecutionTools {
            command_runner: self.approved_tools.command_runner()?,
            git_custody: self.approved_tools.git_custody(git_runtime_root)?,
        })
    }

    #[must_use]
    pub fn openrouter_credential_environment(&self) -> &str {
        &self.openrouter_credential_environment
    }

    /// Builds the assignment-packet runtime identity only for the one
    /// credential provider admitted by this MVP. The returned descriptor is a
    /// configuration name; its Vault-resolved value is never part of runtime
    /// identity or durable assignment data.
    pub fn runtime_identity_for_provider(
        &self,
        provider: &str,
    ) -> Result<RuntimeIdentityV2, InstalledRuntimeError> {
        if provider != OPENROUTER_PROVIDER {
            return Err(InstalledRuntimeError::UnsupportedCredentialProvider {
                provider: provider.to_owned(),
            });
        }
        self.runtime
            .runtime_identity(CredentialDescriptorV2::Environment {
                name: self.openrouter_credential_environment.clone(),
            })
    }

    /// Builds the exact Rust host process contract for the installed
    /// runtime. `credential_environment` comes from a Vault-backed resolver
    /// only at spawn time; this receipt compares its name with configuration
    /// but never reads, stores, logs, or returns its value.
    pub fn tea_host_spawn_spec_for_provider(
        &self,
        provider: &str,
        working_directory: PathBuf,
        actor_source_fd: RawFd,
        credential_environment: (OsString, OsString),
    ) -> Result<TeaHostSpawnSpec, InstalledRuntimeError> {
        let _ = self.runtime_identity_for_provider(provider)?;
        if credential_environment.0 != OsStr::new(&self.openrouter_credential_environment) {
            return Err(InstalledRuntimeError::CredentialEnvironmentMismatch);
        }
        if credential_environment.1.is_empty() {
            return Err(InstalledRuntimeError::CredentialEnvironmentMissing);
        }
        let spawn = TeaHostSpawnSpec::new_for_assignment(
            self.runtime.host_executable.clone(),
            working_directory,
            actor_source_fd,
            vec![credential_environment],
        )?;
        Ok(spawn.with_kernel_tool_path(self.approved_tools.actor_tool_path()?)?)
    }

    /// Encodes a bounded canonical receipt. This wire format is intentionally
    /// private to the trusted kernel, versioned by its fixed domain, and
    /// contains no extensible JSON/metadata section.
    pub fn encode(&self) -> Result<Vec<u8>, InstalledRuntimeError> {
        let mut bytes = Vec::new();
        append_receipt_bytes(&mut bytes, INSTALLED_BUILD_RECEIPT_DOMAIN)?;
        append_receipt_bytes(&mut bytes, &self.kernel_build_id.digest().as_bytes())?;
        append_receipt_text(&mut bytes, &self.schema_identity)?;
        append_receipt_path(&mut bytes, &self.kernel_source_root)?;
        append_receipt_bytes(&mut bytes, &self.kernel_source_digest.as_bytes())?;
        append_receipt_u32(
            &mut bytes,
            u32::try_from(self.kernel_source_files.len()).map_err(|_| {
                InstalledRuntimeError::ReceiptInvalid {
                    reason: "kernel source graph exceeds receipt bound",
                }
            })?,
        );
        for file in &self.kernel_source_files {
            append_receipt_text(&mut bytes, file.relative_path.as_str())?;
            append_receipt_bytes(&mut bytes, &file.digest.as_bytes())?;
        }
        append_receipt_path(&mut bytes, &self.kernel_binary)?;
        append_receipt_bytes(&mut bytes, &self.kernel_binary_digest.as_bytes())?;
        self.approved_tools.encode_receipt_fields(&mut bytes)?;
        self.runtime.encode_receipt_fields(&mut bytes)?;
        append_receipt_text(&mut bytes, &self.openrouter_credential_environment)?;
        if bytes.len() > MAX_RECEIPT_BYTES {
            return Err(InstalledRuntimeError::ReceiptInvalid {
                reason: "installed build receipt exceeds byte bound",
            });
        }
        Ok(bytes)
    }

    /// Decodes only the closed receipt spelling emitted by [`Self::encode`].
    /// It proves every digest relationship before exposing the manifest; the
    /// caller must still invoke [`Self::verify_installed_material`] to detect
    /// post-install filesystem drift.
    pub fn decode(bytes: &[u8]) -> Result<Self, InstalledRuntimeError> {
        if bytes.len() > MAX_RECEIPT_BYTES {
            return Err(InstalledRuntimeError::ReceiptInvalid {
                reason: "installed build receipt exceeds byte bound",
            });
        }
        let mut cursor = ReceiptCursor::new(bytes);
        cursor.expect_bytes(INSTALLED_BUILD_RECEIPT_DOMAIN)?;
        let kernel_build_id = KernelBuildId::new(cursor.digest()?);
        let schema_identity = cursor.text("schema identity")?;
        let kernel_source_root = cursor.absolute_path("kernel source root")?;
        let kernel_source_digest = cursor.digest()?;
        let source_count =
            usize::try_from(cursor.u32()?).map_err(|_| InstalledRuntimeError::ReceiptInvalid {
                reason: "kernel source count cannot be represented",
            })?;
        if source_count == 0 || source_count > MAX_SOURCE_GRAPH_FILES {
            return Err(InstalledRuntimeError::ReceiptInvalid {
                reason: "kernel source graph count is invalid",
            });
        }
        let mut kernel_source_files = Vec::with_capacity(source_count);
        for _ in 0..source_count {
            let relative_path =
                RuntimeRelativePath::parse(cursor.bounded_text("kernel source file", 4_096)?)
                    .map_err(|_| InstalledRuntimeError::ReceiptInvalid {
                        reason: "kernel source file path is invalid",
                    })?;
            kernel_source_files.push(KernelSourceReceiptFileV2::new(
                relative_path,
                cursor.digest()?,
            )?);
        }
        let kernel_binary = cursor.absolute_path("kernel binary")?;
        let kernel_binary_digest = cursor.digest()?;
        let approved_tools =
            InstalledApprovedToolsQualificationV2::decode_receipt_fields(&mut cursor)?;
        let runtime = InstalledRuntimeManifest::decode_receipt_fields(&mut cursor)?;
        let openrouter_credential_environment = cursor.text("OpenRouter credential environment")?;
        if !cursor.is_finished() {
            return Err(InstalledRuntimeError::ReceiptInvalid {
                reason: "installed build receipt has trailing bytes",
            });
        }
        Self::new(
            kernel_build_id,
            schema_identity,
            kernel_source_root,
            kernel_source_files,
            kernel_source_digest,
            kernel_binary,
            kernel_binary_digest,
            approved_tools,
            runtime,
            openrouter_credential_environment,
        )
    }

    /// Rechecks every recorded build/runtime input at daemon startup before a
    /// later assignment can reuse the receipt. It has no provider request,
    /// database write, or credential read path.
    pub fn verify_installed_material(
        &self,
        expected_schema_identity: &str,
    ) -> Result<(), InstalledRuntimeError> {
        if self.schema_identity != expected_schema_identity {
            return Err(InstalledRuntimeError::ReceiptInvalid {
                reason: "installed build schema identity differs from this kernel",
            });
        }
        let source_root = canonical_directory("kernel source root", &self.kernel_source_root)?;
        if source_root != self.kernel_source_root {
            return Err(InstalledRuntimeError::RuntimeDrift {
                evidence: "canonical kernel source-root path changed",
            });
        }
        let mut observed_files = Vec::with_capacity(self.kernel_source_files.len());
        for file in &self.kernel_source_files {
            let canonical = canonical_regular_file(
                "kernel source file",
                &source_root.join(file.relative_path.as_str()),
            )?;
            if !canonical.starts_with(&source_root) {
                return Err(InstalledRuntimeError::RuntimeDrift {
                    evidence: "kernel source file resolves outside source root",
                });
            }
            observed_files.push(KernelSourceReceiptFileV2::new(
                file.relative_path.clone(),
                digest_regular_file("kernel source file", &canonical)?,
            )?);
        }
        let observed_files = normalize_kernel_source_files(observed_files)?;
        if observed_files != self.kernel_source_files
            || kernel_source_graph_digest(&observed_files)? != self.kernel_source_digest
        {
            return Err(InstalledRuntimeError::RuntimeDrift {
                evidence: "kernel source graph digest changed",
            });
        }
        if canonical_regular_file("kernel binary", &self.kernel_binary)? != self.kernel_binary
            || digest_regular_file("kernel binary", &self.kernel_binary)?
                != self.kernel_binary_digest
        {
            return Err(InstalledRuntimeError::RuntimeDrift {
                evidence: "kernel binary identity changed",
            });
        }
        self.approved_tools.verify_installed_material()?;
        if self.kernel_build_id.digest()
            != kernel_build_digest(
                self.kernel_source_digest,
                self.kernel_binary_digest,
                self.approved_tools.identity_digest(),
                self.runtime.identity_digest()?,
                &self.schema_identity,
                &self.openrouter_credential_environment,
            )?
        {
            return Err(InstalledRuntimeError::RuntimeDrift {
                evidence: "kernel build identity changed",
            });
        }
        self.runtime.verify_installed_material()
    }
}

impl SessionRuntimeVerifier for InstalledKernelBuildReceiptV2 {
    fn verify_packet(
        &self,
        packet: &AssignmentPacketV2,
        canonical_packet_bytes: &[u8],
    ) -> Result<(), RuntimeVerificationError> {
        self.runtime
            .verify_packet_bytes(packet, canonical_packet_bytes)
    }

    fn verify_runtime(
        &self,
        packet: &AssignmentPacketV2,
        spawn: &TeaHostSpawnSpec,
    ) -> Result<(), RuntimeVerificationError> {
        self.runtime
            .verify_installed_material()
            .and_then(|()| {
                if packet.runtime.credential_env != self.openrouter_credential_environment {
                    return Err(InstalledRuntimeError::RuntimeDrift {
                        evidence: "assignment packet credential environment is not the installed provider configuration",
                    });
                }
                self.runtime.verify_runtime_identity(packet, spawn)
            })
            .map_err(|error| RuntimeVerificationError::RuntimeIdentity(error.to_string()))
    }
}

/// Closed installed runtime manifest retained by the daemon's build authority.
///
/// It has no open metadata map: each field supports one launch invariant. Its
/// source graph identity is the checked receipt digest carried in the
/// assignment packet.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct InstalledRuntimeManifest {
    host_executable: PathBuf,
    host_identity: String,
    host_identity_output: HostIdentityOutput,
    host_source_root: PathBuf,
    source_files: Vec<InstalledSourceFile>,
    source_graph_digest: ContentDigest,
    host_binary_digest: ContentDigest,
    core_lock_digest: ContentDigest,
    /// Exact local Tea checkout qualification. This is part of
    /// the installed runtime identity and is rechecked before every launch.
    tea: TeaQualification,
}

impl InstalledRuntimeManifest {
    /// Qualifies the exact local Rust host/core material used by a stopped
    /// daemon. No interpreter, script, or ambient package authority is used.
    ///
    /// Qualification captures the Rust host identity, seals every explicit
    /// host-source file, and records the exact local core checkout.
    pub fn qualify(
        qualification: InstalledRuntimeQualification,
    ) -> Result<Self, InstalledRuntimeError> {
        if qualification.host_source_files.is_empty() {
            return Err(InstalledRuntimeError::EmptySourceGraph);
        }
        if qualification.host_source_files.len() > MAX_HOST_SOURCE_GRAPH_FILES {
            return Err(InstalledRuntimeError::SourceGraphTooLarge {
                actual: qualification.host_source_files.len(),
                maximum: MAX_HOST_SOURCE_GRAPH_FILES,
            });
        }

        let host_executable = canonical_executable_file(
            "Rust agent host executable",
            &qualification.host_executable,
        )?;
        let host_source_root =
            canonical_directory("Tea host source root", &qualification.host_source_root)?;
        // The host executable is the sole launch artifact. Source bytes are
        // provenance only and are sealed independently from the executable.
        let (source_files, source_graph_digest) =
            seal_source_graph(&host_source_root, qualification.host_source_files)?;
        let tea = qualify_tea()?;
        // The host identity is a closed Rust-host marker, not a process probe.
        let host_identity = RUST_HOST_IDENTITY.to_owned();
        let host_identity_output = HostIdentityOutput {
            stdout: host_identity.as_bytes().to_vec(),
            stderr: Vec::new(),
        };
        let core_lock = tea.root.join("Cargo.lock");
        let host_binary_digest = digest_regular_file("Rust host executable", &host_executable)?;
        let core_lock_digest = digest_regular_file("Tea Cargo.lock", &core_lock)?;
        let manifest = Self {
            host_executable,
            host_identity,
            host_identity_output,
            host_source_root,
            source_files,
            source_graph_digest,
            host_binary_digest,
            core_lock_digest,
            tea,
        };
        manifest.verify_installed_material()?;
        Ok(manifest)
    }

    #[must_use]
    pub fn host_executable(&self) -> &Path {
        &self.host_executable
    }

    #[must_use]
    pub fn host_identity(&self) -> &str {
        &self.host_identity
    }

    #[must_use]
    pub fn host_identity_output(&self) -> &HostIdentityOutput {
        &self.host_identity_output
    }

    /// Canonical root containing every explicitly qualified local host source
    /// file. This is provenance for an installed build, not actor input.
    #[must_use]
    pub fn host_source_root(&self) -> &Path {
        &self.host_source_root
    }

    #[must_use]
    pub fn core_root(&self) -> &Path {
        &self.tea.root
    }

    #[must_use]
    pub fn source_files(&self) -> &[InstalledSourceFile] {
        &self.source_files
    }

    #[must_use]
    pub const fn source_graph_digest(&self) -> ContentDigest {
        self.source_graph_digest
    }

    #[must_use]
    pub const fn host_binary_digest(&self) -> ContentDigest {
        self.host_binary_digest
    }

    #[must_use]
    pub const fn core_lock_digest(&self) -> ContentDigest {
        self.core_lock_digest
    }

    /// Constructs the closed packet runtime identity from material already
    /// restored from an installed-build receipt. Callers select a typed
    /// credential descriptor; this method never obtains a credential value.
    pub fn runtime_identity(
        &self,
        credential: CredentialDescriptorV2,
    ) -> Result<RuntimeIdentityV2, InstalledRuntimeError> {
        credential
            .validate()
            .map_err(|_| InstalledRuntimeError::ReceiptInvalid {
                reason: "credential descriptor is invalid",
            })?;
        let host_executable =
            AbsoluteHostPath::parse(path_utf8("Rust host executable", &self.host_executable)?)
                .map_err(|_| InstalledRuntimeError::ReceiptInvalid {
                    reason: "Rust host executable path is not a valid absolute host path",
                })?;
        let identity = RuntimeIdentityV2 {
            host_executable,
            core_head: self.tea.head.clone(),
            core_source_digest: self.tea.source_digest,
            rust_toolchain: RUST_TOOLCHAIN.to_owned(),
            credential_env: match credential {
                CredentialDescriptorV2::Environment { name } => name,
            },
        };
        identity
            .validate()
            .map_err(|_| InstalledRuntimeError::ReceiptInvalid {
                reason: "installed runtime identity is invalid",
            })?;
        Ok(identity)
    }

    fn identity_digest(&self) -> Result<ContentDigest, InstalledRuntimeError> {
        let mut bytes = Vec::new();
        self.encode_receipt_fields(&mut bytes)?;
        Ok(ContentDigest::of_bytes(&bytes))
    }

    fn encode_receipt_fields(&self, bytes: &mut Vec<u8>) -> Result<(), InstalledRuntimeError> {
        append_receipt_path(bytes, &self.host_executable)?;
        append_receipt_text(bytes, &self.host_identity)?;
        append_receipt_bytes(bytes, self.host_identity_output.stdout())?;
        append_receipt_bytes(bytes, self.host_identity_output.stderr())?;
        append_receipt_path(bytes, &self.host_source_root)?;
        append_receipt_u32(
            bytes,
            u32::try_from(self.source_files.len()).map_err(|_| {
                InstalledRuntimeError::ReceiptInvalid {
                    reason: "Tea host source graph exceeds receipt bound",
                }
            })?,
        );
        for file in &self.source_files {
            append_receipt_text(bytes, file.relative_path.as_str())?;
            append_receipt_bytes(bytes, &file.digest.as_bytes())?;
        }
        append_receipt_bytes(bytes, &self.source_graph_digest.as_bytes())?;
        append_receipt_bytes(bytes, &self.host_binary_digest.as_bytes())?;
        append_receipt_bytes(bytes, &self.core_lock_digest.as_bytes())?;
        append_receipt_path(bytes, &self.tea.root)?;
        append_receipt_text(bytes, &self.tea.head)?;
        append_receipt_u32(
            bytes,
            u32::try_from(self.tea.files.len()).map_err(|_| {
                InstalledRuntimeError::ReceiptInvalid {
                    reason: "Tea source graph exceeds receipt bound",
                }
            })?,
        );
        for file in &self.tea.files {
            append_receipt_text(bytes, file.relative_path.as_str())?;
            append_receipt_bytes(bytes, &file.digest.as_bytes())?;
        }
        append_receipt_bytes(bytes, &self.tea.source_digest.as_bytes())
    }

    fn decode_receipt_fields(
        cursor: &mut ReceiptCursor<'_>,
    ) -> Result<Self, InstalledRuntimeError> {
        let host_executable = cursor.absolute_path("Rust host executable")?;
        let host_identity = cursor.text("Rust host identity")?;
        let host_identity_output = HostIdentityOutput {
            stdout: cursor.bytes()?.to_vec(),
            stderr: cursor.bytes()?.to_vec(),
        };
        if host_identity_output.stdout.len() > MAX_VERSION_OUTPUT_BYTES
            || host_identity_output.stderr.len() > MAX_VERSION_OUTPUT_BYTES
        {
            return Err(InstalledRuntimeError::ReceiptInvalid {
                reason: "Rust host identity output exceeds receipt bound",
            });
        }
        let host_source_root = cursor.absolute_path("Tea host source root")?;
        let source_count =
            usize::try_from(cursor.u32()?).map_err(|_| InstalledRuntimeError::ReceiptInvalid {
                reason: "Tea host source count cannot be represented",
            })?;
        if source_count == 0 || source_count > MAX_SOURCE_GRAPH_FILES {
            return Err(InstalledRuntimeError::ReceiptInvalid {
                reason: "Tea host source graph count is invalid",
            });
        }
        let mut source_files = Vec::with_capacity(source_count);
        for _ in 0..source_count {
            let relative_path =
                RuntimeRelativePath::parse(cursor.bounded_text("Tea host source file", 4_096)?)
                    .map_err(|_| InstalledRuntimeError::ReceiptInvalid {
                        reason: "Tea host source file path is invalid",
                    })?;
            source_files.push(InstalledSourceFile {
                relative_path,
                digest: cursor.digest()?,
            });
        }
        source_files = normalize_host_source_files(source_files)?;
        let source_graph_digest = cursor.digest()?;
        if source_graph_digest_for(&source_files)? != source_graph_digest {
            return Err(InstalledRuntimeError::ReceiptInvalid {
                reason: "Tea host source graph digest does not match receipt files",
            });
        }
        let host_binary_digest = cursor.digest()?;
        let core_lock_digest = cursor.digest()?;
        validate_text("Rust host identity", &host_identity)?;
        let core_root = cursor.absolute_path("Tea checkout root")?;
        let core_head = cursor.bounded_text("Tea HEAD", TEA_HEAD_MAX_BYTES)?;
        validate_text("Tea HEAD", &core_head)?;
        let core_count =
            usize::try_from(cursor.u32()?).map_err(|_| InstalledRuntimeError::ReceiptInvalid {
                reason: "Tea source count cannot be represented",
            })?;
        if core_count == 0 || core_count > MAX_SOURCE_GRAPH_FILES {
            return Err(InstalledRuntimeError::ReceiptInvalid {
                reason: "Tea source graph count is invalid",
            });
        }
        let mut core_files = Vec::with_capacity(core_count);
        for _ in 0..core_count {
            let relative_path = RuntimeRelativePath::parse(
                cursor.bounded_text("Tea source file", 4_096)?,
            )
            .map_err(|_| InstalledRuntimeError::ReceiptInvalid {
                reason: "Tea source file path is invalid",
            })?;
            core_files.push(InstalledSourceFile {
                relative_path,
                digest: cursor.digest()?,
            });
        }
        let core_files = normalize_host_source_files(core_files)?;
        let core_source_digest = cursor.digest()?;
        if tea_source_digest(&core_files)? != core_source_digest {
            return Err(InstalledRuntimeError::ReceiptInvalid {
                reason: "Tea source graph digest does not match receipt files",
            });
        }
        Ok(Self {
            host_executable,
            host_identity,
            host_identity_output,
            host_source_root,
            source_files,
            source_graph_digest,
            host_binary_digest,
            core_lock_digest,
            tea: TeaQualification {
                root: core_root,
                head: core_head,
                files: core_files,
                source_digest: core_source_digest,
            },
        })
    }

    /// Rechecks every mutable installed file and the exact local core
    /// checkout. This function is provider-free and never starts the host.
    pub fn verify_installed_material(&self) -> Result<(), InstalledRuntimeError> {
        if canonical_directory("Tea host source root", &self.host_source_root)?
            != self.host_source_root
        {
            return Err(InstalledRuntimeError::RuntimeDrift {
                evidence: "canonical Tea host source-root path changed",
            });
        }
        if canonical_executable_file("Rust agent host executable", &self.host_executable)?
            != self.host_executable
        {
            return Err(InstalledRuntimeError::RuntimeDrift {
                evidence: "canonical Rust agent host executable path changed",
            });
        }
        if digest_regular_file("Rust agent host executable", &self.host_executable)?
            != self.host_binary_digest
        {
            return Err(InstalledRuntimeError::RuntimeDrift {
                evidence: "Rust agent host executable digest changed",
            });
        }
        if canonical_directory("Tea checkout", &self.tea.root)?
            != self.tea.root
            || digest_regular_file(
                "Tea Cargo.lock",
                &self.tea.root.join("Cargo.lock"),
            )? != self.core_lock_digest
        {
            return Err(InstalledRuntimeError::RuntimeDrift {
                evidence: "Tea lockfile identity changed",
            });
        }
        let source_paths = self
            .source_files
            .iter()
            .map(|file| file.relative_path.clone())
            .collect();
        let (source_files, source_graph_digest) =
            seal_source_graph(&self.host_source_root, source_paths)?;
        if source_files != self.source_files || source_graph_digest != self.source_graph_digest {
            return Err(InstalledRuntimeError::RuntimeDrift {
                evidence: "Tea host source graph digest changed",
            });
        }
        if self.host_identity != RUST_HOST_IDENTITY
            || self.host_identity_output.stdout() != RUST_HOST_IDENTITY.as_bytes()
            || !self.host_identity_output.stderr().is_empty()
        {
            return Err(InstalledRuntimeError::RuntimeDrift {
                evidence: "Rust host identity changed",
            });
        }
        let observed_core = qualify_tea()?;
        if observed_core != self.tea
            || observed_core.source_digest != self.tea.source_digest
        {
            return Err(InstalledRuntimeError::RuntimeDrift {
                evidence: "Tea checkout HEAD or source identity changed",
            });
        }
        Ok(())
    }

    fn verify_packet_bytes(
        &self,
        packet: &AssignmentPacketV2,
        canonical_packet_bytes: &[u8],
    ) -> Result<(), RuntimeVerificationError> {
        if canonical_packet_bytes.is_empty() {
            return Err(RuntimeVerificationError::PacketBytesEmpty);
        }
        packet
            .validate()
            .map_err(|error| RuntimeVerificationError::PacketContract(error.to_string()))?;
        let wire = parse_assignment_packet_v2(canonical_packet_bytes)
            .map_err(|error| RuntimeVerificationError::PacketContract(error.to_string()))?;
        let computed = unsigned_assignment_packet_digest_v2(&wire)
            .map_err(|error| RuntimeVerificationError::PacketContract(error.to_string()))?;
        if computed != packet.packet_digest || wire.packet_digest != packet.packet_digest.to_hex() {
            return Err(RuntimeVerificationError::PacketSealMismatch);
        }
        if wire.assignment_id != packet.assignment_id.get()
            || wire.runtime.host_executable != packet.runtime.host_executable.as_str()
            || wire.runtime.core_head != packet.runtime.core_head
            || wire.runtime.core_source_digest != packet.runtime.core_source_digest.to_hex()
            || wire.runtime.rust_toolchain != packet.runtime.rust_toolchain
            || wire.runtime.credential_env != packet.runtime.credential_env
        {
            return Err(RuntimeVerificationError::PacketContract(
                "canonical packet runtime identity differs from typed assignment".to_owned(),
            ));
        }
        Ok(())
    }

    fn verify_runtime_identity(
        &self,
        packet: &AssignmentPacketV2,
        spawn: &TeaHostSpawnSpec,
    ) -> Result<(), InstalledRuntimeError> {
        if packet.runtime.host_executable.as_str() != self.host_executable.to_string_lossy()
            || packet.runtime.core_head != self.tea.head
            || packet.runtime.core_source_digest != self.tea.source_digest
            || packet.runtime.rust_toolchain != RUST_TOOLCHAIN
        {
            return Err(InstalledRuntimeError::RuntimeDrift {
                evidence: "assignment packet runtime identity is not the installed manifest",
            });
        }

        let expected_workspace = Path::new(packet.workspace_root.as_str());
        if spawn.executable() != self.host_executable
            || spawn.working_directory() != expected_workspace
            || spawn.actor_source_fd() != 0
        {
            return Err(InstalledRuntimeError::RuntimeDrift {
                evidence: "Tea host spawn specification differs from the installed assignment runtime",
            });
        }

        if !spawn.arguments().is_empty() {
            return Err(InstalledRuntimeError::RuntimeDrift {
                evidence: "Rust host launch unexpectedly carries command-line runtime state",
            });
        }
        verify_credential_environment(packet, spawn)?;
        Ok(())
    }
}

fn seal_source_graph(
    source_root: &Path,
    source_paths: Vec<RuntimeRelativePath>,
) -> Result<(Vec<InstalledSourceFile>, ContentDigest), InstalledRuntimeError> {
    require_complete_host_source_inventory(source_root, &source_paths)?;
    let mut seen = BTreeSet::new();
    let mut files = Vec::with_capacity(source_paths.len());
    for relative_path in source_paths {
        if !seen.insert(relative_path.as_str().to_owned()) {
            return Err(InstalledRuntimeError::DuplicateSourceGraphPath {
                path: relative_path.as_str().to_owned(),
            });
        }
        let path = source_root.join(relative_path.as_str());
        let canonical = canonical_regular_file("Tea host source file", &path)?;
        if !canonical.starts_with(source_root) {
            return Err(InstalledRuntimeError::SourceFileOutsideRoot {
                path: relative_path.as_str().to_owned(),
            });
        }
        files.push(InstalledSourceFile {
            digest: digest_regular_file("Tea host source file", &canonical)?,
            relative_path,
        });
    }
    let files = normalize_host_source_files(files)?;
    let digest = source_graph_digest_for(&files)?;
    Ok((files, digest))
}

fn normalize_host_source_files(
    mut files: Vec<InstalledSourceFile>,
) -> Result<Vec<InstalledSourceFile>, InstalledRuntimeError> {
    let mut seen = BTreeSet::new();
    for file in &files {
        if !seen.insert(file.relative_path.as_str().to_owned()) {
            return Err(InstalledRuntimeError::DuplicateSourceGraphPath {
                path: file.relative_path.as_str().to_owned(),
            });
        }
    }
    files.sort_by(|left, right| {
        left.relative_path
            .as_str()
            .cmp(right.relative_path.as_str())
    });
    Ok(files)
}

fn source_graph_digest_for(
    files: &[InstalledSourceFile],
) -> Result<ContentDigest, InstalledRuntimeError> {
    if files.is_empty() || files.len() > MAX_HOST_SOURCE_GRAPH_FILES {
        return Err(InstalledRuntimeError::ReceiptInvalid {
            reason: "Tea host source graph count is invalid",
        });
    }
    let mut hasher = blake3::Hasher::new();
    hasher.update(SOURCE_GRAPH_DOMAIN);
    for file in files {
        let path = file.relative_path.as_str().as_bytes();
        let length =
            u32::try_from(path.len()).map_err(|_| InstalledRuntimeError::SourcePathTooLong {
                path: file.relative_path.as_str().to_owned(),
            })?;
        hasher.update(&length.to_be_bytes());
        hasher.update(path);
        hasher.update(&file.digest.as_bytes());
    }
    Ok(ContentDigest::from_bytes(*hasher.finalize().as_bytes()))
}

fn tea_source_digest(
    files: &[InstalledSourceFile],
) -> Result<ContentDigest, InstalledRuntimeError> {
    if files.is_empty() || files.len() > MAX_SOURCE_GRAPH_FILES {
        return Err(InstalledRuntimeError::CoreSourceGraphInvalid { count: files.len() });
    }
    let mut hasher = blake3::Hasher::new();
    hasher.update(TEA_SOURCE_DOMAIN);
    for file in files {
        let path = file.relative_path.as_str().as_bytes();
        let length =
            u32::try_from(path.len()).map_err(|_| InstalledRuntimeError::SourcePathTooLong {
                path: file.relative_path.as_str().to_owned(),
            })?;
        hasher.update(&length.to_be_bytes());
        hasher.update(path);
        hasher.update(&file.digest.as_bytes());
    }
    Ok(ContentDigest::from_bytes(*hasher.finalize().as_bytes()))
}

/// Qualifies the temporary local Tea source checkout. A clean
/// Git status is mandatory: a dirty checkout is not a reproducible runtime
/// input even when its current `HEAD` happens to match the receipt. The
/// source inventory excludes only Git's private metadata and Cargo build
/// output; all tracked or untracked project material that can affect Cargo
/// resolution is otherwise bound by digest.
fn qualify_tea() -> Result<TeaQualification, InstalledRuntimeError> {
    let root = canonical_directory("Tea checkout", Path::new(TEA_SOURCE))?;
    let git = find_git_executable()?;
    let head_output = Command::new(&git)
        .arg("-C")
        .arg(&root)
        .args(["rev-parse", "--verify", "HEAD"])
        .output()
        .map_err(|source| InstalledRuntimeError::CoreGitCommand {
            operation: "rev-parse HEAD",
            source,
        })?;
    if !head_output.status.success() {
        return Err(InstalledRuntimeError::CoreGitCommandFailed {
            operation: "rev-parse HEAD",
            status: head_output.status.code(),
        });
    }
    let head = String::from_utf8(head_output.stdout)
        .map_err(|_| InstalledRuntimeError::CoreHeadInvalid)?
        .trim()
        .to_owned();
    if head.len() != 40
        || !head
            .bytes()
            .all(|byte| byte.is_ascii_hexdigit() && !byte.is_ascii_uppercase())
    {
        return Err(InstalledRuntimeError::CoreHeadInvalid);
    }

    let status = Command::new(&git)
        .arg("-C")
        .arg(&root)
        .args(["status", "--porcelain=v1", "--untracked-files=all"])
        .output()
        .map_err(|source| InstalledRuntimeError::CoreGitCommand {
            operation: "status",
            source,
        })?;
    if !status.status.success() {
        return Err(InstalledRuntimeError::CoreGitCommandFailed {
            operation: "status",
            status: status.status.code(),
        });
    }
    if !status.stdout.is_empty() {
        return Err(InstalledRuntimeError::CoreCheckoutDirty);
    }

    let mut files = inventory_core_files(&root, &git)?;
    files.sort_by(|left, right| {
        left.relative_path
            .as_str()
            .cmp(right.relative_path.as_str())
    });
    if files.is_empty() || files.len() > MAX_SOURCE_GRAPH_FILES {
        return Err(InstalledRuntimeError::CoreSourceGraphInvalid { count: files.len() });
    }
    let source_digest = tea_source_digest(&files)?;
    Ok(TeaQualification {
        root,
        head,
        files,
        source_digest,
    })
}

fn inventory_core_files(
    root: &Path,
    git: &Path,
) -> Result<Vec<InstalledSourceFile>, InstalledRuntimeError> {
    let output = Command::new(git)
        .arg("-C")
        .arg(root)
        .args(["ls-files", "-z"])
        .output()
        .map_err(|source| InstalledRuntimeError::CoreGitCommand {
            operation: "ls-files",
            source,
        })?;
    if !output.status.success() {
        return Err(InstalledRuntimeError::CoreGitCommandFailed {
            operation: "ls-files",
            status: output.status.code(),
        });
    }
    let mut files = Vec::new();
    for raw in output
        .stdout
        .split(|byte| *byte == 0)
        .filter(|raw| !raw.is_empty())
    {
        let relative =
            std::str::from_utf8(raw).map_err(|_| InstalledRuntimeError::CoreHeadInvalid)?;
        let relative_path = RuntimeRelativePath::parse(relative.to_owned()).map_err(|_| {
            InstalledRuntimeError::SourceGraphPathInvalid {
                path: relative.to_owned(),
            }
        })?;
        let path = root.join(relative_path.as_str());
        let metadata =
            fs::symlink_metadata(&path).map_err(|source| InstalledRuntimeError::Metadata {
                field: "Tea source file",
                path: path.clone(),
                source,
            })?;
        if metadata.file_type().is_symlink() {
            return Err(InstalledRuntimeError::SourceGraphSymlink { path });
        }
        if !metadata.is_file() {
            return Err(InstalledRuntimeError::NotRegularFile {
                field: "Tea source file",
                path,
            });
        }
        files.push(InstalledSourceFile {
            relative_path,
                digest: digest_regular_file("Tea source file", &path)?,
        });
    }
    Ok(files)
}

fn find_git_executable() -> Result<PathBuf, InstalledRuntimeError> {
    ["/usr/bin/git", "/opt/homebrew/bin/git"]
        .iter()
        .map(Path::new)
        .find(|path| path.is_file())
        .map(Path::to_owned)
        .ok_or(InstalledRuntimeError::CoreGitUnavailable)
}

/// Proves that the declared source graph closes every local filesystem input
/// available under the host root. Hashing only an operator-provided subset
/// would make the source-graph receipt dishonest. This deliberately overbinds
/// harmless local files such as tests and package metadata.
fn require_complete_host_source_inventory(
    source_root: &Path,
    declared: &[RuntimeRelativePath],
) -> Result<(), InstalledRuntimeError> {
    let mut declared_paths = BTreeSet::new();
    for path in declared {
        if !declared_paths.insert(path.as_str().to_owned()) {
            return Err(InstalledRuntimeError::DuplicateSourceGraphPath {
                path: path.as_str().to_owned(),
            });
        }
    }
    let inventory = inventory_host_source_root(source_root)?;
    let inventory_paths = inventory
        .iter()
        .map(|path| path.as_str().to_owned())
        .collect::<BTreeSet<_>>();
    if let Some(path) = inventory_paths.difference(&declared_paths).next() {
        return Err(InstalledRuntimeError::UndeclaredSourceFile {
            path: path.to_owned(),
        });
    }
    if let Some(path) = declared_paths.difference(&inventory_paths).next() {
        return Err(InstalledRuntimeError::DeclaredSourceFileMissing {
            path: path.to_owned(),
        });
    }
    Ok(())
}

fn inventory_host_source_root(
    source_root: &Path,
) -> Result<Vec<RuntimeRelativePath>, InstalledRuntimeError> {
    let mut paths = Vec::new();
    inventory_host_source_directory(source_root, source_root, &mut paths)?;
    if paths.is_empty() {
        return Err(InstalledRuntimeError::EmptySourceGraph);
    }
    if paths.len() > MAX_HOST_SOURCE_GRAPH_FILES {
        return Err(InstalledRuntimeError::SourceGraphTooLarge {
            actual: paths.len(),
            maximum: MAX_HOST_SOURCE_GRAPH_FILES,
        });
    }
    paths.sort_by(|left, right| left.as_str().cmp(right.as_str()));
    Ok(paths)
}

fn inventory_host_source_directory(
    source_root: &Path,
    directory: &Path,
    paths: &mut Vec<RuntimeRelativePath>,
) -> Result<(), InstalledRuntimeError> {
    let entries = fs::read_dir(directory).map_err(|source| InstalledRuntimeError::Read {
        field: "Tea host source directory",
        path: directory.to_owned(),
        source,
    })?;
    for entry in entries {
        let entry = entry.map_err(|source| InstalledRuntimeError::Read {
            field: "Tea host source directory",
            path: directory.to_owned(),
            source,
        })?;
        let path = entry.path();
        let metadata =
            fs::symlink_metadata(&path).map_err(|source| InstalledRuntimeError::Metadata {
                field: "Tea host source-root entry",
                path: path.clone(),
                source,
            })?;
        if metadata.file_type().is_symlink() {
            return Err(InstalledRuntimeError::SourceGraphSymlink { path });
        }
        if metadata.is_dir() {
            let canonical = canonical_directory("Tea host source directory", &path)?;
            if !canonical.starts_with(source_root) {
                return Err(InstalledRuntimeError::SourceFileOutsideRoot {
                    path: canonical.to_string_lossy().to_string(),
                });
            }
            inventory_host_source_directory(source_root, &canonical, paths)?;
            continue;
        }
        if !metadata.is_file() {
            return Err(InstalledRuntimeError::NotRegularFile {
                field: "Tea host source-root entry",
                path,
            });
        }
        let canonical = canonical_regular_file("Tea host source file", &path)?;
        let relative = canonical.strip_prefix(source_root).map_err(|_| {
            InstalledRuntimeError::SourceFileOutsideRoot {
                path: canonical.to_string_lossy().to_string(),
            }
        })?;
        let relative =
            relative
                .to_str()
                .ok_or_else(|| InstalledRuntimeError::SourceGraphPathInvalid {
                    path: relative.to_string_lossy().to_string(),
                })?;
        let relative = RuntimeRelativePath::parse(relative.to_owned()).map_err(|_| {
            InstalledRuntimeError::SourceGraphPathInvalid {
                path: relative.to_owned(),
            }
        })?;
        paths.push(relative);
    }
    Ok(())
}

fn normalize_kernel_source_files(
    mut files: Vec<KernelSourceReceiptFileV2>,
) -> Result<Vec<KernelSourceReceiptFileV2>, InstalledRuntimeError> {
    if files.is_empty() || files.len() > MAX_SOURCE_GRAPH_FILES {
        return Err(InstalledRuntimeError::ReceiptInvalid {
            reason: "kernel source graph count is invalid",
        });
    }
    let mut seen = BTreeSet::new();
    for file in &files {
        if !seen.insert(file.relative_path.as_str().to_owned()) {
            return Err(InstalledRuntimeError::ReceiptInvalid {
                reason: "kernel source graph repeats a path",
            });
        }
    }
    files.sort_by(|left, right| {
        left.relative_path
            .as_str()
            .cmp(right.relative_path.as_str())
    });
    Ok(files)
}

fn kernel_source_graph_digest(
    files: &[KernelSourceReceiptFileV2],
) -> Result<ContentDigest, InstalledRuntimeError> {
    if files.is_empty() || files.len() > MAX_SOURCE_GRAPH_FILES {
        return Err(InstalledRuntimeError::ReceiptInvalid {
            reason: "kernel source graph count is invalid",
        });
    }
    let mut hasher = blake3::Hasher::new();
    hasher.update(KERNEL_SOURCE_GRAPH_DOMAIN);
    for file in files {
        let path = file.relative_path.as_str().as_bytes();
        let length =
            u32::try_from(path.len()).map_err(|_| InstalledRuntimeError::ReceiptInvalid {
                reason: "kernel source path exceeds receipt bound",
            })?;
        hasher.update(&length.to_be_bytes());
        hasher.update(path);
        hasher.update(&file.digest.as_bytes());
    }
    Ok(ContentDigest::from_bytes(*hasher.finalize().as_bytes()))
}

fn kernel_build_digest(
    source_digest: ContentDigest,
    binary_digest: ContentDigest,
    approved_tools_digest: ContentDigest,
    runtime_digest: ContentDigest,
    schema_identity: &str,
    credential_environment: &str,
) -> Result<ContentDigest, InstalledRuntimeError> {
    let mut bytes = Vec::new();
    append_receipt_bytes(&mut bytes, KERNEL_BUILD_DOMAIN)?;
    append_receipt_bytes(&mut bytes, &source_digest.as_bytes())?;
    append_receipt_bytes(&mut bytes, &binary_digest.as_bytes())?;
    append_receipt_bytes(&mut bytes, &approved_tools_digest.as_bytes())?;
    append_receipt_text(&mut bytes, schema_identity)?;
    append_receipt_bytes(&mut bytes, &runtime_digest.as_bytes())?;
    append_receipt_text(&mut bytes, credential_environment)?;
    Ok(ContentDigest::of_bytes(&bytes))
}

fn validate_absolute_receipt_path(
    field: &'static str,
    path: &Path,
) -> Result<(), InstalledRuntimeError> {
    let _ = AbsoluteHostPath::parse(path_utf8(field, path)?).map_err(|_| {
        InstalledRuntimeError::ReceiptInvalid {
            reason: "receipt path is not an absolute host path",
        }
    })?;
    Ok(())
}

fn path_utf8(field: &'static str, path: &Path) -> Result<String, InstalledRuntimeError> {
    path.to_str()
        .filter(|value| !value.is_empty() && !value.contains('\0'))
        .map(ToOwned::to_owned)
        .ok_or(InstalledRuntimeError::ReceiptInvalid {
            reason: match field {
                "Rust host executable" => "Rust host executable path is not valid UTF-8",
                _ => "receipt path is not valid UTF-8",
            },
        })
}

fn append_receipt_u32(bytes: &mut Vec<u8>, value: u32) {
    bytes.extend_from_slice(&value.to_be_bytes());
}

fn append_receipt_bytes(bytes: &mut Vec<u8>, value: &[u8]) -> Result<(), InstalledRuntimeError> {
    let length = u32::try_from(value.len()).map_err(|_| InstalledRuntimeError::ReceiptInvalid {
        reason: "receipt field exceeds byte bound",
    })?;
    append_receipt_u32(bytes, length);
    bytes.extend_from_slice(value);
    Ok(())
}

fn append_receipt_text(bytes: &mut Vec<u8>, value: &str) -> Result<(), InstalledRuntimeError> {
    if value.contains('\0') {
        return Err(InstalledRuntimeError::ReceiptInvalid {
            reason: "receipt text contains NUL",
        });
    }
    append_receipt_bytes(bytes, value.as_bytes())
}

fn append_receipt_path(bytes: &mut Vec<u8>, value: &Path) -> Result<(), InstalledRuntimeError> {
    append_receipt_text(bytes, &path_utf8("receipt path", value)?)
}

struct ReceiptCursor<'a> {
    bytes: &'a [u8],
    offset: usize,
}

impl<'a> ReceiptCursor<'a> {
    const fn new(bytes: &'a [u8]) -> Self {
        Self { bytes, offset: 0 }
    }

    fn u32(&mut self) -> Result<u32, InstalledRuntimeError> {
        let bytes = self.take_exact(4)?;
        Ok(u32::from_be_bytes(
            bytes.try_into().expect("fixed receipt field length"),
        ))
    }

    fn bytes(&mut self) -> Result<&'a [u8], InstalledRuntimeError> {
        let length =
            usize::try_from(self.u32()?).map_err(|_| InstalledRuntimeError::ReceiptInvalid {
                reason: "receipt field length cannot be represented",
            })?;
        self.take_exact(length)
    }

    fn expect_bytes(&mut self, expected: &[u8]) -> Result<(), InstalledRuntimeError> {
        if self.bytes()? == expected {
            Ok(())
        } else {
            Err(InstalledRuntimeError::ReceiptInvalid {
                reason: "installed build receipt version is unsupported",
            })
        }
    }

    fn digest(&mut self) -> Result<ContentDigest, InstalledRuntimeError> {
        let bytes = self.bytes()?;
        let digest: [u8; 32] =
            bytes
                .try_into()
                .map_err(|_| InstalledRuntimeError::ReceiptInvalid {
                    reason: "receipt digest is not 32 bytes",
                })?;
        Ok(ContentDigest::from_bytes(digest))
    }

    fn text(&mut self, field: &'static str) -> Result<String, InstalledRuntimeError> {
        let value = self.bounded_text(field, 240)?;
        validate_text(field, &value)?;
        Ok(value)
    }

    fn bounded_text(
        &mut self,
        _field: &'static str,
        maximum: usize,
    ) -> Result<String, InstalledRuntimeError> {
        let value = std::str::from_utf8(self.bytes()?).map_err(|_| {
            InstalledRuntimeError::ReceiptInvalid {
                reason: "receipt text is not UTF-8",
            }
        })?;
        if value.is_empty() || value.len() > maximum || value.contains('\0') {
            return Err(InstalledRuntimeError::ReceiptInvalid {
                reason: "receipt text is out of bounds",
            });
        }
        Ok(value.to_owned())
    }

    fn absolute_path(&mut self, field: &'static str) -> Result<PathBuf, InstalledRuntimeError> {
        let value = self.bounded_text(field, 4_096)?;
        AbsoluteHostPath::parse(value.clone()).map_err(|_| {
            InstalledRuntimeError::ReceiptInvalid {
                reason: "receipt path is not an absolute host path",
            }
        })?;
        Ok(PathBuf::from(value))
    }

    fn take_exact(&mut self, count: usize) -> Result<&'a [u8], InstalledRuntimeError> {
        let end = self
            .offset
            .checked_add(count)
            .ok_or(InstalledRuntimeError::ReceiptInvalid {
                reason: "receipt field length overflows",
            })?;
        let value =
            self.bytes
                .get(self.offset..end)
                .ok_or(InstalledRuntimeError::ReceiptInvalid {
                    reason: "installed build receipt is truncated",
                })?;
        self.offset = end;
        Ok(value)
    }

    const fn is_finished(&self) -> bool {
        self.offset == self.bytes.len()
    }
}

fn canonical_regular_file(
    field: &'static str,
    path: &Path,
) -> Result<PathBuf, InstalledRuntimeError> {
    let canonical =
        fs::canonicalize(path).map_err(|source| InstalledRuntimeError::Canonicalize {
            field,
            path: path.to_owned(),
            source,
        })?;
    let metadata = fs::metadata(&canonical).map_err(|source| InstalledRuntimeError::Metadata {
        field,
        path: canonical.clone(),
        source,
    })?;
    if !metadata.is_file() {
        return Err(InstalledRuntimeError::NotRegularFile {
            field,
            path: canonical,
        });
    }
    Ok(canonical)
}

fn canonical_executable_file(
    field: &'static str,
    path: &Path,
) -> Result<PathBuf, InstalledRuntimeError> {
    let canonical = canonical_regular_file(field, path)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        let mode = fs::metadata(&canonical)
            .map_err(|source| InstalledRuntimeError::Metadata {
                field,
                path: canonical.clone(),
                source,
            })?
            .permissions()
            .mode();
        if mode & 0o111 == 0 {
            return Err(InstalledRuntimeError::NotExecutable {
                field,
                path: canonical,
            });
        }
    }
    Ok(canonical)
}

fn canonical_directory(field: &'static str, path: &Path) -> Result<PathBuf, InstalledRuntimeError> {
    let canonical =
        fs::canonicalize(path).map_err(|source| InstalledRuntimeError::Canonicalize {
            field,
            path: path.to_owned(),
            source,
        })?;
    let metadata = fs::metadata(&canonical).map_err(|source| InstalledRuntimeError::Metadata {
        field,
        path: canonical.clone(),
        source,
    })?;
    if !metadata.is_dir() {
        return Err(InstalledRuntimeError::NotDirectory {
            field,
            path: canonical,
        });
    }
    Ok(canonical)
}

fn digest_regular_file(
    field: &'static str,
    path: &Path,
) -> Result<ContentDigest, InstalledRuntimeError> {
    let mut source = fs::File::open(path).map_err(|source| InstalledRuntimeError::Read {
        field,
        path: path.to_owned(),
        source,
    })?;
    let mut hasher = blake3::Hasher::new();
    let mut buffer = [0_u8; 64 * 1024];
    loop {
        let count = source
            .read(&mut buffer)
            .map_err(|source| InstalledRuntimeError::Read {
                field,
                path: path.to_owned(),
                source,
            })?;
        if count == 0 {
            break;
        }
        hasher.update(&buffer[..count]);
    }
    Ok(ContentDigest::from_bytes(*hasher.finalize().as_bytes()))
}

/// Only the selected credential environment name may cross into a provider
/// host. The manifest deliberately checks names and fixed kernel values, never
/// secret values; those remain process-local and are never persisted here.
fn verify_credential_environment(
    packet: &AssignmentPacketV2,
    spawn: &TeaHostSpawnSpec,
) -> Result<(), InstalledRuntimeError> {
    let environment = spawn.environment();
    let fixed = [(OsStr::new("NO_COLOR"), OsStr::new("1"))];
    for (name, expected_value) in fixed {
        if environment
            .iter()
            .filter(|(actual_name, actual_value)| {
                actual_name.as_os_str() == name && actual_value.as_os_str() == expected_value
            })
            .count()
            != 1
        {
            return Err(InstalledRuntimeError::RuntimeDrift {
                evidence: "Rust host environment omits or changes a kernel-owned runtime variable",
            });
        }
    }
    if environment
        .iter()
        .filter(|(name, value)| name == OsStr::new("PATH") && !value.is_empty())
        .count()
        != 1
    {
        return Err(InstalledRuntimeError::RuntimeDrift {
            evidence: "Rust host environment omits the kernel-owned approved-tool path",
        });
    }
    let selected_credential = packet.runtime.credential_env.as_str();
    let fixed_names = ["NO_COLOR", "PATH"];
    for (name, value) in environment {
        let Some(name) = name.to_str() else {
            return Err(InstalledRuntimeError::RuntimeDrift {
                evidence: "Rust host environment contains a non-UTF-8 name",
            });
        };
        if fixed_names.contains(&name) {
            continue;
        }
        if selected_credential == name && !value.is_empty() {
            continue;
        }
        return Err(InstalledRuntimeError::RuntimeDrift {
            evidence: "Rust host environment contains an unselected or empty credential variable",
        });
    }
    Ok(())
}

fn validate_text(field: &'static str, value: &str) -> Result<(), InstalledRuntimeError> {
    if value.is_empty() || value.len() > 240 || value.contains('\0') {
        Err(InstalledRuntimeError::InvalidText { field })
    } else {
        Ok(())
    }
}

/// Provider-free installation or launch-time runtime identity failure.
#[derive(Debug, Error)]
pub enum InstalledRuntimeError {
    #[error("installed-build receipt is invalid: {reason}")]
    ReceiptInvalid { reason: &'static str },

    #[error("provider {provider:?} has no credential source in this installed build")]
    UnsupportedCredentialProvider { provider: String },

    #[error("Tea checkout is not clean")]
    CoreCheckoutDirty,

    #[error("Tea checkout has no usable Git executable")]
    CoreGitUnavailable,

    #[error("could not execute Git {operation}: {source}")]
    CoreGitCommand {
        operation: &'static str,
        #[source]
        source: io::Error,
    },

    #[error("Git {operation} failed with status {status:?}")]
    CoreGitCommandFailed {
        operation: &'static str,
        status: Option<i32>,
    },

    #[error("Tea HEAD is not a full hexadecimal commit identity")]
    CoreHeadInvalid,

    #[error("Tea source inventory is invalid ({count} files)")]
    CoreSourceGraphInvalid { count: usize },

    #[error("spawn credential environment name differs from the installed provider configuration")]
    CredentialEnvironmentMismatch,

    #[error("spawn credential environment value is empty")]
    CredentialEnvironmentMissing,

    #[error(transparent)]
    ProcessCustody(#[from] ProcessCustodyError),

    #[error(transparent)]
    CommandSupervision(#[from] CommandSupervisionError),

    #[error(transparent)]
    GitCustody(#[from] GitCustodyError),

    #[error("{field} cannot be canonicalized at {path:?}: {source}")]
    Canonicalize {
        field: &'static str,
        path: PathBuf,
        #[source]
        source: io::Error,
    },

    #[error("{field} metadata cannot be read at {path:?}: {source}")]
    Metadata {
        field: &'static str,
        path: PathBuf,
        #[source]
        source: io::Error,
    },

    #[error("{field} must be a regular file, found {path:?}")]
    NotRegularFile { field: &'static str, path: PathBuf },

    #[error("{field} must be an executable regular file, found {path:?}")]
    NotExecutable { field: &'static str, path: PathBuf },

    #[error("{field} must be a directory, found {path:?}")]
    NotDirectory { field: &'static str, path: PathBuf },

    #[error("{field} must be 1 through 240 bytes without NUL")]
    InvalidText { field: &'static str },

    #[error("the Tea host source graph is empty")]
    EmptySourceGraph,

    #[error("the Tea host source graph has {actual} files, exceeding {maximum}")]
    SourceGraphTooLarge { actual: usize, maximum: usize },

    #[error("Tea host source graph repeats {path:?}")]
    DuplicateSourceGraphPath { path: String },

    #[error("Tea host source root contains undeclared regular file {path:?}")]
    UndeclaredSourceFile { path: String },

    #[error("Tea host source graph declares absent regular file {path:?}")]
    DeclaredSourceFileMissing { path: String },

    #[error("Tea host source root contains a symlink at {path:?}")]
    SourceGraphSymlink { path: PathBuf },

    #[error("Tea host source path {path:?} is not a valid safe relative path")]
    SourceGraphPathInvalid { path: String },

    #[error("Tea host source file {path:?} resolves outside the qualified source root")]
    SourceFileOutsideRoot { path: String },

    #[error("Tea host source path {path:?} cannot be represented")]
    SourcePathTooLong { path: String },

    #[error("cannot read {field} at {path:?}: {source}")]
    Read {
        field: &'static str,
        path: PathBuf,
        #[source]
        source: io::Error,
    },

    #[error("installed runtime drift: {evidence}")]
    RuntimeDrift { evidence: &'static str },
}
