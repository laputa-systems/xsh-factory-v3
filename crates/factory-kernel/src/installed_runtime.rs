//! Installed Deno/Pi runtime identity at the session-launch boundary.
//!
//! The assignment packet names the qualified runtime facts, but a packet is
//! not proof that those bytes still exist on this host. This module retains a
//! finite installed manifest and rechecks it immediately before a host is
//! spawned. It deliberately has no HTTP, package resolver, ambient-home, or
//! provider path: dependency acquisition is an installation-time effect.
//! Qualification records Deno's actual `info --json` graph; later preflight
//! executes one explicit inert module with Deno's supported `--cached-only`
//! graph/typecheck path and never asks Deno to acquire dependencies.

use std::{
    collections::BTreeSet,
    ffi::{OsStr, OsString},
    fs,
    io::{self, Read},
    os::fd::RawFd,
    path::{Path, PathBuf},
    process::{Command, Output, Stdio},
    sync::Arc,
};

use factory_protocol::{
    AbsoluteHostPath, AssignmentPacketV1, ContentDigest, CredentialDescriptorV1, KernelBuildId,
    RuntimeIdentityV1, RuntimeRelativePath, parse_assignment_packet_v1,
    unsigned_assignment_packet_digest_v1,
};
use thiserror::Error;

use crate::{
    command_supervision::{
        ApprovedToolExecutables, CommandRunner, CommandSupervisionError, DEFAULT_TERMINATION_GRACE,
        ExactExecutable,
    },
    git::{GitCustody, GitCustodyError},
    process_custody::{PiHostSpawnSpec, ProcessCustodyError},
    session_runtime::{RuntimeVerificationError, SessionRuntimeVerifier},
};

const MAX_SOURCE_GRAPH_FILES: usize = 1_024;
/// The Pi host root is intentionally small and closed. Qualification inventories
/// every local file below it, so a future local import cannot be omitted from
/// the packet-visible source graph.
const MAX_HOST_SOURCE_GRAPH_FILES: usize = 256;
const MAX_VERSION_OUTPUT_BYTES: usize = 8 * 1024;
const SOURCE_GRAPH_DOMAIN: &[u8] = b"factory-v3-installed-source-graph-v1\0";
const KERNEL_SOURCE_GRAPH_DOMAIN: &[u8] = b"factory-v3-kernel-source-graph-v1\0";
const KERNEL_BUILD_DOMAIN: &[u8] = b"factory-v3-kernel-build-v1\0";
const INSTALLED_BUILD_RECEIPT_DOMAIN: &[u8] = b"factory-v3-installed-build-receipt-v3\0";
const MAX_RECEIPT_BYTES: usize = 256 * 1024;
const OPENROUTER_PROVIDER: &str = "openrouter";
const PINNED_DENO_VERSION: &str = "2.9.4";
const PINNED_PI_VERSION: &str = "0.84.1";

/// Explicit installation inputs for one immutable Deno/Pi runtime.
///
/// This is kernel/operator input during a stopped-daemon deployment, never an
/// actor wire request. `host_source_files` must enumerate the complete regular
/// file inventory under `host_source_root`; an omitted local import is a
/// qualification error. The cache probe is an explicit source-relative module
/// with no host startup action (normally `factory-pi-host/cache-probe.ts`); it proves
/// the already-qualified graph/typecheck is available from the sealed cache.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct InstalledRuntimeQualification {
    pub deno_executable: PathBuf,
    pub host_source_root: PathBuf,
    pub host_entrypoint: PathBuf,
    pub deno_config: PathBuf,
    pub deno_lock: PathBuf,
    pub deno_dir: PathBuf,
    pub host_source_files: Vec<RuntimeRelativePath>,
    pub cache_probe_module: RuntimeRelativePath,
    pub pi_version: String,
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

/// Exact `deno --version` streams captured during qualification.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DenoVersionOutput {
    stdout: Vec<u8>,
    stderr: Vec<u8>,
}

impl DenoVersionOutput {
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
pub struct KernelSourceReceiptFileV1 {
    relative_path: RuntimeRelativePath,
    digest: ContentDigest,
}

impl KernelSourceReceiptFileV1 {
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
pub struct KernelSourceQualificationV1 {
    source_root: PathBuf,
    files: Vec<KernelSourceReceiptFileV1>,
    digest: ContentDigest,
}

impl KernelSourceQualificationV1 {
    #[must_use]
    pub fn source_root(&self) -> &Path {
        &self.source_root
    }

