//! One direct daemon composition from a scheduler-selected durable target to
//! one supervised actor session.
//!
//! This is intentionally not a scheduler or a workflow framework.  It joins
//! only the custody facts that must exist together: a pre-assignment durable
//! target, a daemon-created worktree, exact required-read hashes, rendered
//! prompts, sealed packet bytes, the assignment transition, and the existing
//! `launch_session` process boundary.  Actors receive none of these builders.

use std::{
    collections::{BTreeMap, BTreeSet},
    ffi::OsString,
    fs, io,
    path::{Path, PathBuf},
    sync::Arc,
    time::Duration,
};

use factory_protocol::{
    AbsoluteHostPath, AggregateRevision, ApplicationRevisionId, AssignmentEvidenceRoleV2,
    AssignmentEvidenceV2, AssignmentEvidenceWireV2, AssignmentId, AssignmentLimitsWireV2,
    AssignmentModelWireV2, AssignmentPacketV2, AssignmentPacketWireV2, AssignmentReadWireV2,
    AssignmentRole, AssignmentRuntimeWireV2, CampaignId, ContentDigest, ContextInclusionClassV2,
    ContextItemV2, ContextReferenceV2, ExpectedRevision, HARNESS_COMPILER_VERSION_V2,
    HarnessSpecV2, MicroUsd, OfficeId, ReadExactFileV2, RequiredReadV2, SealedArtifactReferenceV2,
    TerminalOperationV2, TicketContractReadV2, canonical_assignment_packet_json_v2,
    parse_application_bundle_v2, render_template_v2, unsigned_assignment_packet_digest_v2,
};
use miniserde::{Serialize, json};
use thiserror::Error;

use crate::{
    cas::CasStore,
    durable_authority::{
        DurableAssignmentLaunchContext, DurableAssignmentLaunchRequest, DurableAssignmentTarget,
        DurableAuthorityResolver,
    },
    git::{GitCustody, GitCustodyError, OwnedWorktree, WorktreeKind, WorktreeName},
    harness_store::{HarnessStoreError, RecordHarnessCompilation},
    installed_runtime::{
        InstalledKernelBuildReceiptV2, InstalledKernelExecutionTools, InstalledRuntimeError,
    },
    local_transport::LocalDaemon,
    process::{CreateAssignment, ProcessStore, canonical_required_manifest},
    process_custody::ProcessSupervisionSpec,
    session_runtime::{
        CandidateQualitySessionRuntime, SESSION_STDERR_RELATIVE_PATH, SESSION_STDOUT_RELATIVE_PATH,
        SessionLaunchRequest, SessionRuntimeError, SessionRuntimeOutcome, launch_session,
    },
    storage::{KernelStore, StoreError},
    workspace_read::{WorkspaceReadAuthority, WorkspaceReadError},
};

const KERNEL_PRINCIPAL: &str = "factoryd-assignment";
const TERMINATION_GRACE: Duration = Duration::from_secs(1);

/// Scheduler-owned, durable target information for one launch. The target is
/// not an actor request and the credential value deliberately is not
/// printable, cloneable, serializable, or persisted.
pub struct AssignmentMaterializationRequest {
    pub principal: String,
    pub command_id: String,
    pub expected_campaign_revision: ExpectedRevision,
    pub campaign_id: CampaignId,
    pub application_revision_id: ApplicationRevisionId,
    pub target: DurableAssignmentTarget,
    pub credential_environment_value: OsString,
}

/// Durable assignment/session result after its exact disposable inputs have
/// been removed. Every durable artifact was sealed before this value returns.
pub struct AssignmentLaunchOutcome {
    pub assignment_id: AssignmentId,
    pub assignment_revision: AggregateRevision,
    pub session: SessionRuntimeOutcome,
}

/// Exact disposable filesystem resources for one assignment. They are never
/// retained as forensic evidence: the immutable packet, actor seals, streams,
/// transcript, candidate patch, and validation receipts are already in CAS.
struct AssignmentRuntimeResources {
    assignment_id: AssignmentId,
    workspace: OwnedWorktree,
    staging_root: PathBuf,
}

impl AssignmentRuntimeResources {
    fn cleanup(self, git: &GitCustody, runtime_root: &Path) -> Result<(), AssignmentRuntimeError> {
        let workspace = git
            .cleanup_worktree(self.workspace)
            .map_err(AssignmentRuntimeError::WorkspaceCleanup);
        let staging =
            remove_assignment_staging(runtime_root, self.assignment_id, &self.staging_root);
        match (workspace, staging) {
            (Ok(()), Ok(())) => Ok(()),
            (Err(error), Ok(())) | (Ok(()), Err(error)) => Err(error),
            (Err(workspace), Err(staging)) => Err(AssignmentRuntimeError::CleanupFailures {
                workspace: workspace.to_string(),
                staging: staging.to_string(),
            }),
        }
    }
}

