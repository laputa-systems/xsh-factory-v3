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
    AbsoluteHostPath, AggregateRevision, ApplicationRevisionId, AssignmentCredentialWireV1,
    AssignmentId, AssignmentLimitsWireV1, AssignmentModelWireV1, AssignmentPacketV1,
    AssignmentPacketWireV1, AssignmentReadWireV1, AssignmentRuntimeWireV1, CampaignId,
    ContentDigest, ExpectedRevision, MicroUsd, Office, ReadExactFileV1, TerminalOperationV1,
    canonical_assignment_packet_json_v1, parse_application_bundle_v1, render_template_v1,
    unsigned_assignment_packet_digest_v1,
};
use sqlx::Row;
use thiserror::Error;

use crate::{
    cas::CasStore,
    durable_authority::{
        DurableAssignmentLaunchContext, DurableAssignmentLaunchRequest, DurableAssignmentTarget,
        DurableAuthorityResolver,
    },
    git::{GitCustody, GitCustodyError, OwnedWorktree, WorktreeKind, WorktreeName},
    installed_runtime::{
        InstalledKernelBuildReceiptV1, InstalledKernelExecutionTools, InstalledRuntimeError,
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

/// Durable assignment/session result plus the exact disposable workspace
/// owner. The daemon decides retention/cleanup after the terminal outcome;
/// this direct seam never performs broad worktree pruning.
pub struct AssignmentLaunchOutcome {
    pub assignment_id: AssignmentId,
    pub assignment_revision: AggregateRevision,
    pub session: SessionRuntimeOutcome,
    pub workspace: OwnedWorktree,
    pub staging_root: PathBuf,
}

/// Provider-free composition failures. A staging/worktree may remain for
/// forensic inspection when a later immutable seal or transition rejects;
/// neither one is ever silently reused for a different assignment identity.
#[derive(Debug, Error)]
pub enum AssignmentRuntimeError {
    #[error(transparent)]
    Store(#[from] StoreError),

    #[error(transparent)]
    InstalledRuntime(#[from] InstalledRuntimeError),

    #[error(transparent)]
    Git(#[from] GitCustodyError),

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
}

/// Materializes and launches exactly one durable assignment. The resolver is
/// invoked once before packet construction and again through the live session
/// runtime for Engineering/Quality, so stale target state fails closed on both
/// sides of the persistence boundary.
pub async fn materialize_and_launch_assignment(
    store: &KernelStore,
    cas: &CasStore,
    daemon: &LocalDaemon,
    installed: &InstalledKernelBuildReceiptV1,
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

    let workspace =
        materialize_workspace(execution.git_custody().as_ref(), &context, assignment_id)?;
    let staging_root = create_assignment_staging(cas.runtime_root(), assignment_id)?;
    let application = load_application_material(
        store,
        &process,
        cas,
        request.application_revision_id,
        office_for_target(request.target),
    )
    .await?;

    let required_reads = exact_required_reads(&context, workspace.path())?;
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

    let values = prompt_values(
        assignment_id,
        request.campaign_id,
        request.application_revision_id,
        request.target,
        &context,
        &application.mission,
    )?;
    let system_prompt = render_declared_template(
        &application.system_template,
        &application.system_source,
        &values,
    )?;
    let assignment_prompt = render_declared_template(
        &application.assignment_template,
        &application.assignment_source,
        &values,
    )?;
    let system = register_kernel_bytes(
        &process,
        cas,
        installed.kernel_build_id(),
        assignment_id,
        "system-prompt",
        &system_prompt,
    )
    .await?;
    let assignment_prompt_artifact = register_kernel_bytes(
        &process,
        cas,
        installed.kernel_build_id(),
        assignment_id,
        "assignment-prompt",
        &assignment_prompt,
    )
    .await?;

    let campaign = current_campaign_material(
        store,
        request.campaign_id,
        request.application_revision_id,
        request.expected_campaign_revision,
    )
    .await?;
    let runtime = installed.runtime_identity_for_provider(&application.profile.model.provider)?;
    let workspace_root = absolute_path(workspace.path(), "workspace root")?;
    let staging_absolute = absolute_path(&staging_root, "staging root")?;
    let target = target_text(request.target, &context)?;
    let mut wire = assignment_wire(
        assignment_id,
        request.campaign_id,
        request.application_revision_id,
        installed.kernel_build_id(),
        request.target,
        target.clone(),
        &context,
        system.artifact_id,
        assignment_prompt_artifact.artifact_id,
        required_manifest.artifact_id,
        &system_prompt,
        &assignment_prompt,
        workspace_root.as_str(),
        staging_absolute.as_str(),
        &application.profile,
        &runtime,
        &required_reads,
        campaign.remaining,
        campaign.revision,
    )?;
    let packet_digest = unsigned_assignment_packet_digest_v1(&wire)?;
    wire.packet_digest = packet_digest.to_hex();
    let packet_bytes = canonical_assignment_packet_json_v1(&wire)?.into_bytes();
    let packet = typed_packet(
        assignment_id,
        request.campaign_id,
        request.application_revision_id,
        installed.kernel_build_id(),
        request.target,
        target,
        system.artifact_id,
        assignment_prompt_artifact.artifact_id,
        required_manifest.artifact_id,
        workspace_root,
        staging_absolute,
        application.profile.model.clone(),
        application.profile.limits.clone(),
        runtime,
        required_reads.clone(),
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
        workspace.path().to_owned(),
        0,
        (
            OsString::from(installed.openrouter_credential_environment()),
            request.credential_environment_value,
        ),
    )?;
    let supervision = ProcessSupervisionSpec::new(
        staging_root.join(SESSION_STDOUT_RELATIVE_PATH),
        staging_root.join(SESSION_STDERR_RELATIVE_PATH),
        u64::from(application.profile.limits.output_byte_limit),
        u64::from(application.profile.limits.output_byte_limit),
        Duration::from_millis(application.profile.limits.wall_limit.get()),
        TERMINATION_GRACE,
    )?;
    let candidate_quality_runtime = match request.target {
        DurableAssignmentTarget::Product => None,
        DurableAssignmentTarget::Engineering { .. } | DurableAssignmentTarget::Quality { .. } => {
            Some(CandidateQualitySessionRuntime::new(
                store.decision_store(),
                execution.git_custody(),
                resolver,
            ))
        }
    };
    let session = launch_session(
        &process,
        &store.forum_store(),
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
            workspace_root: workspace.path().to_owned(),
            expected_read_manifest_artifact_id: required_manifest.artifact_id,
            required_reads,
            candidate_quality_runtime,
        },
        installed.runtime(),
    )
    .await?;
    Ok(AssignmentLaunchOutcome {
        assignment_id,
        assignment_revision: assignment_receipt.resulting_revision,
        session,
        workspace,
        staging_root,
    })
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
            .rematerialize_tree(
                &context.repository,
                context.materialize_tree.clone(),
                WorktreeKind::Review,
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
    let staging = parent.join(format!("assignment-{}", assignment_id.get()));
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

struct ApplicationMaterial {
    mission: String,
    system_template: factory_protocol::TemplateArtifactV1,
    system_source: String,
    assignment_template: factory_protocol::TemplateArtifactV1,
    assignment_source: String,
    profile: factory_protocol::OfficeProfileV1,
}

async fn load_application_material(
    store: &KernelStore,
    process: &ProcessStore,
    cas: &CasStore,
    application_revision_id: ApplicationRevisionId,
    office: Office,
) -> Result<ApplicationMaterial, AssignmentRuntimeError> {
    let row = sqlx::query(
        "SELECT bundle_artifact_id, mission_artifact_id,
                product_research_system_template_artifact_id,
                product_research_assignment_template_artifact_id,
                engineering_system_template_artifact_id,
                engineering_assignment_template_artifact_id,
                quality_system_template_artifact_id,
                quality_assignment_template_artifact_id
           FROM factory.application_revisions WHERE id = $1",
    )
    .bind(application_revision_id.get())
    .fetch_optional(&store.pool_for_authority())
    .await?
    .ok_or(StoreError::UnknownApplicationRevision {
        application_revision_id,
    })?;
    let artifact = |name: &str| -> Result<factory_protocol::ArtifactId, AssignmentRuntimeError> {
        let value: i64 = row.try_get(name).map_err(|error| {
            AssignmentRuntimeError::Application(format!("durable {name} is corrupt: {error}"))
        })?;
        factory_protocol::ArtifactId::new(value).map_err(Into::into)
    };
    let bundle = registered_bytes(process, cas, artifact("bundle_artifact_id")?).await?;
    let bundle = parse_application_bundle_v1(&bundle).map_err(|error| {
        AssignmentRuntimeError::Application(format!("admitted bundle is invalid: {error}"))
    })?;
    let profile = bundle
        .office_profiles
        .iter()
        .find(|profile| profile.office == office)
        .cloned()
        .ok_or_else(|| {
            AssignmentRuntimeError::Application("admitted bundle lacks selected office".to_owned())
        })?;
    let (system_id, assignment_id) = match office {
        Office::ProductResearch => (
            artifact("product_research_system_template_artifact_id")?,
            artifact("product_research_assignment_template_artifact_id")?,
        ),
        Office::Engineering => (
            artifact("engineering_system_template_artifact_id")?,
            artifact("engineering_assignment_template_artifact_id")?,
        ),
        Office::Quality => (
            artifact("quality_system_template_artifact_id")?,
            artifact("quality_assignment_template_artifact_id")?,
        ),
    };
    let mission = checked_template_bytes(
        registered_bytes(process, cas, artifact("mission_artifact_id")?).await?,
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
    template: &factory_protocol::TemplateArtifactV1,
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
    context: &DurableAssignmentLaunchContext,
    workspace: &Path,
) -> Result<Vec<ReadExactFileV1>, AssignmentRuntimeError> {
    let mut values = Vec::new();
    for read in &context.application_required_reads {
        values.push((read.path.clone(), read.reason.clone()));
    }
    for read in &context.ticket_contract_reads {
        values.push((read.path.clone(), read.reason.clone()));
    }
    let mut paths = BTreeSet::new();
    let mut result = Vec::with_capacity(values.len());
    for (path, reason) in values {
        if !paths.insert(path.clone()) {
            return Err(AssignmentRuntimeError::Application(
                "application and ticket required reads overlap".to_owned(),
            ));
        }
        result.push(ReadExactFileV1 {
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

fn prompt_values(
    assignment_id: AssignmentId,
    campaign_id: CampaignId,
    application_revision_id: ApplicationRevisionId,
    target: DurableAssignmentTarget,
    context: &DurableAssignmentLaunchContext,
    mission: &str,
) -> Result<BTreeMap<String, String>, AssignmentRuntimeError> {
    let mut values = BTreeMap::from([
        ("ASSIGNMENT_ID".to_owned(), assignment_id.get().to_string()),
        (
            "APPLICATION_REVISION_ID".to_owned(),
            application_revision_id.get().to_string(),
        ),
        ("CAMPAIGN_ID".to_owned(), campaign_id.get().to_string()),
        ("MISSION".to_owned(), mission.to_owned()),
        (
            "OFFICE".to_owned(),
            office_name(office_for_target(target)).to_owned(),
        ),
        ("TARGET".to_owned(), target_text(target, context)?),
    ]);
    match target {
        DurableAssignmentTarget::Product => {}
        DurableAssignmentTarget::Engineering { .. } => {
            values.insert(
                "TICKET_ID".to_owned(),
                context
                    .ticket_id
                    .ok_or_else(|| {
                        AssignmentRuntimeError::Application(
                            "Engineering target lacks ticket ID".to_owned(),
                        )
                    })?
                    .get()
                    .to_string(),
            );
            values.insert(
                "TICKET_REVISION_ID".to_owned(),
                context
                    .ticket_revision_id
                    .ok_or_else(|| {
                        AssignmentRuntimeError::Application(
                            "Engineering target lacks ticket revision ID".to_owned(),
                        )
                    })?
                    .get()
                    .to_string(),
            );
        }
        DurableAssignmentTarget::Quality { candidate_id, .. } => {
            values.insert(
                "TICKET_ID".to_owned(),
                context
                    .ticket_id
                    .ok_or_else(|| {
                        AssignmentRuntimeError::Application(
                            "Quality target lacks ticket ID".to_owned(),
                        )
                    })?
                    .get()
                    .to_string(),
            );
            values.insert(
                "TICKET_REVISION_ID".to_owned(),
                context
                    .ticket_revision_id
                    .ok_or_else(|| {
                        AssignmentRuntimeError::Application(
                            "Quality target lacks ticket revision ID".to_owned(),
                        )
                    })?
                    .get()
                    .to_string(),
            );
            values.insert("CANDIDATE_ID".to_owned(), candidate_id.get().to_string());
            values.insert(
                "VALIDATION_ID".to_owned(),
                context
                    .validation_id
                    .ok_or_else(|| {
                        AssignmentRuntimeError::Application(
                            "Quality target lacks hard validation ID".to_owned(),
                        )
                    })?
                    .get()
                    .to_string(),
            );
        }
    }
    Ok(values)
}

fn render_declared_template(
    template: &factory_protocol::TemplateArtifactV1,
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
    Ok(render_template_v1(template, source, &values)?)
}

struct CampaignMaterial {
    revision: AggregateRevision,
    remaining: MicroUsd,
}

async fn current_campaign_material(
    store: &KernelStore,
    campaign_id: CampaignId,
    application_revision_id: ApplicationRevisionId,
    expected: ExpectedRevision,
) -> Result<CampaignMaterial, AssignmentRuntimeError> {
    let row = sqlx::query(
        "SELECT revision, aggregate_budget_micro_usd, measured_cost_micro_usd,
                application_revision_id, lifecycle, cost_state
           FROM factory.campaigns WHERE id = $1",
    )
    .bind(campaign_id.get())
    .fetch_optional(&store.pool_for_authority())
    .await?
    .ok_or(StoreError::UnknownCampaign { campaign_id })?;
    let revision = u64::try_from(row.try_get::<i64, _>("revision").map_err(|error| {
        AssignmentRuntimeError::Application(format!("campaign revision is corrupt: {error}"))
    })?)
    .map_err(|_| AssignmentRuntimeError::Application("campaign revision is negative".to_owned()))?;
    let revision = AggregateRevision::from_persisted(revision);
    if expected.get() != revision
        || row.try_get::<i64, _>("application_revision_id")? != application_revision_id.get()
        || row.try_get::<i16, _>("lifecycle")? != 0
        || row.try_get::<i16, _>("cost_state")? != 0
    {
        return Err(AssignmentRuntimeError::Application(
            "campaign changed before assignment packet construction".to_owned(),
        ));
    }
    let budget =
        u64::try_from(row.try_get::<i64, _>("aggregate_budget_micro_usd")?).map_err(|_| {
            AssignmentRuntimeError::Application("campaign budget is corrupt".to_owned())
        })?;
    let measured = u64::try_from(row.try_get::<i64, _>("measured_cost_micro_usd")?)
        .map_err(|_| AssignmentRuntimeError::Application("campaign cost is corrupt".to_owned()))?;
    let remaining = budget.checked_sub(measured).ok_or_else(|| {
        AssignmentRuntimeError::Application("campaign measured cost exceeds budget".to_owned())
    })?;
    if remaining == 0 {
        return Err(AssignmentRuntimeError::Application(
            "campaign has no remaining assignment allowance".to_owned(),
        ));
    }
    Ok(CampaignMaterial {
        revision,
        remaining: MicroUsd::new(remaining),
    })
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
    profile: &factory_protocol::OfficeProfileV1,
    runtime: &factory_protocol::RuntimeIdentityV1,
    required_reads: &[ReadExactFileV1],
    remaining: MicroUsd,
    revision: AggregateRevision,
) -> Result<AssignmentPacketWireV1, AssignmentRuntimeError> {
    let credential_source = match &runtime.credential {
        factory_protocol::CredentialDescriptorV1::Environment { name } => {
            AssignmentCredentialWireV1 {
                kind: "environment".to_owned(),
                name: Some(name.clone()),
                path: None,
            }
        }
        factory_protocol::CredentialDescriptorV1::PiAuthStore { .. } => {
            return Err(AssignmentRuntimeError::Application(
                "MVP materializer does not admit a Pi auth store".to_owned(),
            ));
        }
    };
    Ok(AssignmentPacketWireV1 {
        format_version: factory_protocol::ASSIGNMENT_PACKET_V1_FORMAT,
        campaign_id: campaign_id.get(),
        assignment_id: assignment_id.get(),
        application_revision_id: application_revision_id.get(),
        kernel_build_id: build_id.digest().to_hex(),
        office: office_name(office_for_target(target_kind)).to_owned(),
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
        workspace_root: workspace_root.to_owned(),
        staging_root: staging_root.to_owned(),
        model: AssignmentModelWireV1 {
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
                    factory_protocol::ModelCapabilityV1::Reasoning => "reasoning".to_owned(),
                })
                .collect(),
        },
        limits: AssignmentLimitsWireV1 {
            turn_limit: profile.limits.turn_limit,
            wall_limit_millis: profile.limits.wall_limit.get(),
            output_byte_limit: profile.limits.output_byte_limit,
        },
        runtime: AssignmentRuntimeWireV1 {
            deno_executable: runtime.deno_executable.as_str().to_owned(),
            deno_version: runtime.deno_version.clone(),
            source_graph_digest: runtime.source_graph_digest.to_hex(),
            resolved_dependency_graph_digest: runtime.resolved_dependency_graph_digest.to_hex(),
            deno_json_digest: runtime.deno_json_digest.to_hex(),
            deno_lock_digest: runtime.deno_lock_digest.to_hex(),
            pi_version: runtime.pi_version.clone(),
            credential_source,
        },
        required_reads: required_reads
            .iter()
            .map(|read| AssignmentReadWireV1 {
                path: read.path.as_str().to_owned(),
                digest: read.digest.to_hex(),
                reason: read.reason.clone(),
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
    model: factory_protocol::ModelProfileV1,
    limits: factory_protocol::SessionLimitsV1,
    runtime: factory_protocol::RuntimeIdentityV1,
    required_reads: Vec<ReadExactFileV1>,
    remaining_campaign_allowance: MicroUsd,
    revision: AggregateRevision,
    packet_digest: ContentDigest,
) -> AssignmentPacketV1 {
    AssignmentPacketV1 {
        format_version: factory_protocol::ASSIGNMENT_PACKET_V1_FORMAT,
        campaign_id,
        assignment_id,
        kernel_build_id,
        application_revision_id,
        office: office_for_target(target_kind),
        target,
        ticket_attempt_id: ticket_attempt(target_kind),
        candidate_id: candidate(target_kind),
        system_prompt_artifact_id,
        assignment_prompt_artifact_id,
        required_read_manifest_artifact_id,
        workspace_root,
        staging_root,
        model,
        limits,
        runtime,
        required_reads,
        terminal_operations: terminal_operations(target_kind),
        remaining_campaign_allowance,
        revision,
        packet_digest,
    }
}

fn office_for_target(target: DurableAssignmentTarget) -> Office {
    match target {
        DurableAssignmentTarget::Product => Office::ProductResearch,
        DurableAssignmentTarget::Engineering { .. } => Office::Engineering,
        DurableAssignmentTarget::Quality { .. } => Office::Quality,
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
fn terminal_operations(target: DurableAssignmentTarget) -> Vec<TerminalOperationV1> {
    match target {
        DurableAssignmentTarget::Product => vec![TerminalOperationV1::WorkComplete],
        DurableAssignmentTarget::Engineering { .. } => vec![TerminalOperationV1::CandidateSubmit],
        DurableAssignmentTarget::Quality { .. } => vec![TerminalOperationV1::QualitySubmitReview],
    }
}
fn target_text(
    target: DurableAssignmentTarget,
    context: &DurableAssignmentLaunchContext,
) -> Result<String, AssignmentRuntimeError> {
    match target {
        DurableAssignmentTarget::Product => Ok("product-research".to_owned()),
        DurableAssignmentTarget::Engineering { ticket_attempt_id } => Ok(format!(
            "ticket-{}-revision-{}-attempt-{}",
            context
                .ticket_id
                .ok_or_else(|| AssignmentRuntimeError::Application(
                    "Engineering target lacks ticket ID".to_owned()
                ))?
                .get(),
            context
                .ticket_revision_id
                .ok_or_else(|| AssignmentRuntimeError::Application(
                    "Engineering target lacks ticket revision ID".to_owned()
                ))?
                .get(),
            ticket_attempt_id.get()
        )),
        DurableAssignmentTarget::Quality {
            ticket_attempt_id,
            candidate_id,
        } => Ok(format!(
            "ticket-{}-revision-{}-attempt-{}-candidate-{}-validation-{}",
            context
                .ticket_id
                .ok_or_else(|| AssignmentRuntimeError::Application(
                    "Quality target lacks ticket ID".to_owned()
                ))?
                .get(),
            context
                .ticket_revision_id
                .ok_or_else(|| AssignmentRuntimeError::Application(
                    "Quality target lacks ticket revision ID".to_owned()
                ))?
                .get(),
            ticket_attempt_id.get(),
            candidate_id.get(),
            context
                .validation_id
                .ok_or_else(|| AssignmentRuntimeError::Application(
                    "Quality target lacks hard validation ID".to_owned()
                ))?
                .get()
        )),
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
fn office_name(office: Office) -> &'static str {
    match office {
        Office::ProductResearch => "product_research",
        Office::Engineering => "engineering",
        Office::Quality => "quality",
    }
}
fn thinking_name(value: factory_protocol::ThinkingLevelV1) -> &'static str {
    match value {
        factory_protocol::ThinkingLevelV1::None => "none",
        factory_protocol::ThinkingLevelV1::Low => "low",
        factory_protocol::ThinkingLevelV1::Medium => "medium",
        factory_protocol::ThinkingLevelV1::High => "high",
        factory_protocol::ThinkingLevelV1::XHigh => "xhigh",
    }
}
fn terminal_name(value: TerminalOperationV1) -> &'static str {
    match value {
        TerminalOperationV1::WorkComplete => "work_complete",
        TerminalOperationV1::CandidateSubmit => "candidate_submit",
        TerminalOperationV1::QualitySubmitReview => "quality_submit_review",
    }
}
fn tool_name(value: factory_protocol::ActorToolV1) -> &'static str {
    match value {
        factory_protocol::ActorToolV1::WorkspaceRead => "workspace_read",
        factory_protocol::ActorToolV1::WorkspaceWrite => "workspace_write",
        factory_protocol::ActorToolV1::WorkspaceEdit => "workspace_edit",
        factory_protocol::ActorToolV1::WorkspaceSearch => "workspace_search",
        factory_protocol::ActorToolV1::WorkspaceList => "workspace_list",
        factory_protocol::ActorToolV1::Shell => "shell",
        factory_protocol::ActorToolV1::ForumSearch => "forum_search",
        factory_protocol::ActorToolV1::ForumListTopics => "forum_list_topics",
        factory_protocol::ActorToolV1::ForumListThreads => "forum_list_threads",
        factory_protocol::ActorToolV1::ForumReadThread => "forum_read_thread",
        factory_protocol::ActorToolV1::ForumCreateTopic => "forum_create_topic",
        factory_protocol::ActorToolV1::ForumCreateThread => "forum_create_thread",
        factory_protocol::ActorToolV1::ForumPost => "forum_post",
        factory_protocol::ActorToolV1::ArtifactSeal => "artifact_seal",
        factory_protocol::ActorToolV1::ArtifactRead => "artifact_read",
        factory_protocol::ActorToolV1::ProductSubmitTicket => "product_submit_ticket",
        factory_protocol::ActorToolV1::CandidateCheckpointRegression => {
            "candidate_checkpoint_regression"
        }
        factory_protocol::ActorToolV1::CandidateSubmit => "candidate_submit",
        factory_protocol::ActorToolV1::QualityRunFullSuite => "quality_run_full_suite",
        factory_protocol::ActorToolV1::QualitySubmitReview => "quality_submit_review",
        factory_protocol::ActorToolV1::WorkComplete => "work_complete",
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