    #[must_use]
    pub fn files(&self) -> &[KernelSourceReceiptFileV1] {
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
pub fn qualify_kernel_source_v1(
    source_root: &Path,
    source_paths: &[RuntimeRelativePath],
) -> Result<KernelSourceQualificationV1, InstalledRuntimeError> {
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
        files.push(KernelSourceReceiptFileV1::new(
            relative_path.clone(),
            digest_regular_file("kernel source file", &canonical)?,
        )?);
    }
    let files = normalize_kernel_source_files(files)?;
    let digest = kernel_source_graph_digest(&files)?;
    Ok(KernelSourceQualificationV1 {
        source_root,
        files,
        digest,
    })
}

/// Canonical executable and content identity of the exact installed Rust
/// kernel binary. It has no process-spawning behavior.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct KernelBinaryQualificationV1 {
    path: PathBuf,
    digest: ContentDigest,
}

/// Canonical executable and content identity for one closed kernel tool.
/// Cargo and Git are installed build material, never application-selected
/// executable paths. Cargo's compiler/documentation companions are derived
/// from Cargo's canonical directory and qualified alongside it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct InstalledExecutableQualificationV1 {
    path: PathBuf,
    digest: ContentDigest,
}

impl InstalledExecutableQualificationV1 {
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
pub fn qualify_installed_executable_v1(
    field: &'static str,
    path: &Path,
) -> Result<InstalledExecutableQualificationV1, InstalledRuntimeError> {
    let path = canonical_executable_file(field, path)?;
    let digest = digest_regular_file(field, &path)?;
    Ok(InstalledExecutableQualificationV1 { path, digest })
}

/// Closed qualification for the deterministic command/Git executables.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct InstalledApprovedToolsQualificationV1 {
    cargo: InstalledExecutableQualificationV1,
    cargo_rustc: InstalledExecutableQualificationV1,
    cargo_rustdoc: InstalledExecutableQualificationV1,
    git: InstalledExecutableQualificationV1,
}

impl InstalledApprovedToolsQualificationV1 {
    pub fn qualify(cargo: &Path, git: &Path) -> Result<Self, InstalledRuntimeError> {
        let cargo = qualify_installed_executable_v1("Cargo executable", cargo)?;
        let cargo_parent = cargo
            .path
            .parent()
            .ok_or(InstalledRuntimeError::ReceiptInvalid {
                reason: "Cargo executable has no parent directory",
            })?;
        let cargo_rustc = qualify_installed_executable_v1(
            "Cargo Rust compiler executable",
            &cargo_parent.join("rustc"),
        )?;
        let cargo_rustdoc = qualify_installed_executable_v1(
            "Cargo Rust documentation executable",
            &cargo_parent.join("rustdoc"),
        )?;
        let git = qualify_installed_executable_v1("Git executable", git)?;
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
        let mut read = |field: &'static str| -> Result<InstalledExecutableQualificationV1, InstalledRuntimeError> {
            Ok(InstalledExecutableQualificationV1 {
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

    fn command_runner(&self, deno: &Path) -> Result<CommandRunner, InstalledRuntimeError> {
        self.verify_installed_material()?;
        let tools = ApprovedToolExecutables::new(
            ExactExecutable::discover(&self.cargo.path)?,
            ExactExecutable::discover(&self.git.path)?,
            ExactExecutable::discover(deno)?,
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

impl KernelBinaryQualificationV1 {
    #[must_use]
    pub fn path(&self) -> &Path {
        &self.path
    }

    #[must_use]
    pub const fn digest(&self) -> ContentDigest {
        self.digest
    }
}

pub fn qualify_kernel_binary_v1(
    path: &Path,
) -> Result<KernelBinaryQualificationV1, InstalledRuntimeError> {
    let path = canonical_regular_file("kernel binary", path)?;
    let digest = digest_regular_file("kernel binary", &path)?;
    Ok(KernelBinaryQualificationV1 { path, digest })
}

/// Versioned, closed installed-build provenance sealed as the one kernel
/// qualification artifact. PostgreSQL keeps its immutable digest/path seal;
/// this value retains the exact local facts necessary to rebuild a typed
/// runtime identity after daemon restart without a new table or open map.
///
/// The only MVP provider source is OpenRouter's named environment variable.
/// This receipt never contains its value, an OAuth token, or any other secret.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct InstalledKernelBuildReceiptV1 {
    kernel_build_id: KernelBuildId,
    schema_identity: String,
    kernel_source_root: PathBuf,
    kernel_source_files: Vec<KernelSourceReceiptFileV1>,
    kernel_source_digest: ContentDigest,
    kernel_binary: PathBuf,
    kernel_binary_digest: ContentDigest,
    approved_tools: InstalledApprovedToolsQualificationV1,
    runtime: InstalledRuntimeManifest,
    openrouter_credential_environment: String,
}

impl InstalledKernelBuildReceiptV1 {
    /// Constructs one installed-build receipt from the two closed
    /// qualifications produced by this module. Daemon entrypoints should use
    /// this operation instead of hashing source or binary bytes themselves.
    pub fn from_qualifications(
        schema_identity: String,
        source: KernelSourceQualificationV1,
        binary: KernelBinaryQualificationV1,
        approved_tools: InstalledApprovedToolsQualificationV1,
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
        kernel_source_files: Vec<KernelSourceReceiptFileV1>,
        kernel_source_digest: ContentDigest,
        kernel_binary: PathBuf,
        kernel_binary_digest: ContentDigest,
        approved_tools: InstalledApprovedToolsQualificationV1,
        runtime: InstalledRuntimeManifest,
        openrouter_credential_environment: String,
    ) -> Result<Self, InstalledRuntimeError> {
        let kernel_build_id = KernelBuildId::new(kernel_build_digest(
            kernel_source_digest,
            kernel_binary_digest,
            approved_tools.identity_digest(),
            runtime.identity_digest()?,
            &schema_identity,
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
        kernel_source_files: Vec<KernelSourceReceiptFileV1>,
        kernel_source_digest: ContentDigest,
        kernel_binary: PathBuf,
        kernel_binary_digest: ContentDigest,
        approved_tools: InstalledApprovedToolsQualificationV1,
        runtime: InstalledRuntimeManifest,
        openrouter_credential_environment: String,
    ) -> Result<Self, InstalledRuntimeError> {
        validate_absolute_receipt_path("kernel source root", &kernel_source_root)?;
        validate_absolute_receipt_path("kernel binary", &kernel_binary)?;
        validate_text("schema identity", &schema_identity)?;
        CredentialDescriptorV1::Environment {
            name: openrouter_credential_environment.clone(),
        }
        .validate()
        .map_err(|_| InstalledRuntimeError::ReceiptInvalid {
            reason: "OpenRouter credential environment name is invalid",
        })?;
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
            command_runner: self
                .approved_tools
                .command_runner(self.runtime.deno_executable())?,
            git_custody: self.approved_tools.git_custody(git_runtime_root)?,
        })
    }

    #[must_use]
    pub fn openrouter_credential_environment(&self) -> &str {
        &self.openrouter_credential_environment
    }

    /// Builds the assignment-packet runtime identity only for the one
    /// credential provider admitted by this MVP. The returned descriptor is a
    /// configuration name; the environment value remains in the operator
    /// process and is supplied only to the supervised child later.
    pub fn runtime_identity_for_provider(
        &self,
        provider: &str,
    ) -> Result<RuntimeIdentityV1, InstalledRuntimeError> {
        if provider != OPENROUTER_PROVIDER {
            return Err(InstalledRuntimeError::UnsupportedCredentialProvider {
                provider: provider.to_owned(),
            });
        }
        self.runtime
            .runtime_identity(CredentialDescriptorV1::Environment {
                name: self.openrouter_credential_environment.clone(),
            })
    }

    /// Builds the exact Deno/Pi host process contract for the installed
    /// runtime. `credential_environment` comes from the operator process only
    /// at spawn time; this receipt compares its name with configuration but
    /// never reads, stores, logs, or returns its value.
    pub fn pi_host_spawn_spec_for_provider(
        &self,
        provider: &str,
        working_directory: PathBuf,
        actor_source_fd: RawFd,
        credential_environment: (OsString, OsString),
    ) -> Result<PiHostSpawnSpec, InstalledRuntimeError> {
        let _ = self.runtime_identity_for_provider(provider)?;
        if credential_environment.0 != OsStr::new(&self.openrouter_credential_environment) {
            return Err(InstalledRuntimeError::CredentialEnvironmentMismatch);
        }
        if credential_environment.1.is_empty() {
            return Err(InstalledRuntimeError::CredentialEnvironmentMissing);
        }
        PiHostSpawnSpec::new_for_assignment(
            self.runtime.deno_executable.clone(),
            self.runtime.host_entrypoint.clone(),
            self.runtime.deno_config.clone(),
            self.runtime.deno_lock.clone(),
            working_directory,
            actor_source_fd,
            self.runtime.deno_dir.clone(),
            vec![credential_environment],
        )
        .map_err(Into::into)
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
            kernel_source_files.push(KernelSourceReceiptFileV1::new(
                relative_path,
                cursor.digest()?,
            )?);
        }
        let kernel_binary = cursor.absolute_path("kernel binary")?;
        let kernel_binary_digest = cursor.digest()?;
        let approved_tools =
            InstalledApprovedToolsQualificationV1::decode_receipt_fields(&mut cursor)?;
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
            observed_files.push(KernelSourceReceiptFileV1::new(
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
            )?
        {
            return Err(InstalledRuntimeError::RuntimeDrift {
                evidence: "kernel build identity changed",
            });
        }
        self.runtime.verify_installed_material()
    }
}

/// Closed installed runtime manifest retained by the daemon's build authority.
///
/// It has no open metadata map: each field supports one launch invariant. The
/// frozen cache directory itself is execution material, not a second package
/// authority. Its graph identity is the checked receipt digest carried in the
/// assignment packet.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct InstalledRuntimeManifest {
    deno_executable: PathBuf,
    deno_version: String,
    deno_version_output: DenoVersionOutput,
    host_source_root: PathBuf,
    host_entrypoint: PathBuf,
    deno_config: PathBuf,
    deno_lock: PathBuf,
    deno_dir: PathBuf,
    source_files: Vec<InstalledSourceFile>,
    source_graph_digest: ContentDigest,
    deno_json_digest: ContentDigest,
    deno_lock_digest: ContentDigest,
    resolved_dependency_graph_digest: ContentDigest,
    cache_probe_module: RuntimeRelativePath,
    pi_version: String,
}

impl InstalledRuntimeManifest {
    /// Qualifies the exact local Deno/Pi material used by a stopped daemon.
    ///
    /// Qualification captures Deno's complete version output, seals every
    /// explicit host-source file, and canonicalizes Deno's actual resolved
    /// module graph under the build-specific cache. It may populate that cache
    /// while the daemon is stopped. Later verification is strictly cached-only
    /// and never starts the Pi host entrypoint.
    pub fn qualify(
        qualification: InstalledRuntimeQualification,
    ) -> Result<Self, InstalledRuntimeError> {
        validate_text("Pi SDK version", &qualification.pi_version)?;
        if qualification.pi_version != PINNED_PI_VERSION {
            return Err(InstalledRuntimeError::RuntimeVersionNotPinned {
                component: "Pi SDK",
                expected: PINNED_PI_VERSION,
                actual: qualification.pi_version,
            });
        }
        if qualification.host_source_files.is_empty() {
            return Err(InstalledRuntimeError::EmptySourceGraph);
        }
        if qualification.host_source_files.len() > MAX_HOST_SOURCE_GRAPH_FILES {
            return Err(InstalledRuntimeError::SourceGraphTooLarge {
                actual: qualification.host_source_files.len(),
                maximum: MAX_HOST_SOURCE_GRAPH_FILES,
            });
        }

        let deno_executable =
            canonical_regular_file("Deno executable", &qualification.deno_executable)?;
        let host_source_root =
            canonical_directory("Pi host source root", &qualification.host_source_root)?;
        let host_entrypoint =
            canonical_regular_file("Pi host entrypoint", &qualification.host_entrypoint)?;
        if !host_entrypoint.starts_with(&host_source_root) {
            return Err(InstalledRuntimeError::EntrypointOutsideSourceRoot);
        }
        let deno_config = canonical_regular_file("Deno config", &qualification.deno_config)?;
        let deno_lock = canonical_regular_file("Deno lock", &qualification.deno_lock)?;
        let deno_dir = canonical_directory("DENO_DIR", &qualification.deno_dir)?;
        let (source_files, source_graph_digest) =
            seal_source_graph(&host_source_root, qualification.host_source_files)?;
        let entrypoint_relative = host_entrypoint
            .strip_prefix(&host_source_root)
            .map_err(|_| InstalledRuntimeError::EntrypointOutsideSourceRoot)?;
        let entrypoint_relative =
            RuntimeRelativePath::parse(entrypoint_relative.to_string_lossy().to_string())
                .map_err(|_| InstalledRuntimeError::EntrypointOutsideSourceRoot)?;
        if !source_files
            .iter()
            .any(|file| file.relative_path == entrypoint_relative)
        {
            return Err(InstalledRuntimeError::EntrypointNotInSourceGraph);
        }
        if !source_files
            .iter()
            .any(|file| file.relative_path == qualification.cache_probe_module)
        {
            return Err(InstalledRuntimeError::CacheProbeNotInSourceGraph);
        }
        if qualification.cache_probe_module == entrypoint_relative {
            return Err(InstalledRuntimeError::CacheProbeIsHostEntrypoint);
        }

        let deno_json_digest = digest_regular_file("Deno config", &deno_config)?;
        let deno_lock_digest = digest_regular_file("Deno lock", &deno_lock)?;
        let (deno_version, deno_version_output) = run_deno_version(&deno_executable)?;
        if deno_version != PINNED_DENO_VERSION {
            return Err(InstalledRuntimeError::RuntimeVersionNotPinned {
                component: "Deno",
                expected: PINNED_DENO_VERSION,
                actual: deno_version,
            });
        }
        populate_frozen_deno_cache(
            &deno_executable,
            &host_source_root,
            &host_entrypoint,
            &deno_config,
            &deno_lock,
            &deno_dir,
        )?;
        let resolved_dependency_graph_digest = qualify_deno_module_graph(
            &deno_executable,
            &host_source_root,
            &host_entrypoint,
            &deno_config,
            &deno_lock,
            &deno_dir,
            &source_files,
        )?;
        let manifest = Self {
            deno_executable,
            deno_version,
            deno_version_output,
            host_source_root,
            host_entrypoint,
            deno_config,
            deno_lock,
            deno_dir,
            source_files,
            source_graph_digest,
            deno_json_digest,
            deno_lock_digest,
            resolved_dependency_graph_digest,
            cache_probe_module: qualification.cache_probe_module,
            pi_version: qualification.pi_version,
        };
        manifest.verify_installed_material()?;
        Ok(manifest)
    }

    #[must_use]
    pub fn deno_executable(&self) -> &Path {
        &self.deno_executable
    }

    #[must_use]
    pub fn deno_version(&self) -> &str {
        &self.deno_version
    }

    #[must_use]
    pub fn deno_version_output(&self) -> &DenoVersionOutput {
        &self.deno_version_output
    }

    #[must_use]
    pub fn host_entrypoint(&self) -> &Path {
        &self.host_entrypoint
    }

    /// Canonical root containing every explicitly qualified local host source
    /// file. This is provenance for an installed build, not actor input.
    #[must_use]
    pub fn host_source_root(&self) -> &Path {
        &self.host_source_root
    }

    #[must_use]
    pub fn deno_config(&self) -> &Path {
        &self.deno_config
    }

    #[must_use]
    pub fn deno_lock(&self) -> &Path {
        &self.deno_lock
    }

    #[must_use]
    pub fn deno_dir(&self) -> &Path {
        &self.deno_dir
    }

    /// The explicit safe module used only for cached graph/typecheck preflight.
    #[must_use]
    pub fn cache_probe_module(&self) -> &RuntimeRelativePath {
        &self.cache_probe_module
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
    pub const fn deno_json_digest(&self) -> ContentDigest {
        self.deno_json_digest
    }

    #[must_use]
    pub const fn deno_lock_digest(&self) -> ContentDigest {
        self.deno_lock_digest
    }

    #[must_use]
    pub const fn resolved_dependency_graph_digest(&self) -> ContentDigest {
        self.resolved_dependency_graph_digest
    }

    #[must_use]
    pub fn pi_version(&self) -> &str {
        &self.pi_version
    }

    /// Constructs the closed packet runtime identity from material already
    /// restored from an installed-build receipt. Callers select a typed
    /// credential descriptor; this method never obtains a credential value.
    pub fn runtime_identity(
        &self,
        credential: CredentialDescriptorV1,
    ) -> Result<RuntimeIdentityV1, InstalledRuntimeError> {
        credential
            .validate()
            .map_err(|_| InstalledRuntimeError::ReceiptInvalid {
                reason: "credential descriptor is invalid",
            })?;
        let deno_executable =
            AbsoluteHostPath::parse(path_utf8("Deno executable", &self.deno_executable)?).map_err(
                |_| InstalledRuntimeError::ReceiptInvalid {
                    reason: "Deno executable path is not a valid absolute host path",
                },
            )?;
        Ok(RuntimeIdentityV1 {
            deno_executable,
            deno_version: self.deno_version.clone(),
            source_graph_digest: self.source_graph_digest,
            resolved_dependency_graph_digest: self.resolved_dependency_graph_digest,
            deno_json_digest: self.deno_json_digest,
            deno_lock_digest: self.deno_lock_digest,
            pi_version: self.pi_version.clone(),
            credential,
        })
    }

    fn identity_digest(&self) -> Result<ContentDigest, InstalledRuntimeError> {
        let mut bytes = Vec::new();
        self.encode_receipt_fields(&mut bytes)?;
        Ok(ContentDigest::of_bytes(&bytes))
    }

    fn encode_receipt_fields(&self, bytes: &mut Vec<u8>) -> Result<(), InstalledRuntimeError> {
        append_receipt_path(bytes, &self.deno_executable)?;
        append_receipt_text(bytes, &self.deno_version)?;
        append_receipt_bytes(bytes, self.deno_version_output.stdout())?;
        append_receipt_bytes(bytes, self.deno_version_output.stderr())?;
        append_receipt_path(bytes, &self.host_source_root)?;
        append_receipt_path(bytes, &self.host_entrypoint)?;
        append_receipt_path(bytes, &self.deno_config)?;
        append_receipt_path(bytes, &self.deno_lock)?;
        append_receipt_path(bytes, &self.deno_dir)?;
        append_receipt_u32(
            bytes,
            u32::try_from(self.source_files.len()).map_err(|_| {
                InstalledRuntimeError::ReceiptInvalid {
                    reason: "Pi host source graph exceeds receipt bound",
                }
            })?,
        );
        for file in &self.source_files {
            append_receipt_text(bytes, file.relative_path.as_str())?;
            append_receipt_bytes(bytes, &file.digest.as_bytes())?;
        }
        append_receipt_bytes(bytes, &self.source_graph_digest.as_bytes())?;
        append_receipt_bytes(bytes, &self.deno_json_digest.as_bytes())?;
        append_receipt_bytes(bytes, &self.deno_lock_digest.as_bytes())?;
        append_receipt_bytes(bytes, &self.resolved_dependency_graph_digest.as_bytes())?;
        append_receipt_text(bytes, self.cache_probe_module.as_str())?;
        append_receipt_text(bytes, &self.pi_version)
    }

    fn decode_receipt_fields(
        cursor: &mut ReceiptCursor<'_>,
    ) -> Result<Self, InstalledRuntimeError> {
        let deno_executable = cursor.absolute_path("Deno executable")?;
        let deno_version = cursor.text("Deno version")?;
        let deno_version_output = DenoVersionOutput {
            stdout: cursor.bytes()?.to_vec(),
            stderr: cursor.bytes()?.to_vec(),
        };
        if deno_version_output.stdout.len() > MAX_VERSION_OUTPUT_BYTES
            || deno_version_output.stderr.len() > MAX_VERSION_OUTPUT_BYTES
        {
            return Err(InstalledRuntimeError::ReceiptInvalid {
                reason: "Deno version output exceeds receipt bound",
            });
        }
        let host_source_root = cursor.absolute_path("Pi host source root")?;
        let host_entrypoint = cursor.absolute_path("Pi host entrypoint")?;
        let deno_config = cursor.absolute_path("Deno config")?;
        let deno_lock = cursor.absolute_path("Deno lock")?;
        let deno_dir = cursor.absolute_path("DENO_DIR")?;
        let source_count =
            usize::try_from(cursor.u32()?).map_err(|_| InstalledRuntimeError::ReceiptInvalid {
                reason: "Pi host source count cannot be represented",
            })?;
        if source_count == 0 || source_count > MAX_SOURCE_GRAPH_FILES {
            return Err(InstalledRuntimeError::ReceiptInvalid {
                reason: "Pi host source graph count is invalid",
            });
        }
        let mut source_files = Vec::with_capacity(source_count);
        for _ in 0..source_count {
            let relative_path =
                RuntimeRelativePath::parse(cursor.bounded_text("Pi host source file", 4_096)?)
                    .map_err(|_| InstalledRuntimeError::ReceiptInvalid {
                        reason: "Pi host source file path is invalid",
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
                reason: "Pi host source graph digest does not match receipt files",
            });
        }
        let deno_json_digest = cursor.digest()?;
        let deno_lock_digest = cursor.digest()?;
        let resolved_dependency_graph_digest = cursor.digest()?;
        let cache_probe_module =
            RuntimeRelativePath::parse(cursor.bounded_text("Pi host cache probe module", 4_096)?)
                .map_err(|_| InstalledRuntimeError::ReceiptInvalid {
                reason: "Pi host cache probe module is invalid",
            })?;
        let pi_version = cursor.text("Pi SDK version")?;
        validate_text("Deno version", &deno_version)?;
        validate_text("Pi SDK version", &pi_version)?;
        if deno_version != PINNED_DENO_VERSION {
            return Err(InstalledRuntimeError::RuntimeVersionNotPinned {
                component: "Deno",
                expected: PINNED_DENO_VERSION,
                actual: deno_version,
            });
        }
        if pi_version != PINNED_PI_VERSION {
            return Err(InstalledRuntimeError::RuntimeVersionNotPinned {
                component: "Pi SDK",
                expected: PINNED_PI_VERSION,
                actual: pi_version,
            });
        }
        if !host_entrypoint.starts_with(&host_source_root) {
            return Err(InstalledRuntimeError::ReceiptInvalid {
                reason: "Pi host entrypoint is outside source root",
            });
        }
        let entrypoint_relative =
            host_entrypoint
                .strip_prefix(&host_source_root)
                .map_err(|_| InstalledRuntimeError::ReceiptInvalid {
                    reason: "Pi host entrypoint is outside source root",
                })?;
        let entrypoint_relative = RuntimeRelativePath::parse(
            entrypoint_relative.to_string_lossy().to_string(),
        )
        .map_err(|_| InstalledRuntimeError::ReceiptInvalid {
            reason: "Pi host entrypoint cannot be represented",
        })?;
        if !source_files
            .iter()
            .any(|file| file.relative_path == entrypoint_relative)
        {
            return Err(InstalledRuntimeError::ReceiptInvalid {
                reason: "Pi host entrypoint is absent from source graph",
            });
        }
        if !source_files
            .iter()
            .any(|file| file.relative_path == cache_probe_module)
        {
            return Err(InstalledRuntimeError::ReceiptInvalid {
                reason: "Pi host cache probe module is absent from source graph",
            });
        }
        if cache_probe_module == entrypoint_relative {
            return Err(InstalledRuntimeError::ReceiptInvalid {
                reason: "Pi host cache probe must not be the host entrypoint",
            });
        }
        Ok(Self {
            deno_executable,
            deno_version,
            deno_version_output,
            host_source_root,
            host_entrypoint,
            deno_config,
            deno_lock,
            deno_dir,
            source_files,
            source_graph_digest,
            deno_json_digest,
            deno_lock_digest,
            resolved_dependency_graph_digest,
            cache_probe_module,
            pi_version,
        })
    }

    /// Rechecks every mutable installed file plus the frozen Deno cache graph.
    /// This function is provider-free and never starts the Pi SDK host.
    pub fn verify_installed_material(&self) -> Result<(), InstalledRuntimeError> {
        if canonical_directory("Pi host source root", &self.host_source_root)?
            != self.host_source_root
        {
            return Err(InstalledRuntimeError::RuntimeDrift {
                evidence: "canonical Pi host source-root path changed",
            });
        }
        if canonical_regular_file("Deno executable", &self.deno_executable)? != self.deno_executable
        {
            return Err(InstalledRuntimeError::RuntimeDrift {
                evidence: "canonical Deno executable path changed",
            });
        }
        if canonical_regular_file("Pi host entrypoint", &self.host_entrypoint)?
            != self.host_entrypoint
        {
            return Err(InstalledRuntimeError::RuntimeDrift {
                evidence: "canonical Pi host entrypoint path changed",
            });
        }
        if canonical_regular_file("Deno config", &self.deno_config)? != self.deno_config {
            return Err(InstalledRuntimeError::RuntimeDrift {
                evidence: "canonical Deno config path changed",
            });
        }
        if canonical_regular_file("Deno lock", &self.deno_lock)? != self.deno_lock {
            return Err(InstalledRuntimeError::RuntimeDrift {
                evidence: "canonical Deno lock path changed",
            });
        }
        if canonical_directory("DENO_DIR", &self.deno_dir)? != self.deno_dir {
            return Err(InstalledRuntimeError::RuntimeDrift {
                evidence: "canonical DENO_DIR path changed",
            });
        }
        if digest_regular_file("Deno config", &self.deno_config)? != self.deno_json_digest {
            return Err(InstalledRuntimeError::RuntimeDrift {
                evidence: "Deno config digest changed",
            });
        }
        if digest_regular_file("Deno lock", &self.deno_lock)? != self.deno_lock_digest {
            return Err(InstalledRuntimeError::RuntimeDrift {
                evidence: "Deno lock digest changed",
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
                evidence: "Pi host source graph digest changed",
            });
        }
        let (version, output) = run_deno_version(&self.deno_executable)?;
        if version != self.deno_version || output != self.deno_version_output {
            return Err(InstalledRuntimeError::RuntimeDrift {
                evidence: "exact Deno --version output changed",
            });
        }
        run_frozen_cache_probe(self)?;
        // The cached-only probe above makes a cache miss fail before this
        // `info --no-remote` observation. With the complete graph already
        // proven cache-resident, Deno cannot acquire npm/jsr/remote material;
        // this re-derives the canonical graph identity instead of merely
        // testing that some cache files exist.
        let observed_dependency_graph_digest = qualify_deno_module_graph(
            &self.deno_executable,
            &self.host_source_root,
            &self.host_entrypoint,
            &self.deno_config,
            &self.deno_lock,
            &self.deno_dir,
            &self.source_files,
        )?;
        if observed_dependency_graph_digest != self.resolved_dependency_graph_digest {
            return Err(InstalledRuntimeError::RuntimeDrift {
                evidence: "Deno resolved dependency graph digest changed",
            });
        }
        Ok(())
    }

    fn verify_packet_bytes(
        &self,
        packet: &AssignmentPacketV1,
        canonical_packet_bytes: &[u8],
    ) -> Result<(), RuntimeVerificationError> {
        if canonical_packet_bytes.is_empty() {
            return Err(RuntimeVerificationError::PacketBytesEmpty);
        }
        packet
            .validate()
            .map_err(|error| RuntimeVerificationError::PacketContract(error.to_string()))?;
        let wire = parse_assignment_packet_v1(canonical_packet_bytes)
            .map_err(|error| RuntimeVerificationError::PacketContract(error.to_string()))?;
        let computed = unsigned_assignment_packet_digest_v1(&wire)
            .map_err(|error| RuntimeVerificationError::PacketContract(error.to_string()))?;
        if computed != packet.packet_digest || wire.packet_digest != packet.packet_digest.to_hex() {
            return Err(RuntimeVerificationError::PacketSealMismatch);
        }
        if wire.assignment_id != packet.assignment_id.get()
            || wire.runtime.deno_executable != packet.runtime.deno_executable.as_str()
            || wire.runtime.deno_version != packet.runtime.deno_version
            || wire.runtime.source_graph_digest != packet.runtime.source_graph_digest.to_hex()
            || wire.runtime.deno_json_digest != packet.runtime.deno_json_digest.to_hex()
            || wire.runtime.deno_lock_digest != packet.runtime.deno_lock_digest.to_hex()
            || wire.runtime.pi_version != packet.runtime.pi_version
            || wire.runtime.resolved_dependency_graph_digest
                != packet.runtime.resolved_dependency_graph_digest.to_hex()
        {
            return Err(RuntimeVerificationError::PacketContract(
                "canonical packet runtime identity differs from typed assignment".to_owned(),
            ));
        }
        Ok(())
    }

    fn verify_runtime_identity(
        &self,
        packet: &AssignmentPacketV1,
        spawn: &PiHostSpawnSpec,
    ) -> Result<(), InstalledRuntimeError> {
        if packet.runtime.deno_executable.as_str() != self.deno_executable.to_string_lossy()
            || packet.runtime.deno_version != self.deno_version
            || packet.runtime.source_graph_digest != self.source_graph_digest
            || packet.runtime.deno_json_digest != self.deno_json_digest
            || packet.runtime.deno_lock_digest != self.deno_lock_digest
            || packet.runtime.pi_version != self.pi_version
            || packet.runtime.resolved_dependency_graph_digest
                != self.resolved_dependency_graph_digest
        {
            return Err(InstalledRuntimeError::RuntimeDrift {
                evidence: "assignment packet runtime identity is not the installed manifest",
            });
        }

        let expected_workspace = Path::new(packet.workspace_root.as_str());
        if spawn.executable() != self.deno_executable
            || spawn.host_entrypoint() != self.host_entrypoint
            || spawn.deno_config() != self.deno_config
            || spawn.deno_lock() != self.deno_lock
            || spawn.deno_dir() != Some(self.deno_dir.as_path())
            || spawn.working_directory() != expected_workspace
            || spawn.actor_source_fd() != 0
        {
            return Err(InstalledRuntimeError::RuntimeDrift {
                evidence: "Pi host spawn specification differs from the installed assignment runtime",
            });
        }

        let expected_arguments = [
            OsString::from("run"),
            OsString::from("-A"),
            OsString::from("--no-prompt"),
            OsString::from("--frozen"),
            OsString::from("--cached-only"),
            OsString::from("--config"),
            self.deno_config.as_os_str().to_owned(),
            OsString::from("--lock"),
            self.deno_lock.as_os_str().to_owned(),
            self.host_entrypoint.as_os_str().to_owned(),
        ];
        if spawn.arguments() != expected_arguments {
            return Err(InstalledRuntimeError::RuntimeDrift {
                evidence: "Pi host launch arguments are not the frozen cached-only Deno command",
            });
        }
        verify_credential_environment(packet, spawn)?;
        Ok(())
    }
}

impl SessionRuntimeVerifier for InstalledRuntimeManifest {
    fn verify_packet(
        &self,
        packet: &AssignmentPacketV1,
        canonical_packet_bytes: &[u8],
    ) -> Result<(), RuntimeVerificationError> {
        self.verify_packet_bytes(packet, canonical_packet_bytes)
    }

    fn verify_runtime(
        &self,
        packet: &AssignmentPacketV1,
        spawn: &PiHostSpawnSpec,
    ) -> Result<(), RuntimeVerificationError> {
        self.verify_installed_material()
            .and_then(|()| self.verify_runtime_identity(packet, spawn))
            .map_err(|error| RuntimeVerificationError::RuntimeIdentity(error.to_string()))
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
        let canonical = canonical_regular_file("Pi host source file", &path)?;
        if !canonical.starts_with(source_root) {
            return Err(InstalledRuntimeError::SourceFileOutsideRoot {
                path: relative_path.as_str().to_owned(),
            });
        }
        files.push(InstalledSourceFile {
            digest: digest_regular_file("Pi host source file", &canonical)?,
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
            reason: "Pi host source graph count is invalid",
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

/// Proves that the declared source graph closes every local filesystem input
/// available under the host root. Deno can resolve a new local import during a
/// preflight check, so hashing only an operator-provided subset would make the
/// source-graph receipt dishonest. This deliberately overbinds harmless local
/// files such as tests and package metadata; the installed host root should be
/// kept narrow (normally repository `packages/`).
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
        field: "Pi host source directory",
        path: directory.to_owned(),
        source,
    })?;
    for entry in entries {
        let entry = entry.map_err(|source| InstalledRuntimeError::Read {
            field: "Pi host source directory",
            path: directory.to_owned(),
            source,
        })?;
        let path = entry.path();
        let metadata =
            fs::symlink_metadata(&path).map_err(|source| InstalledRuntimeError::Metadata {
                field: "Pi host source-root entry",
                path: path.clone(),
                source,
            })?;
        if metadata.file_type().is_symlink() {
            return Err(InstalledRuntimeError::SourceGraphSymlink { path });
        }
        if metadata.is_dir() {
            let canonical = canonical_directory("Pi host source directory", &path)?;
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
                field: "Pi host source-root entry",
                path,
            });
        }
        let canonical = canonical_regular_file("Pi host source file", &path)?;
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
    mut files: Vec<KernelSourceReceiptFileV1>,
) -> Result<Vec<KernelSourceReceiptFileV1>, InstalledRuntimeError> {
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
    files: &[KernelSourceReceiptFileV1],
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
) -> Result<ContentDigest, InstalledRuntimeError> {
    let mut bytes = Vec::new();
    append_receipt_bytes(&mut bytes, KERNEL_BUILD_DOMAIN)?;
    append_receipt_bytes(&mut bytes, &source_digest.as_bytes())?;
    append_receipt_bytes(&mut bytes, &binary_digest.as_bytes())?;
    append_receipt_bytes(&mut bytes, &approved_tools_digest.as_bytes())?;
    append_receipt_text(&mut bytes, schema_identity)?;
    append_receipt_bytes(&mut bytes, &runtime_digest.as_bytes())?;
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
                "Deno executable" => "Deno executable path is not valid UTF-8",
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

fn run_deno_version(
    deno_executable: &Path,
) -> Result<(String, DenoVersionOutput), InstalledRuntimeError> {
    let output = exact_deno_command(deno_executable)
        .arg("--version")
        .output()
        .map_err(|source| InstalledRuntimeError::DenoCommand {
            operation: "--version",
            source,
        })?;
    ensure_success(&output)?;
    if output.stdout.len() > MAX_VERSION_OUTPUT_BYTES
        || output.stderr.len() > MAX_VERSION_OUTPUT_BYTES
    {
        return Err(InstalledRuntimeError::VersionOutputTooLarge);
    }
    let stdout = std::str::from_utf8(&output.stdout)
        .map_err(|_| InstalledRuntimeError::VersionOutputNotUtf8)?;
    let version = stdout
        .lines()
        .next()
        .and_then(|line| line.strip_prefix("deno "))
        .and_then(|line| line.split_ascii_whitespace().next())
        .filter(|version| !version.is_empty())
        .ok_or(InstalledRuntimeError::VersionOutputInvalid)?
        .to_owned();
    validate_text("Deno version", &version)?;
    Ok((
        version,
        DenoVersionOutput {
            stdout: output.stdout,
            stderr: output.stderr,
        },
    ))
}

fn run_frozen_cache_probe(
    manifest: &InstalledRuntimeManifest,
) -> Result<(), InstalledRuntimeError> {
    let cache_probe = manifest
        .host_source_root
        .join(manifest.cache_probe_module.as_str());
    let output = exact_deno_command(&manifest.deno_executable)
        .env("DENO_DIR", &manifest.deno_dir)
        .current_dir(&manifest.host_source_root)
        // Deno 2.9 does not accept `--cached-only` for `check`, but it does
        // for `run --check`. The qualified probe module is intentionally not
        // the host entrypoint, so this proves the full graph/typecheck cache
        // without binding FD0 or starting a Pi actor.
        .arg("run")
        .arg("--check")
        .arg("--frozen")
        .arg("--cached-only")
        .arg("--config")
        .arg(&manifest.deno_config)
        .arg("--lock")
        .arg(&manifest.deno_lock)
        .arg(cache_probe)
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .map_err(|source| InstalledRuntimeError::DenoCommand {
            operation: "run --check --frozen --cached-only",
            source,
        })?;
    if !output.success() {
        return Err(InstalledRuntimeError::FrozenCacheProbeFailed {
            status: output.code(),
        });
    }
    Ok(())
}

/// Populates one build-specific cache during stopped-daemon installation.
/// This command resolves the frozen lock but cannot execute the actor or call
/// a model provider. Every later verification and spawn is cached-only.
fn populate_frozen_deno_cache(
    deno_executable: &Path,
    host_source_root: &Path,
    host_entrypoint: &Path,
    deno_config: &Path,
    deno_lock: &Path,
    deno_dir: &Path,
) -> Result<(), InstalledRuntimeError> {
    let status = exact_deno_command(deno_executable)
        .env("DENO_DIR", deno_dir)
        .current_dir(host_source_root)
        .arg("cache")
        .arg("--frozen")
        .arg("--config")
        .arg(deno_config)
        .arg("--lock")
        .arg(deno_lock)
        .arg(host_entrypoint)
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .map_err(|source| InstalledRuntimeError::DenoCommand {
            operation: "cache --frozen",
            source,
        })?;
    if !status.success() {
        return Err(InstalledRuntimeError::FrozenCacheProbeFailed {
            status: status.code(),
        });
    }
    Ok(())
}

/// Captures Deno's actual resolved graph while installation is allowed to
/// use the populated build-specific cache. It does not trust an operator
/// declaration for the packet's dependency identity. Qualification and
/// re-verification intentionally run this same `--no-remote` command: Deno
/// 2.9 emits a different JSON shape when the flag is omitted, even with an
/// identical graph. The frozen lock and cached-only execution probe provide
/// the remote-package integrity and availability fences.
fn qualify_deno_module_graph(
    deno_executable: &Path,
    host_source_root: &Path,
    host_entrypoint: &Path,
    deno_config: &Path,
    deno_lock: &Path,
    deno_dir: &Path,
    source_files: &[InstalledSourceFile],
) -> Result<ContentDigest, InstalledRuntimeError> {
    let output = exact_deno_command(deno_executable)
        .env("DENO_DIR", deno_dir)
        .current_dir(host_source_root)
        .arg("info")
        .arg("--json")
        .arg("--frozen")
        .arg("--no-remote")
        .arg("--config")
        .arg(deno_config)
        .arg("--lock")
        .arg(deno_lock)
        .arg(host_entrypoint)
        .output()
        .map_err(|source| InstalledRuntimeError::DenoCommand {
            operation: "info --json --frozen --no-remote",
            source,
        })?;
    if !output.status.success() {
        return Err(InstalledRuntimeError::FrozenCacheProbeFailed {
            status: output.status.code(),
        });
    }
    if output.stdout.len() > MAX_RECEIPT_BYTES {
        return Err(InstalledRuntimeError::DenoModuleGraphTooLarge);
    }
    let graph: miniserde::json::Value = miniserde::json::from_str(
        std::str::from_utf8(&output.stdout)
            .map_err(|_| InstalledRuntimeError::DenoModuleGraphInvalid)?,
    )
    .map_err(|_| InstalledRuntimeError::DenoModuleGraphInvalid)?;
    let modules = object_array_field(&graph, "modules")?;
    let declared = source_files
        .iter()
        .map(|source| source.relative_path.as_str().to_owned())
        .collect::<BTreeSet<_>>();
    for module in modules {
        let specifier = object_string_field(module, "specifier")?;
        if !specifier.starts_with("file:") {
            continue;
        }
        let local = object_string_field(module, "local")?;
        let local = PathBuf::from(local);
        let canonical = canonical_regular_file("Deno local module", &local)?;
        let relative = canonical.strip_prefix(host_source_root).map_err(|_| {
            InstalledRuntimeError::DenoLocalModuleOutsideSourceRoot {
                path: canonical.clone(),
            }
        })?;
        let relative = relative
            .to_str()
            .ok_or_else(|| InstalledRuntimeError::DenoModuleGraphInvalid)?;
        let relative = RuntimeRelativePath::parse(relative.to_owned())
            .map_err(|_| InstalledRuntimeError::DenoModuleGraphInvalid)?;
        if !declared.contains(relative.as_str()) {
            return Err(InstalledRuntimeError::DenoLocalModuleUndeclared {
                path: relative.as_str().to_owned(),
            });
        }
    }
    Ok(ContentDigest::of_bytes(
        miniserde::json::to_string(&graph).as_bytes(),
    ))
}

fn object_array_field<'a>(
    value: &'a miniserde::json::Value,
    field: &'static str,
) -> Result<&'a miniserde::json::Array, InstalledRuntimeError> {
    let miniserde::json::Value::Object(object) = value else {
        return Err(InstalledRuntimeError::DenoModuleGraphInvalid);
    };
    match object.get(field) {
        Some(miniserde::json::Value::Array(array)) => Ok(array),
        _ => Err(InstalledRuntimeError::DenoModuleGraphInvalid),
    }
}

fn object_string_field<'a>(
    value: &'a miniserde::json::Value,
    field: &'static str,
) -> Result<&'a str, InstalledRuntimeError> {
    let miniserde::json::Value::Object(object) = value else {
        return Err(InstalledRuntimeError::DenoModuleGraphInvalid);
    };
    match object.get(field) {
        Some(miniserde::json::Value::String(value)) if !value.is_empty() => Ok(value),
        _ => Err(InstalledRuntimeError::DenoModuleGraphInvalid),
    }
}

fn exact_deno_command(deno_executable: &Path) -> Command {
    let mut command = Command::new(deno_executable);
    command.env_clear();
    command.env("DENO_NO_UPDATE_CHECK", "1");
    command.env("NO_COLOR", "1");
    command
}

/// Only the selected credential environment name may cross into a provider
/// host. The manifest deliberately checks names and fixed kernel values, never
/// secret values; those remain process-local and are never persisted here.
fn verify_credential_environment(
    packet: &AssignmentPacketV1,
    spawn: &PiHostSpawnSpec,
) -> Result<(), InstalledRuntimeError> {
    let environment = spawn.environment();
    let deno_dir = spawn
        .deno_dir()
        .ok_or(InstalledRuntimeError::RuntimeDrift {
            evidence: "Pi host environment has no build-specific DENO_DIR",
        })?;
    let fixed = [
        (OsStr::new("DENO_NO_UPDATE_CHECK"), OsStr::new("1")),
        (OsStr::new("NO_COLOR"), OsStr::new("1")),
        (OsStr::new("DENO_DIR"), deno_dir.as_os_str()),
    ];
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
                evidence: "Pi host environment omits or changes a kernel-owned runtime variable",
            });
        }
    }

    let selected_credential = match &packet.runtime.credential {
        factory_protocol::CredentialDescriptorV1::Environment { name } => Some(name.as_str()),
        factory_protocol::CredentialDescriptorV1::PiAuthStore { .. } => None,
    };
    let fixed_names = ["DENO_NO_UPDATE_CHECK", "NO_COLOR", "DENO_DIR"];
    for (name, value) in environment {
        let Some(name) = name.to_str() else {
            return Err(InstalledRuntimeError::RuntimeDrift {
                evidence: "Pi host environment contains a non-UTF-8 name",
            });
        };
        if fixed_names.contains(&name) {
            continue;
        }
        if selected_credential == Some(name) && !value.is_empty() {
            continue;
        }
        return Err(InstalledRuntimeError::RuntimeDrift {
            evidence: "Pi host environment contains an unselected or empty credential variable",
        });
    }
    Ok(())
}