/// Provider-free composition failures. A failed launch removes its exact
/// disposable workspace and staging root after preserving every completed
/// immutable seal. Nothing is reused for another assignment identity.
#[derive(Debug, Error)]
pub enum AssignmentRuntimeError {
    #[error(transparent)]
    Store(#[from] StoreError),

    #[error(transparent)]
    Harness(#[from] HarnessStoreError),

    #[error(transparent)]
    InstalledRuntime(#[from] InstalledRuntimeError),

    #[error(transparent)]
    Git(#[from] GitCustodyError),

    #[error("assignment worktree cleanup failed: {0}")]
    WorkspaceCleanup(#[source] GitCustodyError),

    #[error(transparent)]
    WorkspaceRead(#[from] WorkspaceReadError),

    #[error(transparent)]
    ProcessCustody(#[from] crate::process_custody::ProcessCustodyError),

    #[error(transparent)]
    Cas(#[from] crate::cas::CasError),

    #[error(transparent)]
    Database(#[from] sqlx::Error),

    #[error(transparent)]
    Session(#[from] SessionRuntimeError),

    #[error(transparent)]
    Contract(#[from] factory_protocol::ContractError),

    #[error(transparent)]
    Wire(#[from] factory_protocol::FrameError),

    #[error("durable launch target cannot be resolved: {0}")]
    Target(String),

    #[error("installed application material is inconsistent: {0}")]
    Application(String),

    #[error("I/O while {operation} at {path}: {source}")]
    Io {
        operation: &'static str,
        path: PathBuf,
        #[source]
        source: io::Error,
    },

    #[error(
        "assignment staging root differs from its exact owned path: expected {expected}, observed {observed}"
    )]
    StagingRootMismatch {
        expected: PathBuf,
        observed: PathBuf,
    },

    #[error("assignment staging root is not a real directory: {path}")]
    StagingRootNotDirectory { path: PathBuf },

    #[error("assignment staging cleanup did not remove {path}")]
    StagingCleanupIncomplete { path: PathBuf },

    #[error("assignment launch failed: {launch}; disposable cleanup also failed: {cleanup}")]
    LaunchCleanup { launch: String, cleanup: String },

    #[error(
        "assignment worktree cleanup failed: {workspace}; assignment staging cleanup failed: {staging}"
    )]
    CleanupFailures { workspace: String, staging: String },
}

/// Materializes and launches exactly one durable assignment. The resolver is
/// invoked once before packet construction and again through the live session
/// runtime for Engineering/Quality, so stale target state fails closed on both
/// sides of the persistence boundary.
pub async fn materialize_and_launch_assignment(
    store: &KernelStore,
    cas: &CasStore,
    daemon: &LocalDaemon,
    installed: &InstalledKernelBuildReceiptV2,
    execution: &InstalledKernelExecutionTools,
    resolver: Arc<DurableAuthorityResolver>,
    request: AssignmentMaterializationRequest,
) -> Result<AssignmentLaunchOutcome, AssignmentRuntimeError> {
    let process = store.process_store();
    let identity = process.reserve_assignment_identity().await?;
    let assignment_id = identity.assignment_id();
    // The database requires an ordinal unique within its campaign. The
    // kernel-assigned assignment sequence is already strictly unique and
    // monotonic, so deriving this opaque ordering token here avoids a
    // scheduler-side MAX/read race or caller-selected ordinal.
    let attempt_ordinal = u32::try_from(assignment_id.get()).map_err(|_| {
        AssignmentRuntimeError::Application(
            "assignment identity exceeds the supported assignment ordinal range".to_owned(),
        )
    })?;
    let context = resolver
        .resolve_pre_assignment_launch(DurableAssignmentLaunchRequest {
            campaign_id: request.campaign_id,
            application_revision_id: request.application_revision_id,
            target: request.target,
        })
        .await
        .map_err(AssignmentRuntimeError::Target)?;

    if context.application_revision_id != request.application_revision_id
        || context.target != request.target
    {
        return Err(AssignmentRuntimeError::Application(
            "pre-assignment context does not match the durable request".to_owned(),
        ));
    }

    let staging_root = create_assignment_staging(cas.runtime_root(), assignment_id)?;
    let workspace =
        match materialize_workspace(execution.git_custody().as_ref(), &context, assignment_id) {
            Ok(workspace) => workspace,
            Err(error) => {
                return Err(with_cleanup_failure(
                    error,
                    remove_assignment_staging(cas.runtime_root(), assignment_id, &staging_root),
                ));
            }
        };
    let resources = AssignmentRuntimeResources {
        assignment_id,
        workspace,
        staging_root,
    };

    let launch = async {
        let assignment_role = office_for_target(request.target);
        let application = load_application_material(
            store,
            &process,
            cas,
            request.application_revision_id,
            assignment_role,
        )
        .await?;
        let office_id = store
            .harness_store()
            .active_office(request.application_revision_id, assignment_role)
            .await?;

        let required_reads = exact_required_reads(
            &context.application_required_reads,
            &context.ticket_contract_reads,
            resources.workspace.path(),
        )?;
        let required_manifest_bytes = canonical_required_manifest(&required_reads);
        let required_manifest = register_kernel_bytes(
            &process,
            cas,
            installed.kernel_build_id(),
            assignment_id,
            "required-read-manifest",
            &required_manifest_bytes,
        )
        .await?;

        let assignment_evidence = exact_assignment_evidence(request.target, &context)?;
        let campaign = current_campaign_material(
            store,
            request.campaign_id,
            request.application_revision_id,
            request.expected_campaign_revision,
        )
        .await?;
        let target_facts = harness_target_facts(request.target, &context)?;
        let compiled_harness = compile_harness(HarnessCompileInput {
            assignment_id,
            application_revision_id: request.application_revision_id,
            office_id,
            assignment_role,
            target_facts: &target_facts,
            assignment_evidence: &assignment_evidence,
            required_reads: &required_reads,
            required_manifest_artifact_id: required_manifest.artifact_id,
            remaining_campaign_allowance: campaign.remaining,
            application: &application,
        })?;
        let harness_spec = register_kernel_bytes(
            &process,
            cas,
            installed.kernel_build_id(),
            assignment_id,
            "harness-spec",
            &compiled_harness.canonical_spec_bytes,
        )
        .await?;
        let system = register_kernel_bytes(
            &process,
            cas,
            installed.kernel_build_id(),
            assignment_id,
            "system-prompt",
            &compiled_harness.system_prompt,
        )
        .await?;
        let assignment_prompt_artifact = register_kernel_bytes(
            &process,
            cas,
            installed.kernel_build_id(),
            assignment_id,
            "assignment-prompt",
            &compiled_harness.assignment_prompt,
        )
        .await?;
        let runtime =
            installed.runtime_identity_for_provider(&application.profile.model.provider)?;
        let workspace_root = absolute_path(resources.workspace.path(), "workspace root")?;
        let staging_absolute = absolute_path(&resources.staging_root, "staging root")?;
        let mut wire = assignment_wire(
            assignment_id,
            request.campaign_id,
            request.application_revision_id,
            assignment_packet_kernel_build(&campaign),
            request.target,
            compiled_harness.target.clone(),
            &context,
            system.artifact_id,
            assignment_prompt_artifact.artifact_id,
            required_manifest.artifact_id,
            &compiled_harness.system_prompt,
            &compiled_harness.assignment_prompt,
            workspace_root.as_str(),
            staging_absolute.as_str(),
            &application.profile,
            &application.policy_bytes,
            &runtime,
            &required_reads,
            &assignment_evidence,
            campaign.remaining,
            campaign.revision,
        )?;
        let packet_digest = unsigned_assignment_packet_digest_v2(&wire)?;
        wire.packet_digest = packet_digest.to_hex();
        let packet_bytes = canonical_assignment_packet_json_v2(&wire)?.into_bytes();
        let packet = typed_packet(
            assignment_id,
            request.campaign_id,
            request.application_revision_id,
            assignment_packet_kernel_build(&campaign),
            request.target,
            compiled_harness.target.clone(),
            system.artifact_id,
            assignment_prompt_artifact.artifact_id,
            required_manifest.artifact_id,
            workspace_root,
            staging_absolute,
            application.profile.model.clone(),
            application.profile.limits.clone(),
            application.profile.policy.clone(),
            application.policy_bytes.clone(),
            runtime,
            required_reads.clone(),
            assignment_evidence,
            campaign.remaining,
            campaign.revision,
            packet_digest,
        );
        let packet_artifact = register_kernel_bytes(
            &process,
            cas,
            installed.kernel_build_id(),
            assignment_id,
            "assignment-packet",
            &packet_bytes,
        )
        .await?;
        let assignment_receipt = process
            .create_assignment(
                cas,
                &CreateAssignment {
                    principal: request.principal,
                    command_id: request.command_id,
                    expected_campaign_revision: request.expected_campaign_revision,
                    identity,
                    packet: packet.clone(),
                    packet_bytes: packet_bytes.clone(),
                    packet_artifact: packet_artifact.seal,
                    required_read_manifest_artifact_id: required_manifest.artifact_id,
                    attempt_ordinal,
                    harness: Some(RecordHarnessCompilation {
                        assignment_id,
                        application_revision_id: request.application_revision_id,
                        office_id,
                        assignment_role,
                        compiler_version: HARNESS_COMPILER_VERSION_V2,
                        spec_artifact_id: harness_spec.artifact_id,
                        system_prompt_artifact_id: system.artifact_id,
                        assignment_prompt_artifact_id: assignment_prompt_artifact.artifact_id,
                        packet_artifact_id: packet_artifact.artifact_id,
                        packet_digest,
                        context_items: compiled_harness.spec.context_items,
                    }),
                },
            )
            .await?;
        if assignment_receipt.assignment_id != assignment_id {
            return Err(AssignmentRuntimeError::Application(
                "assignment receipt changed the reserved identity".to_owned(),
            ));
        }

        let spawn = installed.pi_host_spawn_spec_for_provider(
            &application.profile.model.provider,
            resources.workspace.path().to_owned(),
            0,
            (
                OsString::from(installed.openrouter_credential_environment()),
                request.credential_environment_value,
            ),
        )?;
        let supervision = ProcessSupervisionSpec::new(
            resources.staging_root.join(SESSION_STDOUT_RELATIVE_PATH),
            resources.staging_root.join(SESSION_STDERR_RELATIVE_PATH),
            u64::from(application.profile.limits.output_byte_limit),
            u64::from(application.profile.limits.output_byte_limit),
            Duration::from_millis(application.profile.limits.wall_limit.get()),
            TERMINATION_GRACE,
        )?;
        let candidate_quality_runtime = match request.target {
            DurableAssignmentTarget::Product => None,
            DurableAssignmentTarget::Engineering { .. }
            | DurableAssignmentTarget::Quality { .. } => Some(CandidateQualitySessionRuntime::new(
                store.decision_store(),
                execution.git_custody(),
                resolver,
            )),
        };
        let session = launch_session(
            &process,
            &store.forum_store(),
            &store.publication_store(),
            &store.ticket_store(),
            execution.command_runner(),
            daemon,
            cas,
            SessionLaunchRequest {
                principal: KERNEL_PRINCIPAL.to_owned(),
                command_id: format!("session-launch-{}", assignment_id.get()),
                expected_assignment_revision: ExpectedRevision::new(
                    assignment_receipt.resulting_revision,
                ),
                assignment_id,
                packet_digest,
                packet,
                canonical_packet_bytes: packet_bytes,
                packet_artifact: packet_artifact.seal,
                spawn,
                supervision,
                workspace_root: resources.workspace.path().to_owned(),
                expected_read_manifest_artifact_id: required_manifest.artifact_id,
                required_reads,
                candidate_quality_runtime,
            },
            installed,
        )
        .await?;
        Ok((assignment_receipt.resulting_revision, session))
    }
    .await;
    let cleanup = resources.cleanup(execution.git_custody().as_ref(), cas.runtime_root());
    match (launch, cleanup) {
        (Ok((assignment_revision, session)), Ok(())) => Ok(AssignmentLaunchOutcome {
            assignment_id,
            assignment_revision,
            session,
        }),
        (Err(error), Ok(())) | (Ok(_), Err(error)) => Err(error),
        (Err(launch), Err(cleanup)) => Err(AssignmentRuntimeError::LaunchCleanup {
            launch: launch.to_string(),
            cleanup: cleanup.to_string(),
        }),
    }
}

struct RegisteredKernelBytes {
    seal: crate::cas::CasArtifact,
    artifact_id: factory_protocol::ArtifactId,
}

async fn register_kernel_bytes(
    process: &ProcessStore,
    cas: &CasStore,
    build: factory_protocol::KernelBuildId,
    assignment_id: AssignmentId,
    purpose: &'static str,
    bytes: &[u8],
) -> Result<RegisteredKernelBytes, AssignmentRuntimeError> {
    let digest = ContentDigest::of_bytes(bytes);
    let command_id = format!(
        "assignment-{}-{}-{}",
        assignment_id.get(),
        purpose,
        &digest.to_hex()[..16]
    );
    let (seal, receipt) = process
        .adopt_and_register_kernel_bytes(cas, KERNEL_PRINCIPAL, &command_id, build, bytes)
        .await?;
    Ok(RegisteredKernelBytes {
        seal,
        artifact_id: receipt.artifact_id,
    })
}

fn materialize_workspace(
    git: &GitCustody,
    context: &DurableAssignmentLaunchContext,
    assignment_id: AssignmentId,
) -> Result<OwnedWorktree, AssignmentRuntimeError> {
    let name = WorktreeName::parse(format!("assignment-{}", assignment_id.get()))?;
    match context.target {
        DurableAssignmentTarget::Product | DurableAssignmentTarget::Engineering { .. } => git
            .create_detached_worktree(&context.repository, WorktreeKind::Actor, name)
            .map_err(Into::into),
        DurableAssignmentTarget::Quality { .. } => git
            .create_candidate_review_worktree(
                &context.repository,
                context.materialize_commit.clone(),
                context.materialize_tree.clone(),
                name,
            )
            .map_err(Into::into),
    }
}

fn create_assignment_staging(
    runtime_root: &Path,
    assignment_id: AssignmentId,
) -> Result<PathBuf, AssignmentRuntimeError> {
    let parent = runtime_root.join("staging");
    fs::create_dir_all(&parent).map_err(|source| AssignmentRuntimeError::Io {
        operation: "create assignment staging parent",
        path: parent.clone(),
        source,
    })?;
    let staging = assignment_staging_path(runtime_root, assignment_id);
    fs::create_dir(&staging).map_err(|source| AssignmentRuntimeError::Io {
        operation: "create fresh assignment staging root",
        path: staging.clone(),
        source,
    })?;
    fs::canonicalize(&staging).map_err(|source| AssignmentRuntimeError::Io {
        operation: "canonicalize assignment staging root",
        path: staging,
        source,
    })
}

fn assignment_staging_path(runtime_root: &Path, assignment_id: AssignmentId) -> PathBuf {
    runtime_root
        .join("staging")
        .join(format!("assignment-{}", assignment_id.get()))
}

/// Removes only the exact staging directory created for one assignment. CAS
/// adoption has already made completed evidence independent of this directory.
fn remove_assignment_staging(
    runtime_root: &Path,
    assignment_id: AssignmentId,
    staging_root: &Path,
) -> Result<(), AssignmentRuntimeError> {
    let expected = assignment_staging_path(runtime_root, assignment_id);
    if staging_root != expected {
        return Err(AssignmentRuntimeError::StagingRootMismatch {
            expected,
            observed: staging_root.to_owned(),
        });
    }
    let metadata = match fs::symlink_metadata(staging_root) {
        Ok(metadata) => metadata,
        Err(source) if source.kind() == io::ErrorKind::NotFound => return Ok(()),
        Err(source) => {
            return Err(AssignmentRuntimeError::Io {
                operation: "inspect assignment staging root for cleanup",
                path: staging_root.to_owned(),
                source,
            });
        }
    };
    if !metadata.is_dir() {
        return Err(AssignmentRuntimeError::StagingRootNotDirectory {
            path: staging_root.to_owned(),
        });
    }
    fs::remove_dir_all(staging_root).map_err(|source| AssignmentRuntimeError::Io {
        operation: "remove exact assignment staging root",
        path: staging_root.to_owned(),
        source,
    })?;
    if staging_root.exists() {
        return Err(AssignmentRuntimeError::StagingCleanupIncomplete {
            path: staging_root.to_owned(),
        });
    }
    Ok(())
}

fn with_cleanup_failure(
    launch: AssignmentRuntimeError,
    cleanup: Result<(), AssignmentRuntimeError>,
) -> AssignmentRuntimeError {
    match cleanup {
        Ok(()) => launch,
        Err(cleanup) => AssignmentRuntimeError::LaunchCleanup {
            launch: launch.to_string(),
            cleanup: cleanup.to_string(),
        },
    }
}

struct ApplicationMaterial {
    mission: String,
    system_template: factory_protocol::TemplateArtifactV2,
    system_source: String,
    assignment_template: factory_protocol::TemplateArtifactV2,
    assignment_source: String,
    profile: factory_protocol::AssignmentRoleProfileV2,
    /// Policy bytes are loaded from the admitted CAS digest. The host packet
    /// carries these bytes inline so it never consults application source
    /// paths after admission.
    policy_bytes: Vec<u8>,
}

async fn load_application_material(
    store: &KernelStore,
    process: &ProcessStore,
    cas: &CasStore,
    application_revision_id: ApplicationRevisionId,
    assignment_role: AssignmentRole,
) -> Result<ApplicationMaterial, AssignmentRuntimeError> {
    let row = sqlx::query!(
        "SELECT bundle_artifact_id, mission_artifact_id,
                product_research_system_template_artifact_id,
                product_research_assignment_template_artifact_id,
                engineering_system_template_artifact_id,
                engineering_assignment_template_artifact_id,
                quality_system_template_artifact_id,
                quality_assignment_template_artifact_id
           FROM factory.application_revisions WHERE id = $1",
        application_revision_id.get(),
    )
    .fetch_optional(&store.pool_for_authority())
    .await?
    .ok_or(StoreError::UnknownApplicationRevision {
        application_revision_id,
    })?;
    let artifact = |value: i64| -> Result<factory_protocol::ArtifactId, AssignmentRuntimeError> {
        factory_protocol::ArtifactId::new(value).map_err(Into::into)
    };
    let bundle = registered_bytes(process, cas, artifact(row.bundle_artifact_id)?).await?;
    let bundle = parse_application_bundle_v2(&bundle).map_err(|error| {
        AssignmentRuntimeError::Application(format!("admitted bundle is invalid: {error}"))
    })?;
    let profile = bundle
        .assignment_role_profiles
        .iter()
        .find(|profile| profile.assignment_role == assignment_role)
        .cloned()
        .ok_or_else(|| {
            AssignmentRuntimeError::Application(
                "admitted bundle lacks selected assignment role".to_owned(),
            )
        })?;
    let (system_id, assignment_id) = match assignment_role {
        AssignmentRole::ProductResearch => (
            artifact(row.product_research_system_template_artifact_id)?,
            artifact(row.product_research_assignment_template_artifact_id)?,
        ),
        AssignmentRole::Engineering => (
            artifact(row.engineering_system_template_artifact_id)?,
            artifact(row.engineering_assignment_template_artifact_id)?,
        ),
        AssignmentRole::Quality => (
            artifact(row.quality_system_template_artifact_id)?,
            artifact(row.quality_assignment_template_artifact_id)?,
        ),
    };
    let mission = checked_template_bytes(
        registered_bytes(process, cas, artifact(row.mission_artifact_id)?).await?,
        &bundle.mission_template,
        "mission template",
    )?;
    let system_source = checked_template_bytes(
        registered_bytes(process, cas, system_id).await?,
        &profile.system_template,
        "system template",
    )?;
    let assignment_source = checked_template_bytes(
        registered_bytes(process, cas, assignment_id).await?,
        &profile.assignment_template,
        "assignment template",
    )?;
    let policy_bytes = cas.read_verified(profile.policy.digest)?;
    if policy_bytes.len() > profile.policy.byte_limit as usize
        || ContentDigest::of_bytes(&policy_bytes) != profile.policy.digest
    {
        return Err(AssignmentRuntimeError::Application(
            "admitted policy bytes differ from its declared digest or limit".to_owned(),
        ));
    }
    let mission = render_declared_template(&bundle.mission_template, &mission, &BTreeMap::new())?;
    let mission = String::from_utf8(mission).map_err(|_| {
        AssignmentRuntimeError::Application("rendered mission is not UTF-8".to_owned())
    })?;
    Ok(ApplicationMaterial {
        mission,
        system_template: profile.system_template.clone(),
        system_source,
        assignment_template: profile.assignment_template.clone(),
        assignment_source,
        profile,
        policy_bytes,
    })
}

async fn registered_bytes(
    process: &ProcessStore,
    cas: &CasStore,
    artifact_id: factory_protocol::ArtifactId,
) -> Result<Vec<u8>, AssignmentRuntimeError> {
    let seal = process.registered_artifact(cas, artifact_id).await?;
    Ok(cas.read_verified(seal.digest())?)
}

fn checked_template_bytes(
    bytes: Vec<u8>,
    template: &factory_protocol::TemplateArtifactV2,
    label: &'static str,
) -> Result<String, AssignmentRuntimeError> {
    if ContentDigest::of_bytes(&bytes) != template.digest {
        return Err(AssignmentRuntimeError::Application(format!(
            "{label} digest differs from admitted bundle"
        )));
    }
    String::from_utf8(bytes)
        .map_err(|_| AssignmentRuntimeError::Application(format!("{label} is not UTF-8")))
}

fn exact_required_reads(
    application_required_reads: &[RequiredReadV2],
    ticket_contract_reads: &[TicketContractReadV2],
    workspace: &Path,
) -> Result<Vec<ReadExactFileV2>, AssignmentRuntimeError> {
    let mut values = Vec::new();
    for read in application_required_reads {
        values.push((read.path.clone(), read.reason.clone()));
    }
    for read in ticket_contract_reads {
        values.push((read.path.clone(), read.reason.clone()));
    }
    let mut paths = BTreeSet::new();
    let mut result = Vec::with_capacity(values.len());
    for (path, reason) in values {
        if !paths.insert(path.clone()) {
            // Each source is admitted with unique paths. Across sources, the
            // same materialized file is one exact read obligation, not an
            // inconsistency: the application wording wins deterministically
            // and its digest proves both the application and ticket contract.
            continue;
        }
        result.push(ReadExactFileV2 {
            digest: WorkspaceReadAuthority::digest_materialized_required_read(
                workspace,
                path.clone(),
            )?,
            path,
            reason,
        });
    }
    result.sort_by(|left, right| left.path.as_str().cmp(right.path.as_str()));
    Ok(result)
}

/// The compiler needs only these target facts to render a closed harness.
/// The broader resolver context continues to own worktree and evidence
/// materialization, but it is not passed through prompt construction.
#[derive(Clone, Debug)]
enum HarnessTargetFacts {
    Product,
    Engineering {
        ticket_attempt_id: factory_protocol::TicketAttemptId,
        ticket_id: factory_protocol::TicketId,
        ticket_revision_id: factory_protocol::TicketRevisionId,
        checkpoint: crate::durable_authority::EngineeringCheckpointContract,
    },
    Quality {
        ticket_attempt_id: factory_protocol::TicketAttemptId,
        candidate_id: factory_protocol::CandidateId,
        ticket_id: factory_protocol::TicketId,
        ticket_revision_id: factory_protocol::TicketRevisionId,
        validation_id: factory_protocol::ValidationId,
    },
}

fn harness_target_facts(
    target: DurableAssignmentTarget,
    context: &DurableAssignmentLaunchContext,
) -> Result<HarnessTargetFacts, AssignmentRuntimeError> {
    match target {
        DurableAssignmentTarget::Product => Ok(HarnessTargetFacts::Product),
        DurableAssignmentTarget::Engineering { ticket_attempt_id } => {
            Ok(HarnessTargetFacts::Engineering {
                ticket_attempt_id,
                ticket_id: context.ticket_id.ok_or_else(|| {
                    AssignmentRuntimeError::Application(
                        "Engineering target lacks ticket ID".to_owned(),
                    )
                })?,
                ticket_revision_id: context.ticket_revision_id.ok_or_else(|| {
                    AssignmentRuntimeError::Application(
                        "Engineering target lacks ticket revision ID".to_owned(),
                    )
                })?,
                checkpoint: context.engineering_checkpoint.clone().ok_or_else(|| {
                    AssignmentRuntimeError::Application(
                        "Engineering target lacks its checkpoint contract".to_owned(),
                    )
                })?,
            })
        }
        DurableAssignmentTarget::Quality {
            ticket_attempt_id,
            candidate_id,
        } => Ok(HarnessTargetFacts::Quality {
            ticket_attempt_id,
            candidate_id,
            ticket_id: context.ticket_id.ok_or_else(|| {
                AssignmentRuntimeError::Application("Quality target lacks ticket ID".to_owned())
            })?,
            ticket_revision_id: context.ticket_revision_id.ok_or_else(|| {
                AssignmentRuntimeError::Application(
                    "Quality target lacks ticket revision ID".to_owned(),
                )
            })?,
            validation_id: context.validation_id.ok_or_else(|| {
                AssignmentRuntimeError::Application(
                    "Quality target lacks hard validation ID".to_owned(),
                )
            })?,
        }),
    }
}

fn prompt_values(
    assignment_id: AssignmentId,
    target: &str,
    target_facts: &HarnessTargetFacts,
    mission: &str,
) -> Result<BTreeMap<String, String>, AssignmentRuntimeError> {
    let mut values = BTreeMap::from([
        ("ASSIGNMENT_ID".to_owned(), assignment_id.get().to_string()),
        ("MISSION".to_owned(), mission.to_owned()),
        ("TARGET".to_owned(), target.to_owned()),
    ]);
    match target_facts {
        HarnessTargetFacts::Product => {}
        HarnessTargetFacts::Engineering {
            ticket_id,
            ticket_revision_id,
            checkpoint,
            ..
        } => {
            values.insert("TICKET_ID".to_owned(), ticket_id.get().to_string());
            values.insert(
                "TICKET_REVISION_ID".to_owned(),
                ticket_revision_id.get().to_string(),
            );
            values.insert(
                "REGRESSION_COMMAND".to_owned(),
                checkpoint.regression_command.clone(),
            );
            values.insert(
                "REGRESSION_EXPECTED_FAILURE".to_owned(),
                checkpoint.expected_failure.clone(),
            );
        }
        HarnessTargetFacts::Quality {
            ticket_id,
            ticket_revision_id,
            candidate_id,
            validation_id,
            ..
        } => {
            values.insert("TICKET_ID".to_owned(), ticket_id.get().to_string());
            values.insert(
                "TICKET_REVISION_ID".to_owned(),
                ticket_revision_id.get().to_string(),
            );
            values.insert("CANDIDATE_ID".to_owned(), candidate_id.get().to_string());
            values.insert("VALIDATION_ID".to_owned(), validation_id.get().to_string());
        }
    }
    Ok(values)
}

fn render_declared_template(
    template: &factory_protocol::TemplateArtifactV2,
    source: &str,
    values: &BTreeMap<String, String>,
) -> Result<Vec<u8>, AssignmentRuntimeError> {
    let values = template
        .placeholders
        .iter()
        .map(|placeholder| {
            let value = values.get(placeholder.as_str()).ok_or_else(|| {
                AssignmentRuntimeError::Application(format!(
                    "template requires unavailable placeholder {}",
                    placeholder.as_str()
                ))
            })?;
            Ok((placeholder.as_str().to_owned(), value.clone()))
        })
        .collect::<Result<BTreeMap<_, _>, AssignmentRuntimeError>>()?;
    Ok(render_template_v2(template, source, &values)?)
}

/// Closed compiler input. All members are resolved durable facts or admitted
/// policy; this boundary deliberately has no actor text, callback, or
/// untyped context map.
struct HarnessCompileInput<'a> {
    assignment_id: AssignmentId,
    application_revision_id: ApplicationRevisionId,
    office_id: OfficeId,
    assignment_role: AssignmentRole,
    target_facts: &'a HarnessTargetFacts,
    assignment_evidence: &'a [AssignmentEvidenceV2],
    required_reads: &'a [ReadExactFileV2],
    required_manifest_artifact_id: factory_protocol::ArtifactId,
    remaining_campaign_allowance: MicroUsd,
    application: &'a ApplicationMaterial,
}

struct CompiledHarness {
    spec: HarnessSpecV2,
    canonical_spec_bytes: Vec<u8>,
    target: String,
    system_prompt: Vec<u8>,
    assignment_prompt: Vec<u8>,
}

/// The only assignment-prompt compiler. It turns resolved target facts and
/// already-admitted application templates into an inspectable, bounded
/// harness before anything reaches a fungible actor invocation.
fn compile_harness(
    input: HarnessCompileInput<'_>,
) -> Result<CompiledHarness, AssignmentRuntimeError> {
    let target = target_text(
        input.target_facts,
        input.assignment_evidence,
        input.required_reads,
    )?;
    let mut context_items = vec![ContextItemV2 {
        reference: ContextReferenceV2::Office(input.office_id),
        inclusion: ContextInclusionClassV2::DirectTarget,
        reason: "the admitted office owns this invocation".to_owned(),
    }];
    match input.target_facts {
        HarnessTargetFacts::Product => {}
        HarnessTargetFacts::Engineering {
            ticket_id,
            ticket_revision_id,
            ..
        }
        | HarnessTargetFacts::Quality {
            ticket_id,
            ticket_revision_id,
            ..
        } => {
            context_items.push(ContextItemV2 {
                reference: ContextReferenceV2::Ticket(*ticket_id),
                inclusion: ContextInclusionClassV2::DirectTarget,
                reason: "the selected ticket is the direct assignment target".to_owned(),
            });
            context_items.push(ContextItemV2 {
                reference: ContextReferenceV2::TicketRevision(*ticket_revision_id),
                inclusion: ContextInclusionClassV2::DirectTarget,
                reason: "the selected ticket revision is the direct assignment target".to_owned(),
            });
        }
    }
    context_items.push(ContextItemV2 {
        reference: ContextReferenceV2::Artifact(input.required_manifest_artifact_id),
        inclusion: ContextInclusionClassV2::RequiredConstraint,
        reason: "exact workspace reads are required before mutation".to_owned(),
    });
    for evidence in input.assignment_evidence {
        context_items.push(ContextItemV2 {
            reference: ContextReferenceV2::Artifact(evidence.artifact_id),
            inclusion: ContextInclusionClassV2::DirectEvidence,
            reason: format!("direct {} evidence", evidence.role.wire_name()),
        });
    }
    // Several closed evidence roles may intentionally name one immutable
    // artifact. A harness lists a durable reference once: selection priority
    // is deterministic (target, required constraint, then role-sorted
    // evidence), while the sealed packet retains every role-specific proof.
    let mut selected_references = BTreeSet::new();
    context_items.retain(|item| selected_references.insert(item.reference));
    let spec = HarnessSpecV2 {
        compiler_version: HARNESS_COMPILER_VERSION_V2,
        application_revision_id: input.application_revision_id,
        office_id: input.office_id,
        assignment_role: input.assignment_role,
        objective: target.clone(),
        context_items,
        capabilities: input.application.profile.tools.clone(),
        remaining_campaign_allowance: input.remaining_campaign_allowance,
    };
    spec.validate()?;
    let values = prompt_values(
        input.assignment_id,
        &target,
        input.target_facts,
        &input.application.mission,
    )?;
    let system_prompt = render_declared_template(
        &input.application.system_template,
        &input.application.system_source,
        &values,
    )?;
    let assignment_prompt = render_declared_template(
        &input.application.assignment_template,
        &input.application.assignment_source,
        &values,
    )?;
    Ok(CompiledHarness {
        canonical_spec_bytes: canonical_harness_spec_json(&spec).into_bytes(),
        spec,
        target,
        system_prompt,
        assignment_prompt,
    })
}

#[derive(Serialize)]
struct HarnessSpecWire<'a> {
    compiler_version: u16,
    application_revision_id: i64,
    office_id: i64,
    assignment_role: &'a str,
    objective: &'a str,
    context_items: Vec<HarnessContextItemWire<'a>>,
    capabilities: Vec<&'a str>,
    remaining_campaign_allowance_micro_usd: u64,
}

#[derive(Serialize)]
struct HarnessContextItemWire<'a> {
    reference_kind: &'a str,
    reference_id: i64,
    inclusion_class: &'a str,
    reason: &'a str,
}

/// A fixed-field, ordered DTO makes the sealed spec stable across retries.
/// This is an artifact format, not an extensible external API.
fn canonical_harness_spec_json(spec: &HarnessSpecV2) -> String {
    let context_items = spec
        .context_items
        .iter()
        .map(|item| {
            let (reference_kind, reference_id) = harness_reference_parts(item.reference);
            HarnessContextItemWire {
                reference_kind,
                reference_id,
                inclusion_class: harness_inclusion_name(item.inclusion),
                reason: &item.reason,
            }
        })
        .collect();
    let capabilities = spec
        .capabilities
        .iter()
        .map(|tool| tool_name(*tool))
        .collect();
    json::to_string(&HarnessSpecWire {
        compiler_version: spec.compiler_version,
        application_revision_id: spec.application_revision_id.get(),
        office_id: spec.office_id.get(),
        assignment_role: office_name(spec.assignment_role),
        objective: &spec.objective,
        context_items,
        capabilities,
        remaining_campaign_allowance_micro_usd: spec.remaining_campaign_allowance.get(),
    })
}

fn harness_reference_parts(reference: ContextReferenceV2) -> (&'static str, i64) {
    match reference {
        ContextReferenceV2::Artifact(id) => ("artifact", id.get()),
        ContextReferenceV2::Project(id) => ("project", id.get()),
        ContextReferenceV2::Rfc(id) => ("rfc", id.get()),
        ContextReferenceV2::RfcRevision(id) => ("rfc_revision", id.get()),
        ContextReferenceV2::Ticket(id) => ("ticket", id.get()),
        ContextReferenceV2::TicketRevision(id) => ("ticket_revision", id.get()),
        ContextReferenceV2::Experiment(id) => ("experiment", id.get()),
        ContextReferenceV2::Claim(id) => ("claim", id.get()),
        ContextReferenceV2::Decision(id) => ("decision", id.get()),
        ContextReferenceV2::Office(id) => ("office", id.get()),
    }
}

const fn harness_inclusion_name(value: ContextInclusionClassV2) -> &'static str {
    match value {
        ContextInclusionClassV2::DirectTarget => "direct_target",
        ContextInclusionClassV2::RequiredConstraint => "required_constraint",
        ContextInclusionClassV2::DirectEvidence => "direct_evidence",
        ContextInclusionClassV2::CurrentDecision => "current_decision",
    }
}

struct CampaignMaterial {
    revision: AggregateRevision,
    remaining: MicroUsd,
    kernel_build_id: factory_protocol::KernelBuildId,
}

async fn current_campaign_material(
    store: &KernelStore,
    campaign_id: CampaignId,
    application_revision_id: ApplicationRevisionId,
    expected: ExpectedRevision,
) -> Result<CampaignMaterial, AssignmentRuntimeError> {
    let row = sqlx::query!(
        "SELECT c.revision, c.aggregate_budget_micro_usd, c.measured_cost_micro_usd,
                c.application_revision_id, c.lifecycle, c.cost_state, kb.build_digest
           FROM factory.campaigns c
           JOIN factory.kernel_builds kb ON kb.id = c.kernel_build_id
          WHERE c.id = $1",
        campaign_id.get(),
    )
    .fetch_optional(&store.pool_for_authority())
    .await?
    .ok_or(StoreError::UnknownCampaign { campaign_id })?;
    let revision = u64::try_from(row.revision).map_err(|_| {
        AssignmentRuntimeError::Application("campaign revision is negative".to_owned())
    })?;
    let revision = AggregateRevision::from_persisted(revision);
    if expected.get() != revision
        || row.application_revision_id != application_revision_id.get()
        || row.lifecycle != 0
        || row.cost_state != 0
    {
        return Err(AssignmentRuntimeError::Application(
            "campaign changed before assignment packet construction".to_owned(),
        ));
    }
    let budget = u64::try_from(row.aggregate_budget_micro_usd).map_err(|_| {
        AssignmentRuntimeError::Application("campaign budget is corrupt".to_owned())
    })?;
    let measured = u64::try_from(row.measured_cost_micro_usd)
        .map_err(|_| AssignmentRuntimeError::Application("campaign cost is corrupt".to_owned()))?;
    let remaining = budget.checked_sub(measured).ok_or_else(|| {
        AssignmentRuntimeError::Application("campaign measured cost exceeds budget".to_owned())
    })?;
    if remaining == 0 {
        return Err(AssignmentRuntimeError::Application(
            "campaign has no remaining assignment allowance".to_owned(),
        ));
    }
    let kernel_build_id = factory_protocol::KernelBuildId::new(ContentDigest::from_bytes(
        row.build_digest.as_slice().try_into().map_err(|_| {
            AssignmentRuntimeError::Application("campaign kernel digest is corrupt".to_owned())
        })?,
    ));
    Ok(CampaignMaterial {
        revision,
        remaining: MicroUsd::new(remaining),
        kernel_build_id,
    })
}

/// The packet is a campaign-lineage record. A recovered controller can be a
/// newer installed build, but it must not silently rewrite the identity that
/// was admitted with the campaign while materializing a fresh assignment.
fn assignment_packet_kernel_build(campaign: &CampaignMaterial) -> factory_protocol::KernelBuildId {
    campaign.kernel_build_id
}

#[allow(clippy::too_many_arguments)]
fn assignment_wire(
    assignment_id: AssignmentId,
    campaign_id: CampaignId,
    application_revision_id: ApplicationRevisionId,
    build_id: factory_protocol::KernelBuildId,
    target_kind: DurableAssignmentTarget,
    target: String,
    context: &DurableAssignmentLaunchContext,
    system_prompt_artifact_id: factory_protocol::ArtifactId,
    assignment_prompt_artifact_id: factory_protocol::ArtifactId,
    required_read_manifest_artifact_id: factory_protocol::ArtifactId,
    system_prompt: &[u8],
    assignment_prompt: &[u8],
    workspace_root: &str,
    staging_root: &str,
    profile: &factory_protocol::AssignmentRoleProfileV2,
    policy_bytes: &[u8],
    runtime: &factory_protocol::RuntimeIdentityV2,
    required_reads: &[ReadExactFileV2],
    assignment_evidence: &[AssignmentEvidenceV2],
    remaining: MicroUsd,
    revision: AggregateRevision,
) -> Result<AssignmentPacketWireV2, AssignmentRuntimeError> {
    Ok(AssignmentPacketWireV2 {
        format_version: factory_protocol::ASSIGNMENT_PACKET_V2_FORMAT,
        campaign_id: campaign_id.get(),
        assignment_id: assignment_id.get(),
        application_revision_id: application_revision_id.get(),
        kernel_build_id: build_id.digest().to_hex(),
        assignment_role: office_name(office_for_target(target_kind)).to_owned(),
        target,
        repository_base_identity: repository_identity(context),
        factory_base_identity: build_id.digest().to_hex(),
        ticket_attempt_id: ticket_attempt(target_kind).map(factory_protocol::TicketAttemptId::get),
        candidate_id: candidate(target_kind).map(factory_protocol::CandidateId::get),
        system_prompt_artifact_id: system_prompt_artifact_id.get(),
        assignment_prompt_artifact_id: assignment_prompt_artifact_id.get(),
        required_read_manifest_artifact_id: required_read_manifest_artifact_id.get(),
        system_prompt_digest: ContentDigest::of_bytes(system_prompt).to_hex(),
        assignment_prompt_digest: ContentDigest::of_bytes(assignment_prompt).to_hex(),
        system_prompt_bytes_b64: base64(system_prompt),
        assignment_prompt_bytes_b64: base64(assignment_prompt),
        policy_digest: profile.policy.digest.to_hex(),
        policy_byte_limit: profile.policy.byte_limit,
        policy_bytes_b64: base64(policy_bytes),
        policy_entrypoint: profile.policy.entrypoint.as_str().to_owned(),
        workspace_root: workspace_root.to_owned(),
        staging_root: staging_root.to_owned(),
        model: AssignmentModelWireV2 {
            provider: profile.model.provider.clone(),
            model_id: profile.model.model_id.clone(),
            thinking_level: thinking_name(profile.model.thinking_level).to_owned(),
            context_token_limit: profile.model.context_token_limit,
            output_token_limit: profile.model.output_token_limit,
            price_input_micro_usd_per_million_tokens: profile
                .model
                .price_input_micro_usd_per_million_tokens
                .get(),
            price_output_micro_usd_per_million_tokens: profile
                .model
                .price_output_micro_usd_per_million_tokens
                .get(),
            price_cache_read_micro_usd_per_million_tokens: profile
                .model
                .price_cache_read_micro_usd_per_million_tokens
                .get(),
            price_cache_write_micro_usd_per_million_tokens: profile
                .model
                .price_cache_write_micro_usd_per_million_tokens
                .get(),
            capability_flags: profile
                .model
                .capability_flags
                .iter()
                .map(|flag| match flag {
                    factory_protocol::ModelCapabilityV2::Reasoning => "reasoning".to_owned(),
                })
                .collect(),
        },
        limits: AssignmentLimitsWireV2 {
            turn_limit: profile.limits.turn_limit,
            wall_limit_millis: profile.limits.wall_limit.get(),
            output_byte_limit: profile.limits.output_byte_limit,
        },
        runtime: AssignmentRuntimeWireV2 {
            host_executable: runtime.host_executable.as_str().to_owned(),
            core_head: runtime.core_head.clone(),
            core_source_digest: runtime.core_source_digest.to_hex(),
            rust_toolchain: runtime.rust_toolchain.clone(),
            credential_env: runtime.credential_env.clone(),
        },
        required_reads: required_reads
            .iter()
            .map(|read| AssignmentReadWireV2 {
                path: read.path.as_str().to_owned(),
                digest: read.digest.to_hex(),
                reason: read.reason.clone(),
            })
            .collect(),
        assignment_evidence: assignment_evidence
            .iter()
            .map(|evidence| AssignmentEvidenceWireV2 {
                role: evidence.role.wire_name().to_owned(),
                artifact_id: evidence.artifact_id.get(),
                digest: evidence.digest.to_hex(),
                byte_length: evidence.byte_length,
            })
            .collect(),
        tools: profile
            .tools
            .iter()
            .map(|tool| tool_name(*tool).to_owned())
            .collect(),
        terminal_operations: terminal_operations(target_kind)
            .iter()
            .map(|value| terminal_name(*value).to_owned())
            .collect(),
        remaining_campaign_allowance_micro_usd: remaining.get(),
        aggregate_revision: revision.get(),
        packet_digest: String::new(),
    })
}

#[allow(clippy::too_many_arguments)]
fn typed_packet(
    assignment_id: AssignmentId,
    campaign_id: CampaignId,
    application_revision_id: ApplicationRevisionId,
    kernel_build_id: factory_protocol::KernelBuildId,
    target_kind: DurableAssignmentTarget,
    target: String,
    system_prompt_artifact_id: factory_protocol::ArtifactId,
    assignment_prompt_artifact_id: factory_protocol::ArtifactId,
    required_read_manifest_artifact_id: factory_protocol::ArtifactId,
    workspace_root: AbsoluteHostPath,
    staging_root: AbsoluteHostPath,
    model: factory_protocol::ModelProfileV2,
    limits: factory_protocol::SessionLimitsV2,
    policy: factory_protocol::ActorPolicyArtifactV2,
    policy_bytes: Vec<u8>,
    runtime: factory_protocol::RuntimeIdentityV2,
    required_reads: Vec<ReadExactFileV2>,
    assignment_evidence: Vec<AssignmentEvidenceV2>,
    remaining_campaign_allowance: MicroUsd,
    revision: AggregateRevision,
    packet_digest: ContentDigest,
) -> AssignmentPacketV2 {
    AssignmentPacketV2 {
        format_version: factory_protocol::ASSIGNMENT_PACKET_V2_FORMAT,
        campaign_id,
        assignment_id,
        kernel_build_id,
        application_revision_id,
        assignment_role: office_for_target(target_kind),
        target,
        ticket_attempt_id: ticket_attempt(target_kind),
        candidate_id: candidate(target_kind),
        system_prompt_artifact_id,
        assignment_prompt_artifact_id,
        required_read_manifest_artifact_id,
        policy_digest: policy.digest,
        policy_byte_limit: policy.byte_limit,
        policy_bytes,
        policy_entrypoint: policy.entrypoint,
        workspace_root,
        staging_root,
        model,
        limits,
        runtime,
        required_reads,
        assignment_evidence,
        terminal_operations: terminal_operations(target_kind),
        remaining_campaign_allowance,
        revision,
        packet_digest,
    }
}

fn office_for_target(target: DurableAssignmentTarget) -> AssignmentRole {
    match target {
        DurableAssignmentTarget::Product => AssignmentRole::ProductResearch,
        DurableAssignmentTarget::Engineering { .. } => AssignmentRole::Engineering,
        DurableAssignmentTarget::Quality { .. } => AssignmentRole::Quality,
    }
}
fn ticket_attempt(target: DurableAssignmentTarget) -> Option<factory_protocol::TicketAttemptId> {
    match target {
        DurableAssignmentTarget::Product => None,
        DurableAssignmentTarget::Engineering { ticket_attempt_id }
        | DurableAssignmentTarget::Quality {
            ticket_attempt_id, ..
        } => Some(ticket_attempt_id),
    }
}
fn candidate(target: DurableAssignmentTarget) -> Option<factory_protocol::CandidateId> {
    match target {
        DurableAssignmentTarget::Quality { candidate_id, .. } => Some(candidate_id),
        _ => None,
    }
}
fn terminal_operations(target: DurableAssignmentTarget) -> Vec<TerminalOperationV2> {
    match target {
        DurableAssignmentTarget::Product => vec![TerminalOperationV2::WorkComplete],
        DurableAssignmentTarget::Engineering { .. } => vec![TerminalOperationV2::CandidateSubmit],
        DurableAssignmentTarget::Quality { .. } => vec![TerminalOperationV2::QualitySubmitReview],
    }
}
/// Flattens the resolver's exact named evidence closure into the same closed
/// packet list the actor will later use for `artifact.read`. No target prose
/// is an authority: every item is a durable sealed identity re-verified by
/// the resolver immediately before packet construction.
fn exact_assignment_evidence(
    target: DurableAssignmentTarget,
    context: &DurableAssignmentLaunchContext,
) -> Result<Vec<AssignmentEvidenceV2>, AssignmentRuntimeError> {
    let mut values = Vec::new();
    let mut push = |role: AssignmentEvidenceRoleV2, reference: SealedArtifactReferenceV2| {
        values.push(AssignmentEvidenceV2 {
            role,
            artifact_id: reference.artifact_id,
            digest: reference.digest,
            byte_length: reference.byte_length,
        });
    };
    match target {
        DurableAssignmentTarget::Product => {
            if context.evidence.proposal.is_some() || context.evidence.candidate.is_some() {
                return Err(AssignmentRuntimeError::Application(
                    "Product context unexpectedly carries external evidence".to_owned(),
                ));
            }
        }
        DurableAssignmentTarget::Engineering { .. } | DurableAssignmentTarget::Quality { .. } => {
            let proposal = context.evidence.proposal.as_ref().ok_or_else(|| {
                AssignmentRuntimeError::Application(
                    "Engineering or Quality target lacks ticket proposal evidence".to_owned(),
                )
            })?;
            push(
                AssignmentEvidenceRoleV2::TicketProposal,
                proposal.proposal.clone(),
            );
            push(
                AssignmentEvidenceRoleV2::TicketNarrative,
                proposal.narrative.clone(),
            );
            push(
                AssignmentEvidenceRoleV2::TicketEvidence,
                proposal.evidence.clone(),
            );
            push(
                AssignmentEvidenceRoleV2::ReproducerCommand,
                proposal.reproducer_command.clone(),
            );
            if let Some(stdin) = &proposal.reproducer_stdin {
                push(AssignmentEvidenceRoleV2::ReproducerStdin, stdin.clone());
            }
            push(
                AssignmentEvidenceRoleV2::ReproducerExpectedStdout,
                proposal.expected_observation.stdout.clone(),
            );
            push(
                AssignmentEvidenceRoleV2::ReproducerExpectedStderr,
                proposal.expected_observation.stderr.clone(),
            );
            push(
                AssignmentEvidenceRoleV2::ReproducerFirstActualStdout,
                proposal.first_observation.stdout.clone(),
            );
            push(
                AssignmentEvidenceRoleV2::ReproducerFirstActualStderr,
                proposal.first_observation.stderr.clone(),
            );
            push(
                AssignmentEvidenceRoleV2::ReproducerSecondActualStdout,
                proposal.second_observation.stdout.clone(),
            );
            push(
                AssignmentEvidenceRoleV2::ReproducerSecondActualStderr,
                proposal.second_observation.stderr.clone(),
            );
            if let DurableAssignmentTarget::Quality { .. } = target {
                let candidate = context.evidence.candidate.as_ref().ok_or_else(|| {
                    AssignmentRuntimeError::Application(
                        "Quality target lacks candidate evidence".to_owned(),
                    )
                })?;
                push(
                    AssignmentEvidenceRoleV2::ChangedPaths,
                    candidate.changed_paths.clone(),
                );
                push(
                    AssignmentEvidenceRoleV2::RegressionPatch,
                    candidate.regression_patch.clone(),
                );
                push(
                    AssignmentEvidenceRoleV2::RegressionCommandSet,
                    candidate.regression_command_set.clone(),
                );
                push(
                    AssignmentEvidenceRoleV2::RegressionLog,
                    candidate.regression_log.clone(),
                );
                push(
                    AssignmentEvidenceRoleV2::CandidatePatch,
                    candidate.candidate_patch.clone(),
                );
                push(
                    AssignmentEvidenceRoleV2::EngineeringReport,
                    candidate.engineering_report.clone(),
                );
                push(
                    AssignmentEvidenceRoleV2::EngineeringRisks,
                    candidate.engineering_risks.clone(),
                );
                push(
                    AssignmentEvidenceRoleV2::HardValidationCommandSet,
                    candidate.hard_validation_command_set.clone(),
                );
                push(
                    AssignmentEvidenceRoleV2::HardValidationLog,
                    candidate.hard_validation_log.clone(),
                );
                if let Some(probes) = &candidate.prior_quality_additional_probes {
                    push(
                        AssignmentEvidenceRoleV2::QualityAdditionalProbes,
                        probes.clone(),
                    );
                }
                if let Some(rationale) = &candidate.prior_quality_rationale {
                    push(
                        AssignmentEvidenceRoleV2::QualityRationale,
                        rationale.clone(),
                    );
                }
                if let Some(risks) = &candidate.prior_quality_risks {
                    push(AssignmentEvidenceRoleV2::QualityRisks, risks.clone());
                }
                if let Some(rationale) = &candidate.architect_rationale {
                    push(
                        AssignmentEvidenceRoleV2::ExternalDecisionRationale,
                        rationale.clone(),
                    );
                }
            } else if context.evidence.candidate.is_some() {
                return Err(AssignmentRuntimeError::Application(
                    "Engineering context unexpectedly carries candidate evidence".to_owned(),
                ));
            }
        }
    }
    values.sort_by_key(|evidence| evidence.role);
    if values.len() > 24 {
        return Err(AssignmentRuntimeError::Application(
            "assignment evidence exceeds the closed packet reference limit".to_owned(),
        ));
    }
    for pair in values.windows(2) {
        if pair[0].role == pair[1].role {
            return Err(AssignmentRuntimeError::Application(
                "assignment evidence closure repeats a closed role".to_owned(),
            ));
        }
    }
    Ok(values)
}

fn target_text(
    target_facts: &HarnessTargetFacts,
    evidence: &[AssignmentEvidenceV2],
    required_reads: &[ReadExactFileV2],
) -> Result<String, AssignmentRuntimeError> {
    let target = match target_facts {
        HarnessTargetFacts::Product => "product-research".to_owned(),
        HarnessTargetFacts::Engineering {
            ticket_attempt_id,
            ticket_id,
            ticket_revision_id,
            ..
        } => format!(
            "ticket-{}-revision-{}-attempt-{}",
            ticket_id.get(),
            ticket_revision_id.get(),
            ticket_attempt_id.get()
        ),
        HarnessTargetFacts::Quality {
            ticket_attempt_id,
            candidate_id,
            ticket_id,
            ticket_revision_id,
            validation_id,
        } => format!(
            "ticket-{}-revision-{}-attempt-{}-candidate-{}-validation-{}",
            ticket_id.get(),
            ticket_revision_id.get(),
            ticket_attempt_id.get(),
            candidate_id.get(),
            validation_id.get()
        ),
    };
    let mut rendered = target;
    append_target_evidence(&mut rendered, evidence)?;
    append_target_required_reads(&mut rendered, required_reads)?;
    Ok(rendered)
}

/// Appends only the neutral, closed evidence labels that actors may receive
/// in `${TARGET}`.  Durable internals can retain their authority-specific
/// terminology; none of it may leak through this model-visible rendering.
fn append_target_evidence(
    rendered: &mut String,
    evidence: &[AssignmentEvidenceV2],
) -> Result<(), AssignmentRuntimeError> {
    for reference in evidence {
        rendered.push('\n');
        rendered.push_str(reference.role.wire_name());
        rendered.push_str(" artifact_id=");
        rendered.push_str(&reference.artifact_id.get().to_string());
        rendered.push_str(" digest=");
        rendered.push_str(&reference.digest.to_hex());
        rendered.push_str(" byte_length=");
        rendered.push_str(&reference.byte_length.to_string());
    }
    if rendered.len() > 4_096 {
        Err(AssignmentRuntimeError::Application(
            "target plus closed assignment evidence exceeds the packet bound".to_owned(),
        ))
    } else {
        Ok(())
    }
}

/// Renders the exact, packet-bound workspace reads beside the evidence map so
/// an actor can satisfy the kernel's pre-mutation read gate without inferring
/// ticket contract paths from narrative prose or a failed tool invocation.
fn append_target_required_reads(
    rendered: &mut String,
    required_reads: &[ReadExactFileV2],
) -> Result<(), AssignmentRuntimeError> {
    if required_reads.is_empty() {
        return Ok(());
    }
    rendered.push_str("\n\nExact workspace reads required before any mutating tool:\n");
    for read in required_reads {
        rendered.push_str("- `");
        rendered.push_str(read.path.as_str());
        rendered.push_str("`: ");
        rendered.push_str(&read.reason);
        rendered.push('\n');
    }
    if rendered.len() > 4_096 {
        Err(AssignmentRuntimeError::Application(
            "target plus closed assignment evidence and required reads exceeds the packet bound"
                .to_owned(),
        ))
    } else {
        Ok(())
    }
}
fn repository_identity(context: &DurableAssignmentLaunchContext) -> String {
    ContentDigest::of_bytes(
        format!(
            "factory-repository-base-v1\0{}\0{}",
            context.materialize_commit, context.materialize_tree
        )
        .as_bytes(),
    )
    .to_hex()
}
fn absolute_path(
    path: &Path,
    field: &'static str,
) -> Result<AbsoluteHostPath, AssignmentRuntimeError> {
    AbsoluteHostPath::parse(
        path.to_str()
            .ok_or_else(|| AssignmentRuntimeError::Application(format!("{field} is not UTF-8")))?
            .to_owned(),
    )
    .map_err(Into::into)
}
fn office_name(assignment_role: AssignmentRole) -> &'static str {
    match assignment_role {
        AssignmentRole::ProductResearch => "product_research",
        AssignmentRole::Engineering => "engineering",
        AssignmentRole::Quality => "quality",
    }
}
fn thinking_name(value: factory_protocol::ThinkingLevelV2) -> &'static str {
    match value {
        factory_protocol::ThinkingLevelV2::None => "none",
        factory_protocol::ThinkingLevelV2::Low => "low",
        factory_protocol::ThinkingLevelV2::Medium => "medium",
        factory_protocol::ThinkingLevelV2::High => "high",
        factory_protocol::ThinkingLevelV2::XHigh => "xhigh",
    }
}
fn terminal_name(value: TerminalOperationV2) -> &'static str {
    match value {
        TerminalOperationV2::WorkComplete => "work_complete",
        TerminalOperationV2::CandidateSubmit => "candidate_submit",
        TerminalOperationV2::QualitySubmitReview => "quality_submit_review",
    }
}
fn tool_name(value: factory_protocol::ActorToolV2) -> &'static str {
    match value {
        factory_protocol::ActorToolV2::WorkspaceRead => "workspace_read",
        factory_protocol::ActorToolV2::WorkspaceWrite => "workspace_write",
        factory_protocol::ActorToolV2::WorkspaceEdit => "workspace_edit",
        factory_protocol::ActorToolV2::WorkspaceSearch => "workspace_search",
        factory_protocol::ActorToolV2::WorkspaceList => "workspace_list",
        factory_protocol::ActorToolV2::Shell => "shell",
        factory_protocol::ActorToolV2::ForumSearch => "forum_search",
        factory_protocol::ActorToolV2::ForumListTopics => "forum_list_topics",
        factory_protocol::ActorToolV2::ForumListThreads => "forum_list_threads",
        factory_protocol::ActorToolV2::ForumReadThread => "forum_read_thread",
        factory_protocol::ActorToolV2::PublicationCreate => "publication_create",
        factory_protocol::ActorToolV2::ArtifactSeal => "artifact_seal",
        factory_protocol::ActorToolV2::ArtifactRead => "artifact_read",
        factory_protocol::ActorToolV2::ProductSubmitTicket => "product_submit_ticket",
        factory_protocol::ActorToolV2::CandidateCheckpointRegression => {
            "candidate_checkpoint_regression"
        }
        factory_protocol::ActorToolV2::CandidateSubmit => "candidate_submit",
        factory_protocol::ActorToolV2::QualityRunFullSuite => "quality_run_full_suite",
        factory_protocol::ActorToolV2::QualitySubmitReview => "quality_submit_review",
        factory_protocol::ActorToolV2::WorkComplete => "work_complete",
    }
}