fn ensure_success(output: &Output) -> Result<(), InstalledRuntimeError> {
    if output.status.success() {
        Ok(())
    } else {
        Err(InstalledRuntimeError::DenoVersionFailed {
            status: output.status.code(),
        })
    }
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

    #[error("the Pi host source graph is empty")]
    EmptySourceGraph,

    #[error("the Pi host source graph has {actual} files, exceeding {maximum}")]
    SourceGraphTooLarge { actual: usize, maximum: usize },

    #[error("Pi host source graph repeats {path:?}")]
    DuplicateSourceGraphPath { path: String },

    #[error("Pi host source root contains undeclared regular file {path:?}")]
    UndeclaredSourceFile { path: String },

    #[error("Pi host source graph declares absent regular file {path:?}")]
    DeclaredSourceFileMissing { path: String },

    #[error("Pi host source root contains a symlink at {path:?}")]
    SourceGraphSymlink { path: PathBuf },

    #[error("Pi host source path {path:?} is not a valid safe relative path")]
    SourceGraphPathInvalid { path: String },

    #[error("Pi host source file {path:?} resolves outside the qualified source root")]
    SourceFileOutsideRoot { path: String },

    #[error("Pi host source path {path:?} cannot be represented")]
    SourcePathTooLong { path: String },

    #[error("Pi host entrypoint is outside the qualified source root")]
    EntrypointOutsideSourceRoot,

    #[error("Pi host entrypoint is absent from the qualified source graph")]
    EntrypointNotInSourceGraph,

    #[error("Pi host cache probe module is absent from the qualified source graph")]
    CacheProbeNotInSourceGraph,

    #[error("Pi host cache probe module must not be the actor host entrypoint")]
    CacheProbeIsHostEntrypoint,

    #[error("cannot read {field} at {path:?}: {source}")]
    Read {
        field: &'static str,
        path: PathBuf,
        #[source]
        source: io::Error,
    },

    #[error("could not execute Deno {operation}: {source}")]
    DenoCommand {
        operation: &'static str,
        #[source]
        source: io::Error,
    },

    #[error("Deno --version failed with status {status:?}")]
    DenoVersionFailed { status: Option<i32> },

    #[error("Deno --version output exceeds the qualified bound")]
    VersionOutputTooLarge,

    #[error("Deno --version stdout is not UTF-8")]
    VersionOutputNotUtf8,

    #[error("Deno --version stdout does not begin with a Deno version")]
    VersionOutputInvalid,

    #[error("{component} version must be {expected}, found {actual}")]
    RuntimeVersionNotPinned {
        component: &'static str,
        expected: &'static str,
        actual: String,
    },

    #[error("frozen cached-only Deno graph probe failed with status {status:?}")]
    FrozenCacheProbeFailed { status: Option<i32> },

    #[error("Deno local-module graph receipt is malformed")]
    DenoModuleGraphInvalid,

    #[error("Deno local-module graph receipt exceeds the installed receipt bound")]
    DenoModuleGraphTooLarge,

    #[error("Deno resolved local module escapes the Pi host source root: {path:?}")]
    DenoLocalModuleOutsideSourceRoot { path: PathBuf },

    #[error("Deno resolved an undeclared local host module {path:?}")]
    DenoLocalModuleUndeclared { path: String },

    #[error("installed runtime drift: {evidence}")]
    RuntimeDrift { evidence: &'static str },
}

#[cfg(test)]
mod tests {
    use std::{
        fs,
        os::unix::fs::PermissionsExt,
        path::{Path, PathBuf},
    };

    use super::*;
    use factory_protocol::{
        AbsoluteHostPath, AggregateRevision, ApplicationRevisionId, ArtifactId,
        AssignmentEvidenceRoleV1, AssignmentEvidenceV1, AssignmentId, CampaignId,
        CredentialDescriptorV1, DurationMillis, KernelBuildId, MicroUsd, ModelProfileV1, Office,
        ReadExactFileV1, RepositoryRelativePath, SessionLimitsV1, TerminalOperationV1,
        ThinkingLevelV1,
    };

    struct Fixture {
        root: PathBuf,
        deno: PathBuf,
        cargo: PathBuf,
        git: PathBuf,
        source_root: PathBuf,
        entrypoint: PathBuf,
        config: PathBuf,
        lock: PathBuf,
        cache: PathBuf,
    }

    impl Fixture {
        fn new(label: &str) -> Self {
            let root = std::env::temp_dir().join(format!(
                "factory-v3-installed-runtime-{label}-{}-{}",
                std::process::id(),
                fastrand::u64(..)
            ));
            fs::create_dir(&root).expect("fixture root");
            let bin = root.join("bin");
            let source_root = root.join("host");
            let cache = root.join("deno-cache");
            fs::create_dir(&bin).expect("bin");
            fs::create_dir(&source_root).expect("host source root");
            fs::create_dir(&cache).expect("cache");
            fs::write(cache.join("qualified"), b"cache material").expect("cache marker");
            let deno = bin.join("deno");
            fs::write(
                &deno,
                "#!/bin/sh\n\
                 if [ \"$1\" = \"--version\" ]; then\n\
                   printf 'deno 2.9.4 (stable)\\n'; printf 'v8 fake\\n'; printf 'typescript fake\\n'; exit 0\n\
                 fi\n\
                 if [ \"$1\" = \"info\" ] && [ -f \"$DENO_DIR/qualified\" ]; then\n\
                   for value in \"$@\"; do entrypoint=\"$value\"; done\n\
                   directory=$(dirname \"$entrypoint\")\n\
                   printf '{\"modules\":[{\"specifier\":\"file://%s\",\"local\":\"%s\"},{\"specifier\":\"file://%s/support.ts\",\"local\":\"%s/support.ts\"}]}' \"$entrypoint\" \"$entrypoint\" \"$directory\" \"$directory\"; exit 0\n\
                 fi\n\
                 if [ \"$1\" = \"cache\" ]; then exit 0; fi\n\
                 if { [ \"$1\" = \"check\" ] || [ \"$1\" = \"run\" ]; } && [ -f \"$DENO_DIR/qualified\" ]; then exit 0; fi\n\
                 exit 17\n",
            )
            .expect("fake deno");
            fs::set_permissions(&deno, fs::Permissions::from_mode(0o755))
                .expect("make fake deno executable");
            let cargo = bin.join("cargo");
            let git = bin.join("git");
            for tool in [&cargo, &git, &bin.join("rustc"), &bin.join("rustdoc")] {
                fs::write(tool, "#!/bin/sh\nexit 0\n").expect("fake approved executable");
                fs::set_permissions(tool, fs::Permissions::from_mode(0o755))
                    .expect("make fake approved executable executable");
            }
            let entrypoint = source_root.join("main.ts");
            fs::write(&entrypoint, "import './support.ts';\n").expect("entrypoint");
            fs::write(source_root.join("support.ts"), "export const value = 1;\n")
                .expect("support source");
            let config = root.join("deno.json");
            let lock = root.join("deno.lock");
            fs::write(&config, "{\"nodeModulesDir\":\"none\"}\n").expect("config");
            fs::write(&lock, "{\"version\":\"5\"}\n").expect("lock");
            Self {
                root,
                deno,
                cargo,
                git,
                source_root,
                entrypoint,
                config,
                lock,
                cache,
            }
        }

        fn qualification(&self) -> InstalledRuntimeQualification {
            InstalledRuntimeQualification {
                deno_executable: self.deno.clone(),
                host_source_root: self.source_root.clone(),
                host_entrypoint: self.entrypoint.clone(),
                deno_config: self.config.clone(),
                deno_lock: self.lock.clone(),
                deno_dir: self.cache.clone(),
                host_source_files: vec![
                    RuntimeRelativePath::parse("main.ts").expect("main path"),
                    RuntimeRelativePath::parse("support.ts").expect("support path"),
                ],
                cache_probe_module: RuntimeRelativePath::parse("support.ts")
                    .expect("safe cache probe path"),
                pi_version: "0.84.1".to_owned(),
            }
        }

        fn qualify(&self) -> InstalledRuntimeManifest {
            InstalledRuntimeManifest::qualify(self.qualification()).expect("qualify fake runtime")
        }
    }

    impl Drop for Fixture {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.root);
        }
    }

    fn assert_drift(result: Result<(), InstalledRuntimeError>, evidence: &'static str) {
        assert!(matches!(
            result,
            Err(InstalledRuntimeError::RuntimeDrift { evidence: actual }) if actual == evidence
        ));
    }

    fn packet(manifest: &InstalledRuntimeManifest, workspace: &Path) -> AssignmentPacketV1 {
        AssignmentPacketV1 {
            format_version: factory_protocol::ASSIGNMENT_PACKET_V1_FORMAT,
            campaign_id: CampaignId::new(1).unwrap(),
            assignment_id: AssignmentId::new(2).unwrap(),
            kernel_build_id: KernelBuildId::new(ContentDigest::of_bytes(b"build")),
            application_revision_id: ApplicationRevisionId::new(3).unwrap(),
            office: Office::Engineering,
            target: "runtime identity test".to_owned(),
            ticket_attempt_id: Some(factory_protocol::TicketAttemptId::new(1).unwrap()),
            candidate_id: None,
            system_prompt_artifact_id: ArtifactId::new(4).unwrap(),
            assignment_prompt_artifact_id: ArtifactId::new(5).unwrap(),
            required_read_manifest_artifact_id: ArtifactId::new(6).unwrap(),
            workspace_root: AbsoluteHostPath::parse(workspace.to_string_lossy().to_string())
                .unwrap(),
            staging_root: AbsoluteHostPath::parse("/tmp/runtime-identity-staging").unwrap(),
            model: ModelProfileV1 {
                provider: "fake".to_owned(),
                model_id: "fake-model".to_owned(),
                thinking_level: ThinkingLevelV1::None,
                context_token_limit: 1,
                output_token_limit: 1,
                price_input_micro_usd_per_million_tokens: MicroUsd::new(1),
                price_output_micro_usd_per_million_tokens: MicroUsd::new(1),
                price_cache_read_micro_usd_per_million_tokens: MicroUsd::new(1),
                price_cache_write_micro_usd_per_million_tokens: MicroUsd::new(1),
                capability_flags: Vec::new(),
            },
            limits: SessionLimitsV1 {
                turn_limit: 1,
                wall_limit: DurationMillis::new(1),
                output_byte_limit: 1,
            },
            runtime: factory_protocol::RuntimeIdentityV1 {
                deno_executable: AbsoluteHostPath::parse(
                    manifest.deno_executable().to_string_lossy().to_string(),
                )
                .unwrap(),
                deno_version: manifest.deno_version().to_owned(),
                source_graph_digest: manifest.source_graph_digest(),
                resolved_dependency_graph_digest: manifest.resolved_dependency_graph_digest(),
                deno_json_digest: manifest.deno_json_digest(),
                deno_lock_digest: manifest.deno_lock_digest(),
                pi_version: manifest.pi_version().to_owned(),
                credential: CredentialDescriptorV1::Environment {
                    name: "FAKE_PROVIDER_KEY".to_owned(),
                },
            },
            required_reads: vec![ReadExactFileV1 {
                path: RepositoryRelativePath::parse("AGENTS.md").unwrap(),
                digest: ContentDigest::of_bytes(b"read"),
                reason: "test".to_owned(),
            }],
            assignment_evidence: vec![AssignmentEvidenceV1 {
                role: AssignmentEvidenceRoleV1::TicketProposal,
                artifact_id: ArtifactId::new(7).unwrap(),
                digest: ContentDigest::of_bytes(b"proposal"),
                byte_length: 0,
            }],
            terminal_operations: vec![TerminalOperationV1::WorkComplete],
            remaining_campaign_allowance: MicroUsd::new(1),
            revision: AggregateRevision::initial(),
            packet_digest: ContentDigest::of_bytes(b"packet"),
        }
    }

    fn installed_build_receipt(fixture: &Fixture) -> InstalledKernelBuildReceiptV1 {
        let runtime = fixture.qualify();
        let source = qualify_kernel_source_v1(
            &fixture.source_root,
            &[
                RuntimeRelativePath::parse("main.ts").unwrap(),
                RuntimeRelativePath::parse("support.ts").unwrap(),
            ],
        )
        .expect("kernel source qualification");
        let binary = qualify_kernel_binary_v1(&fixture.deno).expect("kernel binary qualification");
        let approved_tools =
            InstalledApprovedToolsQualificationV1::qualify(&fixture.cargo, &fixture.git)
                .expect("approved tool qualification");
        InstalledKernelBuildReceiptV1::from_qualifications(
            "factory-v3-schema:test-receipt".to_owned(),
            source,
            binary,
            approved_tools,
            runtime,
            "OPENROUTER_API_KEY".to_owned(),
        )
        .expect("qualified build receipt")
    }

    #[test]
    fn qualification_captures_exact_deno_graph_and_frozen_cache() {
        let fixture = Fixture::new("qualifies");
        let manifest = fixture.qualify();

        assert_eq!(
            manifest.deno_executable(),
            fs::canonicalize(&fixture.deno).unwrap().as_path()
        );
        assert_eq!(manifest.deno_version(), "2.9.4");
        assert_eq!(manifest.pi_version(), "0.84.1");
        assert_eq!(manifest.source_files().len(), 2);
        assert_ne!(manifest.source_graph_digest(), ContentDigest::of_bytes(b""));
        assert_ne!(
            manifest.resolved_dependency_graph_digest(),
            ContentDigest::of_bytes(b"")
        );
        manifest
            .verify_installed_material()
            .expect("qualified material remains exact");
    }

    #[test]
    fn installed_build_receipt_restores_closed_runtime_and_credential_name_only() {
        let fixture = Fixture::new("build-receipt");
        let receipt = installed_build_receipt(&fixture);
        let bytes = receipt.encode().expect("encode receipt");
        assert!(
            !bytes
                .windows(b"operator-secret-value".len())
                .any(|bytes| bytes == b"operator-secret-value")
        );

        let restored = InstalledKernelBuildReceiptV1::decode(&bytes).expect("decode receipt");
        assert_eq!(restored, receipt);
        restored
            .verify_installed_material("factory-v3-schema:test-receipt")
            .expect("restored receipt requalifies local material");
        let runtime = restored
            .runtime_identity_for_provider("openrouter")
            .expect("configured provider runtime identity");
        assert_eq!(runtime.deno_version, "2.9.4");
        assert!(matches!(
            runtime.credential,
            CredentialDescriptorV1::Environment { name } if name == "OPENROUTER_API_KEY"
        ));
        assert!(restored.runtime_identity_for_provider("other").is_err());

        let spawn = restored
            .pi_host_spawn_spec_for_provider(
                "openrouter",
                fixture.root.clone(),
                0,
                (
                    OsString::from("OPENROUTER_API_KEY"),
                    OsString::from("operator-secret-value"),
                ),
            )
            .expect("build exact launch specification");
        let canonical_cache = fs::canonicalize(&fixture.cache).expect("canonical fixture cache");
        assert_eq!(spawn.deno_dir(), Some(canonical_cache.as_path()));
        assert!(
            restored
                .pi_host_spawn_spec_for_provider(
                    "openrouter",
                    fixture.root.clone(),
                    0,
                    (OsString::from("WRONG_KEY"), OsString::from("value")),
                )
                .is_err()
        );

        fs::write(
            fixture.source_root.join("support.ts"),
            "export const value = 2;\n",
        )
        .expect("drift source graph");
        assert!(
            restored
                .verify_installed_material("factory-v3-schema:test-receipt")
                .is_err()
        );
    }

    #[test]
    fn host_source_graph_drift_blocks_runtime_admission() {
        let fixture = Fixture::new("source-drift");
        let manifest = fixture.qualify();
        fs::write(
            fixture.source_root.join("support.ts"),
            "export const value = 2;\n",
        )
        .expect("mutate source");
        assert_drift(
            manifest.verify_installed_material(),
            "Pi host source graph digest changed",
        );
    }

    #[test]
    fn host_source_graph_rejects_omitted_local_files_and_later_additions() {
        let fixture = Fixture::new("closed-root-inventory");
        fs::write(
            fixture.source_root.join("omitted-local-import.ts"),
            "export const omitted = true;\n",
        )
        .expect("add omitted local source");
        assert!(matches!(
            InstalledRuntimeManifest::qualify(fixture.qualification()),
            Err(InstalledRuntimeError::UndeclaredSourceFile { path })
                if path == "omitted-local-import.ts"
        ));

        fs::remove_file(fixture.source_root.join("omitted-local-import.ts"))
            .expect("remove omitted local source");
        let manifest = fixture.qualify();
        fs::write(
            fixture.source_root.join("new-local-import.ts"),
            "export const newlyVisible = true;\n",
        )
        .expect("add new local source after qualification");
        assert!(matches!(
            manifest.verify_installed_material(),
            Err(InstalledRuntimeError::UndeclaredSourceFile { path })
                if path == "new-local-import.ts"
        ));
    }

    #[test]
    fn config_lock_and_cache_drift_are_individually_observable() {
        let fixture = Fixture::new("config-drift");
        let manifest = fixture.qualify();
        fs::write(&fixture.config, "{\"nodeModulesDir\":\"auto\"}\n").expect("mutate config");
        assert_drift(
            manifest.verify_installed_material(),
            "Deno config digest changed",
        );

        let fixture = Fixture::new("lock-drift");
        let manifest = fixture.qualify();
        fs::write(&fixture.lock, "{\"version\":\"6\"}\n").expect("mutate lock");
        assert_drift(
            manifest.verify_installed_material(),
            "Deno lock digest changed",
        );
    }

    #[test]
    fn exact_deno_version_and_build_specific_cache_are_rechecked() {
        let fixture = Fixture::new("version-and-cache-drift");
        let manifest = fixture.qualify();
        fs::write(
            &fixture.deno,
            "#!/bin/sh\nif [ \"$1\" = \"--version\" ]; then printf 'deno 2.9.5 (stable)\\n'; exit 0; fi\nif [ \"$1\" = \"check\" ]; then exit 0; fi\nexit 17\n",
        )
        .expect("mutate deno");
        fs::set_permissions(&fixture.deno, fs::Permissions::from_mode(0o755))
            .expect("make changed fake deno executable");
        assert_drift(
            manifest.verify_installed_material(),
            "exact Deno --version output changed",
        );

        let fixture = Fixture::new("cache-drift");
        let manifest = fixture.qualify();
        fs::remove_file(fixture.cache.join("qualified")).expect("remove cache marker");
        assert!(matches!(
            manifest.verify_installed_material(),
            Err(InstalledRuntimeError::FrozenCacheProbeFailed { status: Some(17) })
        ));
    }

    #[test]
    fn installation_rejects_unpinned_deno_and_pi_versions() {
        let fixture = Fixture::new("unpinned-deno");
        let script = fs::read_to_string(&fixture.deno)
            .expect("read fake Deno")
            .replace("deno 2.9.4", "deno 2.9.5");
        fs::write(&fixture.deno, script).expect("write changed fake Deno");
        fs::set_permissions(&fixture.deno, fs::Permissions::from_mode(0o755))
            .expect("retain executable mode");
        assert!(matches!(
            InstalledRuntimeManifest::qualify(fixture.qualification()),
            Err(InstalledRuntimeError::RuntimeVersionNotPinned {
                component: "Deno",
                expected: "2.9.4",
                actual,
            }) if actual == "2.9.5"
        ));

        let fixture = Fixture::new("unpinned-pi");
        let mut qualification = fixture.qualification();
        qualification.pi_version = "0.84.2".to_owned();
        assert!(matches!(
            InstalledRuntimeManifest::qualify(qualification),
            Err(InstalledRuntimeError::RuntimeVersionNotPinned {
                component: "Pi SDK",
                expected: "0.84.1",
                actual,
            }) if actual == "0.84.2"
        ));
    }

    #[test]
    fn installed_build_receipt_rejects_cargo_and_git_drift() {
        for (label, changed) in [("cargo-drift", "cargo"), ("git-drift", "git")] {
            let fixture = Fixture::new(label);
            let receipt = installed_build_receipt(&fixture);
            let path = if changed == "cargo" {
                &fixture.cargo
            } else {
                &fixture.git
            };
            fs::write(path, "#!/bin/sh\necho drift\n").expect("mutate approved executable");
            fs::set_permissions(path, fs::Permissions::from_mode(0o755))
                .expect("retain executable mode after drift");
            assert!(
                receipt
                    .verify_installed_material("factory-v3-schema:test-receipt")
                    .is_err(),
                "{changed} drift must reject the installed build before serve"
            );
        }
    }

    #[test]
    fn pinned_deno_check_probe_uses_supported_no_remote_deno_2_9_contract() {
        let deno = ["/opt/homebrew/bin/deno", "/usr/local/bin/deno"]
            .into_iter()
            .map(PathBuf::from)
            .find(|path| path.is_file())
            .expect("the pinned Deno 2.9 runtime is required for installed-runtime qualification");
        let root = std::env::temp_dir().join(format!(
            "factory-v3-real-deno-probe-{}-{}",
            std::process::id(),
            fastrand::u64(..)
        ));
        let source = root.join("host");
        let cache = root.join("deno-cache");
        fs::create_dir_all(&source).expect("real Deno source root");
        fs::create_dir(&cache).expect("real Deno cache root");
        let entrypoint = source.join("main.ts");
        let cache_probe = source.join("probe.ts");
        let config = root.join("deno.json");
        let lock = root.join("deno.lock");
        fs::write(&entrypoint, "export const qualified = true;\n").expect("entrypoint");
        fs::write(&cache_probe, "export {};\n").expect("safe cache probe");
        fs::write(&config, "{\"nodeModulesDir\":\"none\"}\n").expect("config");
        fs::write(&lock, "{\"version\":\"5\"}\n").expect("lock");
        let manifest = InstalledRuntimeManifest::qualify(InstalledRuntimeQualification {
            deno_executable: deno,
            host_source_root: source,
            host_entrypoint: entrypoint,
            deno_config: config,
            deno_lock: lock,
            deno_dir: cache,
            host_source_files: vec![
                RuntimeRelativePath::parse("main.ts").expect("path"),
                RuntimeRelativePath::parse("probe.ts").expect("path"),
            ],
            cache_probe_module: RuntimeRelativePath::parse("probe.ts").expect("path"),
            pi_version: "0.84.1".to_owned(),
        })
        .expect("Deno 2.9 qualifies an explicit inert cache probe");
        manifest
            .verify_installed_material()
            .expect("the same real Deno 2.9 probe remains re-verifiable");
        fs::remove_dir_all(root).expect("remove real Deno probe fixture");
    }

    #[test]
    fn deno_resolved_local_import_cannot_escape_closed_host_root() {
        let deno = ["/opt/homebrew/bin/deno", "/usr/local/bin/deno"]
            .into_iter()
            .map(PathBuf::from)
            .find(|path| path.is_file())
            .expect("the pinned Deno 2.9 runtime is required for installed-runtime qualification");
        let root = std::env::temp_dir().join(format!(
            "factory-v3-real-deno-outside-import-{}-{}",
            std::process::id(),
            fastrand::u64(..)
        ));
        let source = root.join("host");
        let cache = root.join("deno-cache");
        fs::create_dir_all(&source).expect("real Deno source root");
        fs::create_dir(&cache).expect("real Deno cache root");
        fs::write(root.join("outside.ts"), "export const outside = true;\n")
            .expect("outside module");
        let entrypoint = source.join("main.ts");
        let cache_probe = source.join("probe.ts");
        fs::write(&entrypoint, "import '../outside.ts';\n").expect("entrypoint import");
        fs::write(&cache_probe, "export {};\n").expect("safe cache probe");
        let config = root.join("deno.json");
        let lock = root.join("deno.lock");
        fs::write(&config, "{\"nodeModulesDir\":\"none\"}\n").expect("config");
        fs::write(&lock, "{\"version\":\"5\"}\n").expect("lock");
        assert!(matches!(
            InstalledRuntimeManifest::qualify(InstalledRuntimeQualification {
                deno_executable: deno,
                host_source_root: source,
                host_entrypoint: entrypoint,
                deno_config: config,
                deno_lock: lock,
                deno_dir: cache,
                host_source_files: vec![
                    RuntimeRelativePath::parse("main.ts").expect("path"),
                    RuntimeRelativePath::parse("probe.ts").expect("path"),
                ],
                cache_probe_module: RuntimeRelativePath::parse("probe.ts").expect("path"),
                pi_version: "0.84.1".to_owned(),
            }),
            Err(InstalledRuntimeError::DenoLocalModuleOutsideSourceRoot { .. })
        ));
        fs::remove_dir_all(root).expect("remove real Deno outside-import fixture");
    }

    #[test]
    fn runtime_identity_requires_the_exact_cached_only_fd_zero_spawn() {
        let fixture = Fixture::new("spawn-identity");
        let manifest = fixture.qualify();
        let workspace = fixture.root.join("workspace");
        fs::create_dir(&workspace).expect("workspace");
        let packet = packet(&manifest, &workspace);
        let spawn = PiHostSpawnSpec::new_for_assignment(
            manifest.deno_executable().to_owned(),
            manifest.host_entrypoint().to_owned(),
            manifest.deno_config().to_owned(),
            manifest.deno_lock().to_owned(),
            workspace.clone(),
            0,
            manifest.deno_dir().to_owned(),
            vec![(
                OsString::from("FAKE_PROVIDER_KEY"),
                OsString::from("secret"),
            )],
        )
        .expect("exact spawn specification");
        manifest
            .verify_runtime_identity(&packet, &spawn)
            .expect("exact assignment runtime identity");

        let mut dependency_drift_packet = packet.clone();
        dependency_drift_packet
            .runtime
            .resolved_dependency_graph_digest =
            ContentDigest::of_bytes(b"different resolved graph");
        assert_drift(
            manifest.verify_runtime_identity(&dependency_drift_packet, &spawn),
            "assignment packet runtime identity is not the installed manifest",
        );

        let mut pi_version_drift_packet = packet.clone();
        pi_version_drift_packet.runtime.pi_version = "0.84.2".to_owned();
        assert_drift(
            manifest.verify_runtime_identity(&pi_version_drift_packet, &spawn),
            "assignment packet runtime identity is not the installed manifest",
        );

        let ambient_cache_spawn = PiHostSpawnSpec::new(
            manifest.deno_executable().to_owned(),
            manifest.host_entrypoint().to_owned(),
            manifest.deno_config().to_owned(),
            manifest.deno_lock().to_owned(),
            workspace,
            0,
            Vec::new(),
        )
        .expect("regular spawn specification");
        assert_drift(
            manifest.verify_runtime_identity(&packet, &ambient_cache_spawn),
            "Pi host spawn specification differs from the installed assignment runtime",
        );
    }

    #[test]
    fn source_graph_cannot_hide_duplicate_or_outside_paths() {
        let fixture = Fixture::new("closed-graph");
        let mut qualification = fixture.qualification();
        qualification
            .host_source_files
            .push(RuntimeRelativePath::parse("main.ts").unwrap());
        assert!(matches!(
            InstalledRuntimeManifest::qualify(qualification),
            Err(InstalledRuntimeError::DuplicateSourceGraphPath { path }) if path == "main.ts"
        ));

        let outside = fixture.root.join("outside.ts");
        fs::write(&outside, "export const outside = true;\n").unwrap();
        std::os::unix::fs::symlink(&outside, fixture.source_root.join("escape.ts"))
            .expect("source-root escape symlink");
        let mut qualification = fixture.qualification();
        qualification
            .host_source_files
            .push(RuntimeRelativePath::parse("escape.ts").unwrap());
        let result = InstalledRuntimeManifest::qualify(qualification);
        assert!(matches!(
            result,
            Err(InstalledRuntimeError::SourceGraphSymlink { ref path })
                if path == &fs::canonicalize(&fixture.source_root).unwrap().join("escape.ts")
        ));
    }
}