fn base64(bytes: &[u8]) -> String {
    const TABLE: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::with_capacity(bytes.len().div_ceil(3) * 4);
    for chunk in bytes.chunks(3) {
        let value = (u32::from(chunk[0]) << 16)
            | (u32::from(*chunk.get(1).unwrap_or(&0)) << 8)
            | u32::from(*chunk.get(2).unwrap_or(&0));
        out.push(TABLE[((value >> 18) & 63) as usize] as char);
        out.push(TABLE[((value >> 12) & 63) as usize] as char);
        out.push(if chunk.len() > 1 {
            TABLE[((value >> 6) & 63) as usize] as char
        } else {
            '='
        });
        out.push(if chunk.len() > 2 {
            TABLE[(value & 63) as usize] as char
        } else {
            '='
        });
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{env, fs};

    #[test]
    fn target_evidence_uses_neutral_external_decision_vocabulary() {
        let evidence = [AssignmentEvidenceV2 {
            role: AssignmentEvidenceRoleV2::ExternalDecisionRationale,
            artifact_id: factory_protocol::ArtifactId::new(7).unwrap(),
            digest: ContentDigest::of_bytes(b"external decision rationale"),
            byte_length: 27,
        }];
        let mut rendered = "ticket-1-revision-1-attempt-1".to_owned();
        append_target_evidence(&mut rendered, &evidence).unwrap();
        assert!(rendered.contains("external_decision_rationale"));
        assert!(
            !rendered.to_ascii_lowercase().contains("architect"),
            "worker-visible TARGET leaked authority vocabulary: {rendered}"
        );
    }

    #[test]
    fn target_names_each_packet_bound_workspace_read_before_mutation() {
        let reads = [
            ReadExactFileV2 {
                path: factory_protocol::RepositoryRelativePath::parse("docs/contract.md").unwrap(),
                digest: ContentDigest::of_bytes(b"contract"),
                reason: "defines the assigned runtime behavior".to_owned(),
            },
            ReadExactFileV2 {
                path: factory_protocol::RepositoryRelativePath::parse("src/runtime.rs").unwrap(),
                digest: ContentDigest::of_bytes(b"runtime"),
                reason: "owns the assigned implementation boundary".to_owned(),
            },
        ];
        let mut rendered = "ticket-1-revision-1-attempt-1".to_owned();

        append_target_required_reads(&mut rendered, &reads).unwrap();

        assert!(rendered.contains("Exact workspace reads required before any mutating tool"));
        assert!(rendered.contains("`docs/contract.md`: defines the assigned runtime behavior"));
        assert!(rendered.contains("`src/runtime.rs`: owns the assigned implementation boundary"));
    }

    #[test]
    fn exact_required_reads_reuses_an_application_read_for_the_same_ticket_contract_path() {
        let workspace = env::temp_dir().join(format!(
            "factory-assignment-required-reads-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos(),
        ));
        fs::create_dir_all(workspace.join("docs")).unwrap();
        fs::write(workspace.join("docs/contract.md"), b"the exact contract").unwrap();
        let workspace = fs::canonicalize(workspace).unwrap();
        let path = factory_protocol::RepositoryRelativePath::parse("docs/contract.md").unwrap();

        let reads = exact_required_reads(
            &[RequiredReadV2 {
                path: path.clone(),
                reason: "application orientation".to_owned(),
            }],
            &[TicketContractReadV2 {
                path,
                reason: "ticket acceptance contract".to_owned(),
            }],
            &workspace,
        )
        .unwrap();

        assert_eq!(reads.len(), 1);
        assert_eq!(reads[0].path.as_str(), "docs/contract.md");
        assert_eq!(reads[0].reason, "application orientation");
        fs::remove_dir_all(workspace).unwrap();
    }

    #[test]
    fn assignment_staging_cleanup_removes_only_the_exact_owned_assignment_root() {
        let runtime = env::temp_dir().join(format!(
            "factory-assignment-staging-cleanup-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos(),
        ));
        fs::create_dir_all(&runtime).unwrap();
        let runtime = fs::canonicalize(runtime).unwrap();
        let assignment = AssignmentId::new(7).unwrap();
        let staging = create_assignment_staging(&runtime, assignment).unwrap();
        fs::write(staging.join("unsealed-stream"), b"temporary bytes").unwrap();
        let neighboring = runtime.join("staging").join("assignment-8");
        fs::create_dir(&neighboring).unwrap();
        fs::write(neighboring.join("keep"), b"another assignment").unwrap();

        remove_assignment_staging(&runtime, assignment, &staging).unwrap();

        assert!(
            !staging.exists(),
            "the terminal assignment staging root must be discarded"
        );
        assert_eq!(
            fs::read(neighboring.join("keep")).unwrap(),
            b"another assignment",
            "cleanup must not touch a sibling assignment root"
        );
        fs::remove_dir_all(runtime).unwrap();
    }

    #[test]
    fn assignment_packet_preserves_the_campaign_kernel_identity() {
        let campaign_build =
            factory_protocol::KernelBuildId::new(ContentDigest::of_bytes(b"campaign"));
        let installed_build =
            factory_protocol::KernelBuildId::new(ContentDigest::of_bytes(b"installed"));
        let campaign = CampaignMaterial {
            revision: AggregateRevision::initial(),
            remaining: MicroUsd::new(1),
            kernel_build_id: campaign_build,
        };

        assert_eq!(assignment_packet_kernel_build(&campaign), campaign_build);
        assert_ne!(assignment_packet_kernel_build(&campaign), installed_build);
    }

    #[test]
    fn harness_spec_artifact_spelling_is_stable_and_keeps_context_typed() {
        let spec = HarnessSpecV2 {
            compiler_version: HARNESS_COMPILER_VERSION_V2,
            application_revision_id: ApplicationRevisionId::new(1).unwrap(),
            office_id: OfficeId::new(2).unwrap(),
            assignment_role: AssignmentRole::Engineering,
            objective: "ticket-3-revision-4-attempt-5".to_owned(),
            context_items: vec![
                ContextItemV2 {
                    reference: ContextReferenceV2::Office(OfficeId::new(2).unwrap()),
                    inclusion: ContextInclusionClassV2::DirectTarget,
                    reason: "the admitted office owns this invocation".to_owned(),
                },
                ContextItemV2 {
                    reference: ContextReferenceV2::Ticket(
                        factory_protocol::TicketId::new(3).unwrap(),
                    ),
                    inclusion: ContextInclusionClassV2::DirectTarget,
                    reason: "the selected ticket is the direct assignment target".to_owned(),
                },
            ],
            capabilities: vec![
                factory_protocol::ActorToolV2::WorkspaceRead,
                factory_protocol::ActorToolV2::ArtifactRead,
            ],
            remaining_campaign_allowance: MicroUsd::new(42),
        };
        assert_eq!(
            canonical_harness_spec_json(&spec),
            canonical_harness_spec_json(&spec)
        );
        let rendered = canonical_harness_spec_json(&spec);
        assert!(rendered.contains("\"reference_kind\":\"office\""));
        assert!(rendered.contains("\"reference_kind\":\"ticket\""));
        assert!(!rendered.contains("context_text"));
    }
}
