//! Transactional assignment/session authority for Tranche 5.
//!
//! The host supplies only an immutable packet seal, process custody evidence,
//! and sealed terminal artifacts. PostgreSQL rechecks the packet/campaign
//! identities and performs one semantic transition plus one audit receipt.

use std::str::FromStr;

use factory_protocol::{
    AbsoluteHostPath, AggregateRevision, ApplicationRevisionId, ArtifactId,
    AssignmentEvidenceRoleV2, AssignmentEvidenceV2, AssignmentId, AssignmentPacketV2,
    AssignmentRole, CampaignId, CandidateId, ContentDigest, DurationMillis, ExpectedRevision,
    KernelBuildId, MicroUsd, ModelCapabilityV2, ModelProfileV2, PolicyEntrypointV2,
    ProcessCustodyV2, ReadExactFileV2, RepositoryId, RepositoryRelativePath, RuntimeIdentityV2,
    SessionId, SessionLimitsV2, SessionState, StopReasonV2, TerminalCostV2, TerminalOperationV2,
    TerminalReportV2, ThinkingLevelV2, TicketAttemptId, UsageTotalsV2,
};
use factory_settings::CAMPAIGN_SESSION_COST_AGGREGATE_MAXIMUM;
use sqlx::{PgPool, Postgres, Row};

use crate::cas::{CasArtifact, CasStore};
use crate::harness_store::{RecordHarnessCompilation, persist_harness_compilation};
use crate::storage::{self, KernelStore, StoreError};
use crate::workspace_read::SealedRequiredReadAssertion;

const CAMPAIGN_SUBJECT: i16 = 4;
const ASSIGNMENT_SUBJECT: i16 = 5;
const SESSION_SUBJECT: i16 = 6;
const CAMPAIGN_START: &str = "campaign.start";
const CAMPAIGN_CANCEL: &str = "campaign.cancel";
const CAMPAIGN_FAIL: &str = "campaign.fail";
const ASSIGNMENT_CREATE: &str = "assignment.create";
const SESSION_START: &str = "session.start";
const SESSION_TERMINAL: &str = "session.terminal";
const RUNNING: i16 = 0;
const FAILED: i16 = 2;
const PREPARED: i16 = 0;
const SESSION_RUNNING: i16 = 1;
const COST_KNOWN: i16 = 0;
const COST_UNKNOWN: i16 = 1;
const COST_EXCEEDED: i16 = 2;
const UNKNOWN_TERMINAL_COST_FAILURE_REASON: &str = "terminal session cost is unknown";
const EXCEEDED_TERMINAL_COST_FAILURE_REASON: &str =
    "terminal session exceeded the campaign cost limit";

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct StartCampaign {
    pub principal: String,
    pub command_id: String,
    /// Guards the immutable application revision selected for this campaign.
    /// The kernel, not the caller, resolves the current installed build and
    /// the repository bound by that application revision in the same
    /// transaction that creates the campaign.
    pub expected_application_revision: ExpectedRevision,
    pub application_revision_id: ApplicationRevisionId,
    pub aggregate_budget: MicroUsd,
    pub deadline_unix_millis: u64,
    pub delivery_target: u32,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CampaignReceipt {
    pub campaign_id: CampaignId,
    pub resulting_revision: AggregateRevision,
    pub kernel_build_id: KernelBuildId,
    pub application_revision_id: ApplicationRevisionId,
    pub repository_id: RepositoryId,
    pub audit_log_id: i64,
    pub was_idempotent_retry: bool,
}

/// Operator-authorized campaign cancellation. Cancellation is a durable
/// aggregate transition, not a test cleanup shortcut: it requires the
/// current campaign revision and records one idempotent audit receipt. The
/// operator RPC may first use that exact revision to close admission for an
/// active session, then complete this command after ordinary session
/// reconciliation advances the campaign by exactly one revision.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CancelCampaign {
    pub principal: String,
    pub command_id: String,
    pub expected_revision: ExpectedRevision,
    pub campaign_id: CampaignId,
}

/// The only nonterminal outcome of cancellation admission. The session ID is
/// selected from the locked campaign state, never from operator input, and is
/// used solely to find the daemon-owned cancellation handle for that process.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum CampaignCancellationAdmission {
    Completed(CampaignReceipt),
    ActiveSession { session_id: SessionId },
}

/// Proof passed back from the live session runtime after it has directly
/// waited the exact owned process and committed its terminal evidence. This is
/// crate-private because only the daemon's active-session coordinator may
/// bridge an admitted operator cancellation across that reconciliation step.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct ReconciledSessionCancellation {
    pub campaign_id: CampaignId,
    pub session_id: SessionId,
    pub campaign_revision: AggregateRevision,
}

/// Kernel-owned infrastructure/process failure transition. This is not an
/// Architect cancellation: a failed Product assignment or a materialization
/// failure cannot remain running and quietly consume another paid launch.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FailCampaign {
    pub principal: String,
    pub command_id: String,
    pub expected_revision: ExpectedRevision,
    pub campaign_id: CampaignId,
    pub reason: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CreateAssignment {
    pub principal: String,
    pub command_id: String,
    pub expected_campaign_revision: ExpectedRevision,
    pub identity: AssignmentIdentityCapability,
    pub packet: AssignmentPacketV2,
    pub packet_bytes: Vec<u8>,
    pub packet_artifact: CasArtifact,
    pub required_read_manifest_artifact_id: ArtifactId,
    pub attempt_ordinal: u32,
    /// The closed compiler receipt sealed with this assignment. This is
    /// kernel-derived material, never caller-provided actor intent.
    pub harness: Option<RecordHarnessCompilation>,
}

/// A sequence value reserved by the kernel before an assignment packet is
/// rendered. The ID is intentionally private: callers can only obtain it
/// from this capability and cannot fabricate an arbitrary assignment ID.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct AssignmentIdentityCapability {
    id: AssignmentId,
}

impl AssignmentIdentityCapability {
    #[must_use]
    pub const fn assignment_id(self) -> AssignmentId {
        self.id
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AssignmentReceipt {
    pub assignment_id: AssignmentId,
    pub resulting_revision: AggregateRevision,
    pub resulting_campaign_revision: AggregateRevision,
    pub audit_log_id: i64,
    pub was_idempotent_retry: bool,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct StartSession {
    pub principal: String,
    pub command_id: String,
    pub expected_assignment_revision: ExpectedRevision,
    pub assignment_id: AssignmentId,
    pub packet_digest: ContentDigest,
    pub custody: ProcessCustodyV2,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SessionReceipt {
    pub session_id: SessionId,
    pub resulting_revision: AggregateRevision,
    pub assignment_revision: AggregateRevision,
    pub audit_log_id: i64,
    pub was_idempotent_retry: bool,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TerminalReceipt {
    pub session_id: SessionId,
    pub session_state: SessionState,
    pub cost: TerminalCostV2,
    pub resulting_revision: AggregateRevision,
    pub campaign_revision: AggregateRevision,
    pub audit_log_id: i64,
    pub was_idempotent_retry: bool,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SessionStatus {
    pub session_id: SessionId,
    pub assignment_id: AssignmentId,
    pub state: SessionState,
    pub revision: AggregateRevision,
    pub cost: Option<TerminalCostV2>,
}

/// Every durable fact needed to reconcile a live session after the daemon
/// that created its `Child` handle has died. The packet bytes are re-read from
/// their registered CAS object on every recovery startup; this is not an
/// in-memory resume record and cannot launch another host.
#[derive(Clone, Debug)]
pub struct RestartRecoverySession {
    pub session_id: SessionId,
    pub expected_session_revision: AggregateRevision,
    pub custody: ProcessCustodyV2,
    pub packet: AssignmentPacketV2,
    pub packet_artifact: CasArtifact,
    pub canonical_packet_bytes: Vec<u8>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CampaignStatus {
    pub campaign_id: CampaignId,
    pub state: factory_protocol::CampaignState,
    pub kernel_build_id: KernelBuildId,
    pub application_revision_id: ApplicationRevisionId,
    pub repository_id: RepositoryId,
    pub aggregate_budget: MicroUsd,
    pub measured_cost: TerminalCostV2,
    pub revision: AggregateRevision,
    pub deadline_unix_millis: u64,
    pub delivery_target: u32,
    /// Present exactly for a failed campaign. This is the bounded daemon or
    /// operator fault that made a terminal campaign explainable without
    /// reconstructing a command fingerprint.
    pub failure_reason: Option<String>,
}

/// Concise immutable Git identities for the newest campaign work and newest
/// delivery. A claimed base exists before candidate submission; the other
/// candidate fields appear only as their durable facts are admitted.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CampaignProductIdentity {
    pub base_commit: Option<String>,
    pub candidate_tree: Option<String>,
    pub candidate_commit: Option<String>,
    pub delivered_commit: Option<String>,
    pub delivered_factory_cost_micro_usd: Option<u64>,
}

/// One bounded, read-only row in a campaign's provider-cost breakdown.
/// Grouping these rows by assignment role/model/outcome is presentation; the terminal
/// session fact remains the single source of its cost identity.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SessionCostBreakdown {
    pub session_id: SessionId,
    pub assignment_id: AssignmentId,
    pub assignment_role: AssignmentRole,
    pub model_provider: String,
    pub model_id: String,
    pub outcome: SessionState,
    pub cost: Option<TerminalCostV2>,
    /// Nullable provider diagnostics. They remain distinct from the exact
    /// provider-reported terminal cost used for economics.
    pub usage: UsageTotalsV2,
    /// Present only while this is the resident paid session.  The database
    /// derives it from its own clock, so a daemon restart cannot invent an
    /// elapsed duration or turn a status read into a write.
    pub elapsed_millis: Option<u64>,
}

/// Complete spend aggregation for one assignment-role/model/outcome tuple. The
/// application revision pins one model per assignment role, bounding one campaign to
/// three roles times six session outcomes.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SessionCostAggregate {
    pub assignment_role: AssignmentRole,
    pub model_provider: String,
    pub model_id: String,
    pub outcome: SessionState,
    pub session_count: u32,
    pub accounted_cost_micro_usd: u64,
    pub pending_cost_session_count: u32,
    pub unknown_cost_session_count: u32,
    pub exceeded_cost_session_count: u32,
}

/// A terminal evidence capability. Its fields are private so a caller cannot
/// manufacture artifact IDs or counts and bypass the CAS/read verification
/// gate. Obtain one with [`ProcessStore::verify_terminal_evidence_with_packet_bytes`].
#[derive(Clone, Debug)]
pub struct VerifiedTerminalEvidence {
    transcript_artifact_id: ArtifactId,
    execution_summary_artifact_id: Option<ArtifactId>,
    stdout_artifact_id: ArtifactId,
    stderr_artifact_id: ArtifactId,
    partial_transcript_artifact_id: Option<ArtifactId>,
    /// The sealed terminal assertion, distinct from the assignment's expected
    /// required-read manifest. The assertion proves daemon-observed reads;
    /// storing it under the old manifest-shaped name obscured the intentional
    /// inequality enforced at terminal transition.
    required_read_assertion_artifact_id: ArtifactId,
    required_read_expected_count: u32,
    required_read_satisfied_count: u32,
    usage: Option<UsageTotalsV2>,
}

/// CAS seals produced by the directly-owned child before terminal admission.
/// Stdout and stderr remain durable terminal facts even when the child exits
/// before it can produce a complete transcript; an optional partial transcript
/// records the restart/crash salvage seam without pretending it is complete.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct TerminalArtifactSeals {
    pub transcript: CasArtifact,
    /// Factory-only summary, separately sealed from the pure Tea trajectory.
    pub execution_summary: Option<CasArtifact>,
    pub stdout: CasArtifact,
    pub stderr: CasArtifact,
    pub partial_transcript: Option<CasArtifact>,
}

/// Durable summary of the narrow provider-effect ledger for one session.
/// `usage` is field-wise complete only where every recorded provider request
/// supplied that category. A session with no provider requests is a complete,
/// known zero ledger.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ProviderEffectLedger {
    pub provider_effect_count: u32,
    pub settled_provider_effect_count: u32,
    pub complete_usage: bool,
    pub complete_cost: bool,
    pub usage: UsageTotalsV2,
}

/// Closed state returned by a provider-effect intent or settlement.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ProviderEffectState {
    Started,
    Settled,
    Failed,
    Cancelled,
}

impl ProviderEffectState {
    #[must_use]
    pub const fn name(self) -> &'static str {
        match self {
            Self::Started => "started",
            Self::Settled => "settled",
            Self::Failed => "failed",
            Self::Cancelled => "cancelled",
        }
    }

    const fn code(self) -> i16 {
        match self {
            Self::Started => 0,
            Self::Settled => 1,
            Self::Failed => 2,
            Self::Cancelled => 3,
        }
    }

    fn from_settlement(value: &str) -> Result<Self, StoreError> {
        match value {
            "settled" => Ok(Self::Settled),
            "failed" => Ok(Self::Failed),
            "cancelled" => Ok(Self::Cancelled),
            _ => Err(StoreError::InvalidProcessCommand {
                field: "provider effect outcome",
            }),
        }
    }
}

#[derive(Clone, Debug)]
pub struct ProcessStore {
    pool: PgPool,
}

impl KernelStore {
    #[must_use]
    pub fn process_store(&self) -> ProcessStore {
        ProcessStore {
            pool: self.pool_for_authority(),
        }
    }
}

impl ProcessStore {
    /// Returns the one running campaign, if any, without creating a polling
    /// receipt. The partial unique index normally enforces this cardinality;
    /// reading two rows makes catalog/data corruption fail closed instead of
    /// letting the resident driver choose an arbitrary campaign.
    pub async fn current_running_campaign_id(&self) -> Result<Option<CampaignId>, StoreError> {
        let rows = sqlx::query_scalar!(
            "SELECT id FROM factory.campaigns WHERE lifecycle = $1 ORDER BY id ASC LIMIT 2",
            RUNNING
        )
        .fetch_all(&self.pool)
        .await?;
        match rows.as_slice() {
            [] => Ok(None),
            [id] => Ok(Some(CampaignId::new(*id)?)),
            values => Err(StoreError::RunningCampaignCardinality {
                observed_running: values.len(),
            }),
        }
    }

    /// Counts Product assignments already admitted for one campaign. The
    /// campaign driver uses this durable count to bound automatic recovery of
    /// a failed or incomplete Product turn without introducing an in-memory
    /// retry queue or allowing an unchanged campaign to spend indefinitely.
    pub async fn product_assignment_count(
        &self,
        campaign_id: CampaignId,
    ) -> Result<u32, StoreError> {
        let count: i64 = sqlx::query_scalar(
            "SELECT COUNT(*)::BIGINT
             FROM factory.assignments
             WHERE campaign_id = $1 AND assignment_role = 0",
        )
        .bind(campaign_id.get())
        .fetch_one(&self.pool)
        .await?;
        u32::try_from(count).map_err(|_| StoreError::InvalidProcessCommand {
            field: "product assignment count",
        })
    }

    /// Shares the fixed kernel pool only with the session runtime's closed
    /// evidence-closure reader. It is crate-private so no transport, actor,
    /// or application code gains an arbitrary query surface.
    pub(crate) fn pool_for_session_runtime(&self) -> PgPool {
        self.pool.clone()
    }

    /// Seals canonical bytes produced inside the trusted kernel and records
    /// their immutable artifact identity through the ordinary audit path.
    /// Actor files must use [`Self::adopt_and_register_actor_artifact`].
    pub(crate) async fn adopt_and_register_kernel_bytes(
        &self,
        cas: &CasStore,
        principal: &str,
        command_id: &str,
        kernel_build_id: KernelBuildId,
        bytes: &[u8],
    ) -> Result<(CasArtifact, storage::ArtifactReceipt), StoreError> {
        let sealed = cas.adopt_kernel_bytes(bytes)?;
        let revision = sqlx::query_scalar!(
            "SELECT revision FROM factory.kernel_builds WHERE build_digest = $1",
            &kernel_build_id.digest().as_bytes()[..]
        )
        .fetch_optional(&self.pool)
        .await?
        .ok_or(StoreError::UnknownKernelBuild { kernel_build_id })?;
        let kernel = storage::KernelStore::from_pool(self.pool.clone());
        let receipt = kernel
            .register_artifact(
                cas,
                &storage::RegisterArtifact {
                    principal: principal.to_owned(),
                    command_id: command_id.to_owned(),
                    expected_kernel_build_revision: ExpectedRevision::new(
                        storage::aggregate_revision_from_sql_for_process(revision)?,
                    ),
                    kernel_build_id,
                    sealed,
                },
            )
            .await?;
        Ok((sealed, receipt))
    }

    /// Adopts one bounded actor-owned file and registers its immutable CAS identity
    /// through the ordinary kernel artifact command. A failed registration may
    /// leave an unreferenced CAS object, but never a mutable or unverified row.
    pub async fn adopt_and_register_actor_artifact(
        &self,
        cas: &CasStore,
        principal: &str,
        command_id: &str,
        kernel_build_id: KernelBuildId,
        source_root: &std::path::Path,
        relative_path: &std::path::Path,
        byte_limit: u64,
    ) -> Result<(CasArtifact, storage::ArtifactReceipt), StoreError> {
        let sealed = cas.adopt(source_root, relative_path)?;
        if byte_limit == 0 || sealed.byte_length() > byte_limit {
            return Err(crate::cas::CasError::SizeLimitExceeded {
                path: source_root.join(relative_path),
                maximum: byte_limit,
                observed: sealed.byte_length(),
            }
            .into());
        }
        let revision = sqlx::query_scalar!(
            "SELECT revision FROM factory.kernel_builds WHERE build_digest = $1",
            &kernel_build_id.digest().as_bytes()[..]
        )
        .fetch_optional(&self.pool)
        .await?
        .ok_or(StoreError::UnknownKernelBuild { kernel_build_id })?;
        let kernel = storage::KernelStore::from_pool(self.pool.clone());
        let receipt = kernel
            .register_artifact(
                cas,
                &storage::RegisterArtifact {
                    principal: principal.to_owned(),
                    command_id: command_id.to_owned(),
                    expected_kernel_build_revision: ExpectedRevision::new(
                        storage::aggregate_revision_from_sql_for_process(revision)?,
                    ),
                    kernel_build_id,
                    sealed,
                },
            )
            .await?;
        Ok((sealed, receipt))
    }

    /// Returns a registered artifact only after re-verifying its physical CAS
    /// bytes and declared length.
    pub async fn registered_artifact(
        &self,
        cas: &CasStore,
        artifact_id: ArtifactId,
    ) -> Result<CasArtifact, StoreError> {
        let row = sqlx::query!(
            "SELECT digest, byte_length FROM factory.artifacts WHERE id = $1",
            artifact_id.get()
        )
        .fetch_optional(&self.pool)
        .await?
        .ok_or(StoreError::UnknownArtifact { artifact_id })?;
        let digest = ContentDigest::from_bytes(
            row.digest
                .as_slice()
                .try_into()
                .map_err(|_| StoreError::CorruptDigestColumn)?,
        );
        let sealed = cas.verify(digest)?;
        if sealed.byte_length()
            != u64::try_from(row.byte_length).map_err(|_| StoreError::ArtifactLengthOutOfRange)?
        {
            return Err(StoreError::ArtifactSealChanged);
        }
        Ok(sealed)
    }

    /// Re-verifies a registered object and proves the current actor principal
    /// registered that artifact through an audited command. A guessed artifact
    /// ID from another session is not a proposal capability.
    pub(crate) async fn registered_artifact_for_principal(
        &self,
        cas: &CasStore,
        principal: &str,
        artifact_id: ArtifactId,
    ) -> Result<CasArtifact, StoreError> {
        let owned = sqlx::query_scalar!(
            "SELECT EXISTS (
                 SELECT 1 FROM factory.audit_log
                 WHERE principal = $1 AND subject_kind = $2 AND subject_id = $3
             ) AS \"owned!\"",
            principal,
            3_i16,
            artifact_id.get(),
        )
        .fetch_one(&self.pool)
        .await?;
        if !owned {
            return Err(StoreError::ArtifactBuildMismatch);
        }
        self.registered_artifact(cas, artifact_id).await
    }

    /// Verifies the exact canonical wire packet, its out-of-band digest, and
    /// the CAS bytes referenced by the assignment. This is the sole packet
    /// digest check used by the concrete session RPC path.
    pub fn verify_packet_bytes(
        &self,
        cas: &CasStore,
        packet: &AssignmentPacketV2,
        packet_artifact: CasArtifact,
        canonical_packet_bytes: &[u8],
        expected_digest: ContentDigest,
    ) -> Result<(), StoreError> {
        let wire = factory_protocol::verify_assignment_packet_v2(
            canonical_packet_bytes,
            &expected_digest.to_hex(),
        )
        .map_err(|_| StoreError::InvalidPacketDigest)?;
        if wire.assignment_id != packet.assignment_id.get()
            || verify_wire_domain_mapping(&wire, packet).is_err()
        {
            return Err(StoreError::PacketIdentityMismatch);
        }
        if cas.read_verified(packet_artifact.digest())? != canonical_packet_bytes {
            return Err(StoreError::PacketArtifactDigestMismatch);
        }
        Ok(())
    }

    /// Reserves the next PostgreSQL assignment identity. Sequence gaps are
    /// harmless; the capability is consumed by canonical packet admission and
    /// prevents a caller from selecting an arbitrary durable assignment ID.
    pub async fn reserve_assignment_identity(
        &self,
    ) -> Result<AssignmentIdentityCapability, StoreError> {
        let id = sqlx::query_scalar!("SELECT nextval('factory.assignments_id_seq') AS \"id!\"")
            .fetch_one(&self.pool)
            .await?;
        Ok(AssignmentIdentityCapability {
            id: AssignmentId::new(id)?,
        })
    }

    /// Reconstructs the opaque actor identity only from an admitted running
    /// session and its exact packet. Local transport uses this to create the
    /// connection-bound capability; callers never receive identity fields
    /// they can edit or serialize.
    pub(crate) async fn actor_connection_identity(
        &self,
        session_id: SessionId,
        packet: &AssignmentPacketV2,
    ) -> Result<crate::local_transport::ActorConnectionIdentity, StoreError> {
        let row = sqlx::query!(
            "SELECT assignment_id, application_revision_id, campaign_id, assignment_role, lifecycle
             FROM factory.sessions WHERE id = $1",
            session_id.get()
        )
        .fetch_optional(&self.pool)
        .await?
        .ok_or(StoreError::UnknownSession { session_id })?;
        if row.lifecycle != SESSION_RUNNING
            || row.assignment_id != packet.assignment_id.get()
            || row.application_revision_id != packet.application_revision_id.get()
            || row.campaign_id != packet.campaign_id.get()
            || row.assignment_role != assignment_role_code(packet.assignment_role)
        {
            return Err(StoreError::PacketIdentityMismatch);
        }
        Ok(
            crate::local_transport::ActorConnectionIdentity::from_admitted_assignment(
                session_id,
                packet.assignment_id,
                packet.application_revision_id,
                packet.campaign_id,
                packet.assignment_role,
            ),
        )
    }

    /// Reconstructs the exact durable inputs needed by daemon-restart
    /// reconciliation. This is intentionally read-only: recovery first proves
    /// packet/CAS/custody identity, terminates the exact recorded group, then
    /// invokes the ordinary terminal transaction for each returned record.
    ///
    /// A session row exists only after `start_session` recorded a direct-child
    /// PID/PGID. There are therefore no "prepared session" rows to guess at
    /// during restart recovery.
    pub async fn restart_recovery_sessions(
        &self,
        cas: &CasStore,
    ) -> Result<Vec<RestartRecoverySession>, StoreError> {
        let rows = sqlx::query!(
            "SELECT s.id, s.revision, s.assignment_id, s.campaign_id,
                    s.kernel_build_id, s.application_revision_id, s.assignment_role AS session_assignment_role,
                    s.model_provider AS session_model_provider, s.model_id AS session_model_id,
                    s.thinking_level AS session_thinking_level,
                    s.input_price_micro_usd_per_million AS session_input_price,
                    s.output_price_micro_usd_per_million AS session_output_price,
                    s.cache_read_price_micro_usd_per_million AS session_cache_read_price,
                    s.cache_write_price_micro_usd_per_million AS session_cache_write_price,
                    s.pid, s.pgid,
                    s.process_started_at_unix_millis,
                    a.assignment_role AS assignment_assignment_role, a.target,
                    a.packet_artifact_id, a.packet_digest,
                    a.system_prompt_artifact_id, a.assignment_prompt_artifact_id,
                    a.required_read_manifest_artifact_id,
                    a.model_provider AS assignment_model_provider,
                    a.model_id AS assignment_model_id,
                    a.thinking_level AS assignment_thinking_level,
                    a.input_price_micro_usd_per_million AS assignment_input_price,
                    a.output_price_micro_usd_per_million AS assignment_output_price,
                    a.cache_read_price_micro_usd_per_million AS assignment_cache_read_price,
                    a.cache_write_price_micro_usd_per_million AS assignment_cache_write_price,
                    artifact.digest AS packet_artifact_digest,
                    artifact.byte_length AS packet_artifact_byte_length,
                    build.build_digest AS kernel_build_digest
             FROM factory.sessions s
             JOIN factory.assignments a ON a.id = s.assignment_id
             JOIN factory.artifacts artifact ON artifact.id = a.packet_artifact_id
             JOIN factory.kernel_builds build ON build.id = s.kernel_build_id
             WHERE s.lifecycle = $1
             ORDER BY s.id",
            SESSION_RUNNING
        )
        .fetch_all(&self.pool)
        .await?;
        let mut sessions = Vec::with_capacity(rows.len());
        for row in rows {
            let session_id = SessionId::new(row.id)?;
            let expected_session_revision =
                storage::aggregate_revision_from_sql_for_process(row.revision)?;
            let custody = ProcessCustodyV2 {
                pid: u32::try_from(row.pid).map_err(|_| StoreError::InvalidProcessCommand {
                    field: "persisted PID",
                })?,
                pgid: u32::try_from(row.pgid).map_err(|_| StoreError::InvalidProcessCommand {
                    field: "persisted PGID",
                })?,
                started_at_unix_millis: u64::try_from(row.process_started_at_unix_millis).map_err(
                    |_| StoreError::InvalidProcessCommand {
                        field: "persisted process start time",
                    },
                )?,
            };
            let packet_artifact_digest = ContentDigest::from_bytes(
                row.packet_artifact_digest
                    .as_slice()
                    .try_into()
                    .map_err(|_| StoreError::CorruptDigestColumn)?,
            );
            let packet_artifact = cas.verify(packet_artifact_digest)?;
            if packet_artifact.byte_length()
                != u64::try_from(row.packet_artifact_byte_length)
                    .map_err(|_| StoreError::ArtifactLengthOutOfRange)?
            {
                return Err(StoreError::PacketArtifactDigestMismatch);
            }
            let canonical_packet_bytes = cas.read(packet_artifact_digest)?;
            let packet_digest = ContentDigest::from_bytes(
                row.packet_digest
                    .as_slice()
                    .try_into()
                    .map_err(|_| StoreError::CorruptDigestColumn)?,
            );
            let wire = factory_protocol::verify_assignment_packet_v2(
                &canonical_packet_bytes,
                &packet_digest.to_hex(),
            )
            .map_err(|_| StoreError::InvalidPacketDigest)?;
            let packet = assignment_packet_from_wire(&wire)?;
            let persisted_build = ContentDigest::from_bytes(
                row.kernel_build_digest
                    .as_slice()
                    .try_into()
                    .map_err(|_| StoreError::CorruptDigestColumn)?,
            );
            if packet.assignment_id.get() != row.assignment_id
                || packet.campaign_id.get() != row.campaign_id
                || packet.application_revision_id.get() != row.application_revision_id
                || packet.kernel_build_id.digest() != persisted_build
                || packet.packet_digest != packet_digest
                || assignment_role_code(packet.assignment_role) != row.assignment_assignment_role
                || assignment_role_code(packet.assignment_role) != row.session_assignment_role
                || packet.target != row.target
                || packet.system_prompt_artifact_id.get() != row.system_prompt_artifact_id
                || packet.assignment_prompt_artifact_id.get() != row.assignment_prompt_artifact_id
                || packet.required_read_manifest_artifact_id.get()
                    != row.required_read_manifest_artifact_id
                || packet.model.provider != row.assignment_model_provider
                || packet.model.provider != row.session_model_provider
                || packet.model.model_id != row.assignment_model_id
                || packet.model.model_id != row.session_model_id
                || thinking_code(packet.model.thinking_level) != row.assignment_thinking_level
                || thinking_code(packet.model.thinking_level) != row.session_thinking_level
                || packet.model.price_input_micro_usd_per_million_tokens.get()
                    != u64::try_from(row.assignment_input_price)
                        .map_err(|_| StoreError::CorruptCostColumn)?
                || packet.model.price_input_micro_usd_per_million_tokens.get()
                    != u64::try_from(row.session_input_price)
                        .map_err(|_| StoreError::CorruptCostColumn)?
                || packet.model.price_output_micro_usd_per_million_tokens.get()
                    != u64::try_from(row.assignment_output_price)
                        .map_err(|_| StoreError::CorruptCostColumn)?
                || packet.model.price_output_micro_usd_per_million_tokens.get()
                    != u64::try_from(row.session_output_price)
                        .map_err(|_| StoreError::CorruptCostColumn)?
                || packet
                    .model
                    .price_cache_read_micro_usd_per_million_tokens
                    .get()
                    != u64::try_from(row.assignment_cache_read_price)
                        .map_err(|_| StoreError::CorruptCostColumn)?
                || packet
                    .model
                    .price_cache_read_micro_usd_per_million_tokens
                    .get()
                    != u64::try_from(row.session_cache_read_price)
                        .map_err(|_| StoreError::CorruptCostColumn)?
                || packet
                    .model
                    .price_cache_write_micro_usd_per_million_tokens
                    .get()
                    != u64::try_from(row.assignment_cache_write_price)
                        .map_err(|_| StoreError::CorruptCostColumn)?
                || packet
                    .model
                    .price_cache_write_micro_usd_per_million_tokens
                    .get()
                    != u64::try_from(row.session_cache_write_price)
                        .map_err(|_| StoreError::CorruptCostColumn)?
                || row.packet_artifact_id <= 0
                || row.kernel_build_id <= 0
            {
                return Err(StoreError::PacketIdentityMismatch);
            }
            sessions.push(RestartRecoverySession {
                session_id,
                expected_session_revision,
                custody,
                packet,
                packet_artifact,
                canonical_packet_bytes,
            });
        }
        Ok(sessions)
    }

    pub async fn start_campaign(
        &self,
        command: &StartCampaign,
    ) -> Result<CampaignReceipt, StoreError> {
        validate_command(&command.principal, &command.command_id)?;
        if command.delivery_target == 0 || command.deadline_unix_millis == 0 {
            return Err(StoreError::InvalidProcessCommand {
                field: "campaign bounds",
            });
        }
        let fingerprint = fingerprint_campaign(command);
        let mut tx = self.pool.begin().await?;
        lock_process_transaction(&mut tx).await?;
        if let Some(receipt) = find_audit(&mut tx, command, CAMPAIGN_START, fingerprint).await? {
            require_subject(&receipt, CAMPAIGN_SUBJECT)?;
            let campaign_id = CampaignId::new(receipt.subject_id)?;
            let pins = campaign_pinning(&mut tx, campaign_id).await?;
            tx.commit().await?;
            return Ok(CampaignReceipt {
                campaign_id,
                resulting_revision: receipt.resulting_revision,
                kernel_build_id: pins.kernel_build_id,
                application_revision_id: pins.application_revision_id,
                repository_id: pins.repository_id,
                audit_log_id: receipt.audit_log_id,
                was_idempotent_retry: true,
            });
        }
        if sqlx::query_scalar!(
            "SELECT id FROM factory.campaigns WHERE lifecycle = $1 LIMIT 1",
            RUNNING
        )
        .fetch_optional(&mut *tx)
        .await?
        .is_some()
        {
            return Err(StoreError::CampaignAlreadyRunning);
        }
        let build = sqlx::query!(
            "SELECT id, build_digest FROM factory.kernel_builds WHERE is_current FOR UPDATE"
        )
        .fetch_optional(&mut *tx)
        .await?
        .ok_or(StoreError::NoCurrentKernelBuild)?;
        let build_bytes: [u8; 32] = build
            .build_digest
            .as_slice()
            .try_into()
            .map_err(|_| StoreError::CorruptDigestColumn)?;
        let kernel_build_id = KernelBuildId::new(ContentDigest::from_bytes(build_bytes));
        let application = sqlx::query!(
            "SELECT repository_id, aggregate_revision, is_active
             FROM factory.application_revisions
             WHERE id = $1
             FOR SHARE",
            command.application_revision_id.get()
        )
        .fetch_optional(&mut *tx)
        .await?
        .ok_or(StoreError::UnknownApplicationRevision {
            application_revision_id: command.application_revision_id,
        })?;
        let application_revision =
            storage::aggregate_revision_from_sql_for_process(application.aggregate_revision)?;
        if command.expected_application_revision.get() != application_revision {
            return Err(StoreError::RevisionConflict {
                expected: command.expected_application_revision,
                current: application_revision,
            });
        }
        if !application.is_active {
            return Err(StoreError::ApplicationRevisionInactive {
                application_revision_id: command.application_revision_id,
            });
        }
        let repository_id = RepositoryId::new(application.repository_id)?;
        let budget = i64::try_from(command.aggregate_budget.get()).map_err(|_| {
            StoreError::InvalidProcessCommand {
                field: "aggregate budget",
            }
        })?;
        let deadline = i64::try_from(command.deadline_unix_millis)
            .map_err(|_| StoreError::InvalidProcessCommand { field: "deadline" })?;
        let target = i32::try_from(command.delivery_target).map_err(|_| {
            StoreError::InvalidProcessCommand {
                field: "delivery target",
            }
        })?;
        let deadline_open = sqlx::query_scalar!(
            "SELECT to_timestamp($1::DOUBLE PRECISION / 1000.0) > CURRENT_TIMESTAMP AS \"open!\"",
            deadline as f64
        )
        .fetch_one(&mut *tx)
        .await?;
        if !deadline_open {
            return Err(StoreError::CampaignDeadlineElapsed);
        }
        let id = sqlx::query_scalar!(
            "INSERT INTO factory.campaigns (
                 kernel_build_id, application_revision_id, repository_id, lifecycle,
                 aggregate_budget_micro_usd, deadline, delivery_target, revision
             ) VALUES ($1, $2, $3, $4, $5,
                       to_timestamp($6::DOUBLE PRECISION / 1000.0), $7, 0)
             RETURNING id",
            build.id,
            command.application_revision_id.get(),
            repository_id.get(),
            RUNNING,
            budget,
            deadline as f64,
            target,
        )
        .fetch_one(&mut *tx)
        .await?;
        let audit_log_id = insert_audit(
            &mut tx,
            &command.principal,
            &command.command_id,
            CAMPAIGN_START,
            fingerprint,
            CAMPAIGN_SUBJECT,
            id,
            AggregateRevision::initial(),
        )
        .await?;
        tx.commit().await?;
        Ok(CampaignReceipt {
            campaign_id: CampaignId::new(id)?,
            resulting_revision: AggregateRevision::initial(),
            kernel_build_id,
            application_revision_id: command.application_revision_id,
            repository_id,
            audit_log_id,
            was_idempotent_retry: false,
        })
    }

    pub(crate) async fn admit_campaign_cancellation(
        &self,
        command: &CancelCampaign,
    ) -> Result<CampaignCancellationAdmission, StoreError> {
        validate_command(&command.principal, &command.command_id)?;
        let fingerprint = fingerprint_cancel_campaign(command);
        let mut tx = self.pool.begin().await?;
        lock_process_transaction(&mut tx).await?;
        if let Some(receipt) = find_audit(&mut tx, command, CAMPAIGN_CANCEL, fingerprint).await? {
            require_subject(&receipt, CAMPAIGN_SUBJECT)?;
            let campaign_id = CampaignId::new(receipt.subject_id)?;
            let pins = campaign_pinning(&mut tx, campaign_id).await?;
            tx.commit().await?;
            return Ok(CampaignCancellationAdmission::Completed(CampaignReceipt {
                campaign_id,
                resulting_revision: receipt.resulting_revision,
                kernel_build_id: pins.kernel_build_id,
                application_revision_id: pins.application_revision_id,
                repository_id: pins.repository_id,
                audit_log_id: receipt.audit_log_id,
                was_idempotent_retry: true,
            }));
        }
        let campaign = sqlx::query!(
            "SELECT lifecycle, revision FROM factory.campaigns WHERE id = $1 FOR UPDATE",
            command.campaign_id.get()
        )
        .fetch_optional(&mut *tx)
        .await?
        .ok_or(StoreError::UnknownCampaign {
            campaign_id: command.campaign_id,
        })?;
        let current_revision = storage::aggregate_revision_from_sql_for_process(campaign.revision)?;
        if command.expected_revision.get() != current_revision {
            return Err(StoreError::RevisionConflict {
                expected: command.expected_revision,
                current: current_revision,
            });
        }
        if campaign.lifecycle != RUNNING {
            return Err(StoreError::CampaignClosed {
                campaign_id: command.campaign_id,
            });
        }
        if let Some(session_id) = sqlx::query_scalar!(
            "SELECT id FROM factory.sessions WHERE campaign_id = $1 AND lifecycle = $2 LIMIT 1",
            command.campaign_id.get(),
            SESSION_RUNNING
        )
        .fetch_optional(&mut *tx)
        .await?
        .map(SessionId::new)
        .transpose()?
        {
            tx.commit().await?;
            return Ok(CampaignCancellationAdmission::ActiveSession { session_id });
        }
        let receipt =
            cancel_campaign_in_transaction(&mut tx, command, current_revision, fingerprint).await?;
        tx.commit().await?;
        Ok(CampaignCancellationAdmission::Completed(receipt))
    }

    pub(crate) async fn finish_campaign_cancellation(
        &self,
        command: &CancelCampaign,
        reconciled: ReconciledSessionCancellation,
    ) -> Result<CampaignReceipt, StoreError> {
        validate_command(&command.principal, &command.command_id)?;
        if reconciled.campaign_id != command.campaign_id
            || reconciled.campaign_revision != command.expected_revision.get().next()?
        {
            return Err(StoreError::InvalidProcessCommand {
                field: "reconciled cancellation session",
            });
        }
        let fingerprint = fingerprint_cancel_campaign(command);
        let mut tx = self.pool.begin().await?;
        lock_process_transaction(&mut tx).await?;
        if let Some(receipt) = find_audit(&mut tx, command, CAMPAIGN_CANCEL, fingerprint).await? {
            require_subject(&receipt, CAMPAIGN_SUBJECT)?;
            let pins = campaign_pinning(&mut tx, command.campaign_id).await?;
            tx.commit().await?;
            return Ok(CampaignReceipt {
                campaign_id: command.campaign_id,
                resulting_revision: receipt.resulting_revision,
                kernel_build_id: pins.kernel_build_id,
                application_revision_id: pins.application_revision_id,
                repository_id: pins.repository_id,
                audit_log_id: receipt.audit_log_id,
                was_idempotent_retry: true,
            });
        }
        let campaign = sqlx::query!(
            "SELECT lifecycle, revision FROM factory.campaigns WHERE id = $1 FOR UPDATE",
            command.campaign_id.get()
        )
        .fetch_optional(&mut *tx)
        .await?
        .ok_or(StoreError::UnknownCampaign {
            campaign_id: command.campaign_id,
        })?;
        let current_revision = storage::aggregate_revision_from_sql_for_process(campaign.revision)?;
        if campaign.lifecycle != RUNNING {
            return Err(StoreError::CampaignClosed {
                campaign_id: command.campaign_id,
            });
        }
        if current_revision != reconciled.campaign_revision {
            return Err(StoreError::RevisionConflict {
                expected: ExpectedRevision::new(reconciled.campaign_revision),
                current: current_revision,
            });
        }
        // `ReconciledSessionCancellation` is minted only after the session
        // runtime has directly waited this exact daemon-owned child and
        // committed its terminal evidence. Rechecking a caller-selectable row
        // here would weaken that capability boundary and add no authority.
        let receipt =
            cancel_campaign_in_transaction(&mut tx, command, current_revision, fingerprint).await?;
        tx.commit().await?;
        Ok(receipt)
    }

    pub async fn cancel_campaign(
        &self,
        command: &CancelCampaign,
    ) -> Result<CampaignReceipt, StoreError> {
        match self.admit_campaign_cancellation(command).await? {
            CampaignCancellationAdmission::Completed(receipt) => Ok(receipt),
            CampaignCancellationAdmission::ActiveSession { .. } => {
                Err(StoreError::CampaignHasRunningSession {
                    campaign_id: command.campaign_id,
                })
            }
        }
    }

    /// Closes a running campaign after a daemon-owned failed Product path.
    /// It refuses to race a paid session and persists one bounded explanation
    /// atomically with the failed lifecycle; terminal session evidence remains
    /// the source for process-level detail where a session exists.
    pub async fn fail_campaign(
        &self,
        command: &FailCampaign,
    ) -> Result<CampaignReceipt, StoreError> {
        validate_command(&command.principal, &command.command_id)?;
        validate_failure_reason(&command.reason)?;
        let fingerprint = fingerprint_fail_campaign(command);
        let mut tx = self.pool.begin().await?;
        lock_process_transaction(&mut tx).await?;
        if let Some(receipt) = find_audit(&mut tx, command, CAMPAIGN_FAIL, fingerprint).await? {
            require_subject(&receipt, CAMPAIGN_SUBJECT)?;
            let campaign_id = CampaignId::new(receipt.subject_id)?;
            let pins = campaign_pinning(&mut tx, campaign_id).await?;
            tx.commit().await?;
            return Ok(CampaignReceipt {
                campaign_id,
                resulting_revision: receipt.resulting_revision,
                kernel_build_id: pins.kernel_build_id,
                application_revision_id: pins.application_revision_id,
                repository_id: pins.repository_id,
                audit_log_id: receipt.audit_log_id,
                was_idempotent_retry: true,
            });
        }
        let campaign = sqlx::query!(
            "SELECT lifecycle, revision FROM factory.campaigns WHERE id = $1 FOR UPDATE",
            command.campaign_id.get()
        )
        .fetch_optional(&mut *tx)
        .await?
        .ok_or(StoreError::UnknownCampaign {
            campaign_id: command.campaign_id,
        })?;
        let current_revision = storage::aggregate_revision_from_sql_for_process(campaign.revision)?;
        if command.expected_revision.get() != current_revision {
            return Err(StoreError::RevisionConflict {
                expected: command.expected_revision,
                current: current_revision,
            });
        }
        if campaign.lifecycle != RUNNING {
            return Err(StoreError::CampaignClosed {
                campaign_id: command.campaign_id,
            });
        }
        if sqlx::query_scalar!(
            "SELECT id FROM factory.sessions WHERE campaign_id = $1 AND lifecycle = $2 LIMIT 1",
            command.campaign_id.get(),
            SESSION_RUNNING
        )
        .fetch_optional(&mut *tx)
        .await?
        .is_some()
        {
            return Err(StoreError::CampaignHasRunningSession {
                campaign_id: command.campaign_id,
            });
        }
        let resulting_revision = current_revision.next()?;
        sqlx::query!(
            "UPDATE factory.campaigns
                 SET lifecycle = $1, failure_reason = $2, revision = $3
               WHERE id = $4",
            FAILED,
            &command.reason,
            i64::try_from(resulting_revision.get()).map_err(|_| StoreError::RevisionOutOfRange)?,
            command.campaign_id.get()
        )
        .execute(&mut *tx)
        .await?;
        let audit_log_id = insert_audit(
            &mut tx,
            &command.principal,
            &command.command_id,
            CAMPAIGN_FAIL,
            fingerprint,
            CAMPAIGN_SUBJECT,
            command.campaign_id.get(),
            resulting_revision,
        )
        .await?;
        let pins = campaign_pinning(&mut tx, command.campaign_id).await?;
        tx.commit().await?;
        Ok(CampaignReceipt {
            campaign_id: command.campaign_id,
            resulting_revision,
            kernel_build_id: pins.kernel_build_id,
            application_revision_id: pins.application_revision_id,
            repository_id: pins.repository_id,
            audit_log_id,
            was_idempotent_retry: false,
        })
    }

    pub async fn create_assignment(
        &self,
        cas: &CasStore,
        command: &CreateAssignment,
    ) -> Result<AssignmentReceipt, StoreError> {
        validate_command(&command.principal, &command.command_id)?;
        command.packet.validate()?;
        let wire = factory_protocol::verify_assignment_packet_v2(
            &command.packet_bytes,
            &command.packet.packet_digest.to_hex(),
        )
        .map_err(|_| StoreError::InvalidPacketDigest)?;
        if wire.assignment_id != command.identity.assignment_id().get()
            || command.packet.assignment_id != command.identity.assignment_id()
        {
            return Err(StoreError::PacketIdentityMismatch);
        }
        verify_wire_domain_mapping(&wire, &command.packet)?;
        cas.verify(command.packet_artifact.digest())?;
        if cas.read(command.packet_artifact.digest())? != command.packet_bytes {
            return Err(StoreError::PacketArtifactDigestMismatch);
        }
        if command.packet.campaign_id.get() <= 0 || command.attempt_ordinal == 0 {
            return Err(StoreError::InvalidProcessCommand {
                field: "assignment identity",
            });
        }
        if command.required_read_manifest_artifact_id
            != command.packet.required_read_manifest_artifact_id
        {
            return Err(StoreError::RequiredReadManifestMismatch);
        }
        if let Some(harness) = &command.harness {
            validate_harness_for_assignment(command, harness)?;
        }
        let fingerprint = fingerprint_assignment(command);
        let mut tx = self.pool.begin().await?;
        lock_process_transaction(&mut tx).await?;
        if let Some(receipt) = find_audit(&mut tx, command, ASSIGNMENT_CREATE, fingerprint).await? {
            require_subject(&receipt, ASSIGNMENT_SUBJECT)?;
            let assignment_id = AssignmentId::new(receipt.subject_id)?;
            if let Some(harness) = &command.harness {
                persist_harness_compilation(&mut tx, harness).await?;
            }
            let campaign_revision =
                current_campaign_revision(&mut tx, command.packet.campaign_id).await?;
            tx.commit().await?;
            return Ok(AssignmentReceipt {
                assignment_id,
                resulting_revision: receipt.resulting_revision,
                resulting_campaign_revision: campaign_revision,
                audit_log_id: receipt.audit_log_id,
                was_idempotent_retry: true,
            });
        }
        let campaign = sqlx::query!(
            "SELECT c.revision, c.lifecycle, c.aggregate_budget_micro_usd,
                    c.measured_cost_micro_usd, c.cost_state, c.application_revision_id,
                    c.kernel_build_id, kb.build_digest,
                    c.deadline > CURRENT_TIMESTAMP AS \"deadline_open!\"
             FROM factory.campaigns c
             JOIN factory.kernel_builds kb ON kb.id = c.kernel_build_id
             WHERE c.id = $1 FOR UPDATE",
            command.packet.campaign_id.get()
        )
        .fetch_optional(&mut *tx)
        .await?
        .ok_or(StoreError::UnknownCampaign {
            campaign_id: command.packet.campaign_id,
        })?;
        let campaign_revision =
            storage::aggregate_revision_from_sql_for_process(campaign.revision)?;
        if command.expected_campaign_revision.get() != campaign_revision {
            return Err(StoreError::RevisionConflict {
                expected: command.expected_campaign_revision,
                current: campaign_revision,
            });
        }
        if campaign.lifecycle != RUNNING {
            return Err(StoreError::CampaignClosed {
                campaign_id: command.packet.campaign_id,
            });
        }
        if !campaign.deadline_open {
            return Err(StoreError::CampaignDeadlineElapsed);
        }
        if campaign.cost_state != COST_KNOWN {
            return Err(StoreError::CampaignCostFrozen {
                campaign_id: command.packet.campaign_id,
            });
        }
        if campaign.application_revision_id != command.packet.application_revision_id.get()
            || campaign.build_digest.as_slice()
                != command.packet.kernel_build_id.digest().as_bytes()
        {
            return Err(StoreError::PacketIdentityMismatch);
        }
        validate_assignment_target_in_transaction(&mut tx, &command.packet).await?;
        let budget = u64::try_from(campaign.aggregate_budget_micro_usd)
            .map_err(|_| StoreError::CorruptCostColumn)?;
        let measured = u64::try_from(campaign.measured_cost_micro_usd)
            .map_err(|_| StoreError::CorruptCostColumn)?;
        let remaining = budget
            .checked_sub(measured)
            .ok_or(StoreError::CorruptCostColumn)?;
        if command.packet.remaining_campaign_allowance.get() != remaining {
            return Err(StoreError::RemainingAllowanceMismatch);
        }
        let packet_artifact_id = require_artifact_seal(
            &mut tx,
            command.packet_artifact,
            command.packet_bytes.len() as i64,
        )
        .await?;
        require_artifact(&mut tx, command.packet.system_prompt_artifact_id, None).await?;
        require_artifact(&mut tx, command.packet.assignment_prompt_artifact_id, None).await?;
        require_artifact(&mut tx, command.required_read_manifest_artifact_id, None).await?;
        verify_prompt_artifacts(&mut tx, &wire).await?;
        if let Some(harness) = &command.harness
            && harness.packet_artifact_id != packet_artifact_id
        {
            return Err(StoreError::HarnessPacketArtifactMismatch);
        }
        let id = command.identity.assignment_id().get();
        let role_code = assignment_role_code(command.packet.assignment_role);
        let model = &command.packet.model;
        sqlx::query!(
            "INSERT INTO factory.assignments (
                 id, campaign_id, kernel_build_id, application_revision_id,
                 office_id, assignment_role, target,
                 ticket_attempt_id, candidate_id,
                 packet_artifact_id, packet_digest, system_prompt_artifact_id,
                 assignment_prompt_artifact_id, required_read_manifest_artifact_id,
                 model_provider, model_id, thinking_level, context_token_limit, output_token_limit,
                 input_price_micro_usd_per_million, output_price_micro_usd_per_million,
                 cache_read_price_micro_usd_per_million, cache_write_price_micro_usd_per_million,
                 wall_limit_millis, output_byte_limit, terminal_operations_mask,
                 remaining_campaign_allowance_micro_usd, attempt_ordinal, lifecycle, revision
             ) OVERRIDING SYSTEM VALUE VALUES (
                 $1, $2, (SELECT id FROM factory.kernel_builds WHERE build_digest = $3), $4,
                 (SELECT id FROM factory.offices
                    WHERE application_revision_id = $4 AND assignment_role = $5),
                 $5, $6, $7, $8,
                 $9, $10, $11, $12, $13, $14, $15, $16, $17, $18, $19, $20, $21, $22, $23, $24, $25, $26, $27, 0, 0)",
            id,
            command.packet.campaign_id.get(),
            &command.packet.kernel_build_id.digest().as_bytes()[..],
            command.packet.application_revision_id.get(),
            role_code,
            command.packet.target,
            command.packet.ticket_attempt_id.map(TicketAttemptId::get),
            command.packet.candidate_id.map(CandidateId::get),
            packet_artifact_id.get(),
            &command.packet.packet_digest.as_bytes()[..],
            command.packet.system_prompt_artifact_id.get(),
            command.packet.assignment_prompt_artifact_id.get(),
            command.required_read_manifest_artifact_id.get(),
            model.provider,
            model.model_id,
            thinking_code(model.thinking_level),
            i32::try_from(model.context_token_limit).map_err(|_| StoreError::InvalidProcessCommand { field: "context token limit" })?,
            i32::try_from(model.output_token_limit).map_err(|_| StoreError::InvalidProcessCommand { field: "output token limit" })?,
            i64::try_from(model.price_input_micro_usd_per_million_tokens.get()).map_err(|_| StoreError::InvalidProcessCommand { field: "input price" })?,
            i64::try_from(model.price_output_micro_usd_per_million_tokens.get()).map_err(|_| StoreError::InvalidProcessCommand { field: "output price" })?,
            i64::try_from(model.price_cache_read_micro_usd_per_million_tokens.get()).map_err(|_| StoreError::InvalidProcessCommand { field: "cache read price" })?,
            i64::try_from(model.price_cache_write_micro_usd_per_million_tokens.get()).map_err(|_| StoreError::InvalidProcessCommand { field: "cache write price" })?,
            i64::try_from(command.packet.limits.wall_limit.get()).map_err(|_| StoreError::InvalidProcessCommand { field: "wall limit" })?,
            i32::try_from(command.packet.limits.output_byte_limit).map_err(|_| StoreError::InvalidProcessCommand { field: "output limit" })?,
            operation_mask(&command.packet.terminal_operations),
            i64::try_from(remaining).map_err(|_| StoreError::CorruptCostColumn)?,
            i32::try_from(command.attempt_ordinal).map_err(|_| StoreError::InvalidProcessCommand { field: "attempt ordinal" })?,
        )
        .execute(&mut *tx)
        .await?;
        if let Some(harness) = &command.harness {
            persist_harness_compilation(&mut tx, harness).await?;
        }
        let assignment_revision = AggregateRevision::initial();
        let next_campaign_revision = campaign_revision.next()?;
        sqlx::query!(
            "UPDATE factory.campaigns SET revision = $1 WHERE id = $2",
            i64::try_from(next_campaign_revision.get())
                .map_err(|_| StoreError::RevisionOutOfRange)?,
            command.packet.campaign_id.get()
        )
        .execute(&mut *tx)
        .await?;
        let audit_log_id = insert_audit(
            &mut tx,
            &command.principal,
            &command.command_id,
            ASSIGNMENT_CREATE,
            fingerprint,
            ASSIGNMENT_SUBJECT,
            id,
            assignment_revision,
        )
        .await?;
        tx.commit().await?;
        Ok(AssignmentReceipt {
            assignment_id: AssignmentId::new(id)?,
            resulting_revision: assignment_revision,
            resulting_campaign_revision: next_campaign_revision,
            audit_log_id,
            was_idempotent_retry: false,
        })
    }

    /// Registers custody before the host startup gate is released.
    pub async fn start_session(
        &self,
        command: &StartSession,
    ) -> Result<SessionReceipt, StoreError> {
        validate_command(&command.principal, &command.command_id)?;
        if command.custody.pid == 0
            || command.custody.pgid == 0
            || command.custody.started_at_unix_millis == 0
        {
            return Err(StoreError::InvalidProcessCommand {
                field: "process custody",
            });
        }
        let fingerprint = fingerprint_session_start(command);
        let mut tx = self.pool.begin().await?;
        lock_process_transaction(&mut tx).await?;
        if let Some(receipt) = find_audit(&mut tx, command, SESSION_START, fingerprint).await? {
            require_subject(&receipt, SESSION_SUBJECT)?;
            let assignment_revision =
                current_assignment_revision(&mut tx, command.assignment_id).await?;
            tx.commit().await?;
            return Ok(SessionReceipt {
                session_id: SessionId::new(receipt.subject_id)?,
                resulting_revision: receipt.resulting_revision,
                assignment_revision,
                audit_log_id: receipt.audit_log_id,
                was_idempotent_retry: true,
            });
        }
        let assignment = sqlx::query!(
            "SELECT a.campaign_id, a.kernel_build_id, a.application_revision_id,
                    a.packet_digest, a.revision, a.lifecycle, a.model_provider, a.model_id,
                    a.thinking_level, a.input_price_micro_usd_per_million,
                    a.output_price_micro_usd_per_million, a.cache_read_price_micro_usd_per_million,
                    a.cache_write_price_micro_usd_per_million, c.lifecycle AS campaign_lifecycle,
                    c.cost_state, c.deadline > CURRENT_TIMESTAMP AS \"deadline_open!\"
             FROM factory.assignments a JOIN factory.campaigns c ON c.id = a.campaign_id
             WHERE a.id = $1 FOR UPDATE",
            command.assignment_id.get()
        )
        .fetch_optional(&mut *tx)
        .await?
        .ok_or(StoreError::UnknownAssignment {
            assignment_id: command.assignment_id,
        })?;
        let assignment_revision =
            storage::aggregate_revision_from_sql_for_process(assignment.revision)?;
        if command.expected_assignment_revision.get() != assignment_revision {
            return Err(StoreError::RevisionConflict {
                expected: command.expected_assignment_revision,
                current: assignment_revision,
            });
        }
        if assignment.lifecycle != PREPARED {
            return Err(StoreError::AssignmentStateConflict {
                assignment_id: command.assignment_id,
            });
        }
        if assignment.campaign_lifecycle != RUNNING || assignment.cost_state != COST_KNOWN {
            return Err(StoreError::CampaignCostFrozen {
                campaign_id: CampaignId::new(assignment.campaign_id)?,
            });
        }
        if !assignment.deadline_open {
            return Err(StoreError::CampaignDeadlineElapsed);
        }
        if assignment.packet_digest.as_slice() != command.packet_digest.as_bytes() {
            return Err(StoreError::PacketIdentityMismatch);
        }
        if sqlx::query_scalar!(
            "SELECT id FROM factory.sessions WHERE lifecycle = $1 LIMIT 1",
            SESSION_RUNNING
        )
        .fetch_optional(&mut *tx)
        .await?
        .is_some()
        {
            return Err(StoreError::PaidSessionAlreadyRunning);
        }
        let session_id = sqlx::query_scalar!(
            "INSERT INTO factory.sessions (
                 assignment_id, campaign_id, kernel_build_id, application_revision_id,
                 office_id, assignment_role,
                 model_provider, model_id, thinking_level, input_price_micro_usd_per_million,
                 output_price_micro_usd_per_million, cache_read_price_micro_usd_per_million,
                 cache_write_price_micro_usd_per_million, pid, pgid,
                 process_started_at_unix_millis, lifecycle, revision
             ) SELECT a.id, a.campaign_id, a.kernel_build_id, a.application_revision_id,
                 a.office_id, a.assignment_role,
                 a.model_provider, a.model_id, a.thinking_level, a.input_price_micro_usd_per_million,
                 a.output_price_micro_usd_per_million, a.cache_read_price_micro_usd_per_million,
                 a.cache_write_price_micro_usd_per_million, $2, $3, $4, $5, 0
             FROM factory.assignments a WHERE a.id = $1 RETURNING id",
            command.assignment_id.get(),
            i32::try_from(command.custody.pid).map_err(|_| StoreError::InvalidProcessCommand { field: "PID" })?,
            i32::try_from(command.custody.pgid).map_err(|_| StoreError::InvalidProcessCommand { field: "PGID" })?,
            i64::try_from(command.custody.started_at_unix_millis).map_err(|_| StoreError::InvalidProcessCommand { field: "process start time" })?,
            SESSION_RUNNING)
            .fetch_one(&mut *tx).await?;
        let next_assignment_revision = assignment_revision.next()?;
        sqlx::query!(
            "UPDATE factory.assignments SET lifecycle = $1, revision = $2 WHERE id = $3",
            SESSION_RUNNING,
            i64::try_from(next_assignment_revision.get())
                .map_err(|_| StoreError::RevisionOutOfRange)?,
            command.assignment_id.get()
        )
        .execute(&mut *tx)
        .await?;
        let audit_log_id = insert_audit(
            &mut tx,
            &command.principal,
            &command.command_id,
            SESSION_START,
            fingerprint,
            SESSION_SUBJECT,
            session_id,
            AggregateRevision::initial(),
        )
        .await?;
        tx.commit().await?;
        Ok(SessionReceipt {
            session_id: SessionId::new(session_id)?,
            resulting_revision: AggregateRevision::initial(),
            assignment_revision: next_assignment_revision,
            audit_log_id,
            was_idempotent_retry: false,
        })
    }

    /// Canonical-packet variant used by the live session RPC path.
    pub async fn verify_terminal_evidence_with_packet_bytes(
        &self,
        cas: &CasStore,
        session_id: SessionId,
        packet: &AssignmentPacketV2,
        packet_artifact: CasArtifact,
        canonical_packet_bytes: &[u8],
        artifacts: TerminalArtifactSeals,
        assertion: SealedRequiredReadAssertion,
        usage: Option<UsageTotalsV2>,
    ) -> Result<VerifiedTerminalEvidence, StoreError> {
        self.verify_packet_bytes(
            cas,
            packet,
            packet_artifact,
            canonical_packet_bytes,
            packet.packet_digest,
        )?;
        self.verify_terminal_evidence_inner(
            cas,
            session_id,
            packet,
            packet_artifact,
            artifacts,
            assertion,
            usage,
            Some(canonical_packet_bytes),
        )
        .await
    }

    async fn verify_terminal_evidence_inner(
        &self,
        cas: &CasStore,
        session_id: SessionId,
        packet: &AssignmentPacketV2,
        packet_artifact: CasArtifact,
        artifacts: TerminalArtifactSeals,
        assertion: SealedRequiredReadAssertion,
        usage: Option<UsageTotalsV2>,
        canonical_packet_bytes: Option<&[u8]>,
    ) -> Result<VerifiedTerminalEvidence, StoreError> {
        packet.validate()?;
        if let Some(bytes) = canonical_packet_bytes {
            factory_protocol::verify_assignment_packet_v2(bytes, &packet.packet_digest.to_hex())
                .map_err(|_| StoreError::InvalidPacketDigest)?;
        } else {
            return Err(StoreError::InvalidPacketDigest);
        }
        cas.verify(artifacts.transcript.digest())?;
        if let Some(summary) = artifacts.execution_summary {
            cas.verify(summary.digest())?;
        }
        cas.verify(artifacts.stdout.digest())?;
        cas.verify(artifacts.stderr.digest())?;
        cas.verify(packet_artifact.digest())?;
        cas.verify(assertion.artifact().digest())?;
        if let Some(partial) = artifacts.partial_transcript {
            cas.verify(partial.digest())?;
        }
        if assertion.binding().session_id() != session_id {
            return Err(StoreError::RequiredReadManifestMismatch);
        }
        let row = sqlx::query!(
            "SELECT a.id, a.packet_artifact_id, a.packet_digest, a.required_read_manifest_artifact_id,
                    a.campaign_id, a.kernel_build_id, a.application_revision_id
             FROM factory.sessions s
             JOIN factory.assignments a ON a.id = s.assignment_id
             WHERE s.id = $1",
            session_id.get()
        )
        .fetch_optional(&self.pool)
        .await?
        .ok_or(StoreError::UnknownSession { session_id })?;
        if row.id != packet.assignment_id.get()
            || row.campaign_id != packet.campaign_id.get()
            || row.application_revision_id != packet.application_revision_id.get()
            || row.packet_digest.as_slice() != packet.packet_digest.as_bytes()
        {
            return Err(StoreError::PacketIdentityMismatch);
        }
        let packet_artifact_id = artifact_id_for_seal(&self.pool, packet_artifact).await?;
        if row.packet_artifact_id != packet_artifact_id.get() {
            return Err(StoreError::PacketArtifactDigestMismatch);
        }
        let packet_build = sqlx::query!(
            "SELECT id FROM factory.kernel_builds WHERE build_digest = $1",
            &packet.kernel_build_id.digest().as_bytes()[..]
        )
        .fetch_one(&self.pool)
        .await?;
        if row.kernel_build_id != packet_build.id {
            return Err(StoreError::PacketIdentityMismatch);
        }
        let expected_manifest_row = sqlx::query!(
            "SELECT digest, byte_length FROM factory.artifacts WHERE id = $1",
            row.required_read_manifest_artifact_id
        )
        .fetch_one(&self.pool)
        .await?;
        let expected_digest = ContentDigest::from_bytes(
            expected_manifest_row
                .digest
                .as_slice()
                .try_into()
                .map_err(|_| StoreError::RequiredReadManifestMismatch)?,
        );
        let expected_bytes = cas.read(expected_digest)?;
        if expected_bytes != canonical_required_manifest(&packet.required_reads) {
            return Err(StoreError::RequiredReadManifestMismatch);
        }
        if assertion.binding().assignment_id().get() != row.id
            || assertion.expected_manifest_artifact_id().get()
                != row.required_read_manifest_artifact_id
        {
            return Err(StoreError::RequiredReadManifestMismatch);
        }
        let transcript_id = artifact_id_for_seal(&self.pool, artifacts.transcript).await?;
        let execution_summary_artifact_id = match artifacts.execution_summary {
            Some(summary) => Some(artifact_id_for_seal(&self.pool, summary).await?),
            None => None,
        };
        if execution_summary_artifact_id == Some(transcript_id) {
            return Err(StoreError::InvalidProcessCommand {
                field: "execution summary evidence",
            });
        }
        let stdout_id = artifact_id_for_seal(&self.pool, artifacts.stdout).await?;
        let stderr_id = artifact_id_for_seal(&self.pool, artifacts.stderr).await?;
        let partial_transcript_id = match artifacts.partial_transcript {
            Some(partial) => Some(artifact_id_for_seal(&self.pool, partial).await?.get()),
            None => None,
        };
        let manifest_id = artifact_id_for_seal(&self.pool, assertion.artifact()).await?;
        let mut expected = packet.required_reads.clone();
        expected.sort_by(|left, right| left.path.as_str().cmp(right.path.as_str()));
        let mut asserted_expected = assertion.expected().to_vec();
        asserted_expected.sort_by(|left, right| left.path.as_str().cmp(right.path.as_str()));
        if expected != asserted_expected {
            return Err(StoreError::RequiredReadManifestMismatch);
        }
        let mut actual = assertion.observed().to_vec();
        for item in &actual {
            item.validate()?;
        }
        actual.sort_by(|left, right| left.path.as_str().cmp(right.path.as_str()));
        if cas.read(assertion.artifact().digest())? != assertion.canonical_bytes() {
            return Err(StoreError::RequiredReadManifestMismatch);
        }
        if row.required_read_manifest_artifact_id != packet.required_read_manifest_artifact_id.get()
        {
            return Err(StoreError::RequiredReadManifestMismatch);
        }
        Ok(VerifiedTerminalEvidence {
            transcript_artifact_id: transcript_id,
            execution_summary_artifact_id,
            stdout_artifact_id: stdout_id,
            stderr_artifact_id: stderr_id,
            partial_transcript_artifact_id: partial_transcript_id
                .map(ArtifactId::new)
                .transpose()?,
            required_read_assertion_artifact_id: manifest_id,
            required_read_expected_count: u32::try_from(packet.required_reads.len()).map_err(
                |_| StoreError::InvalidProcessCommand {
                    field: "required read count",
                },
            )?,
            required_read_satisfied_count: assertion.satisfied_count(),
            usage,
        })
    }

    /// Record provider request intent immediately before Tea dispatches it.
    /// The actor's session binding supplies assignment identity; all supplied
    /// IDs are verified against that binding before the ledger is touched.
    pub async fn provider_effect_start(
        &self,
        session_id: SessionId,
        assignment_id: AssignmentId,
        request: &factory_protocol::SessionProviderRequestStartRequest,
    ) -> Result<ProviderEffectState, StoreError> {
        validate_provider_effect_start(request)?;
        require_provider_effect_command("start", session_id, assignment_id, request.client_command_id.as_str(), &request.core_run_id, request.effect_id)?;
        let provider_surface_digest = request
            .provider_surface_digest
            .parse::<ContentDigest>()
            .map_err(|_| StoreError::InvalidProcessCommand {
                field: "provider effect provider surface digest",
            })?;
        let request_fingerprint = request
            .request_fingerprint
            .parse::<ContentDigest>()
            .map_err(|_| StoreError::InvalidProcessCommand {
                field: "provider effect request fingerprint",
            })?;
        let mut tx = self.pool.begin().await?;
        lock_process_transaction(&mut tx).await?;
        let session = sqlx::query(
            "SELECT assignment_id, revision, lifecycle, model_provider, model_id
               FROM factory.sessions WHERE id = $1 FOR UPDATE",
        )
        .bind(session_id.get())
        .fetch_optional(&mut *tx)
        .await?
        .ok_or(StoreError::UnknownSession { session_id })?;
        provider_effect_session_matches(
            &session,
            session_id,
            assignment_id,
            request.expected_revision,
            &request.provider,
            &request.model,
        )?;
        let existing = sqlx::query(
            "SELECT harness_snapshot_id, harness_revision_id, model_harness_profile_id,
                    provider_surface_digest, provider, model, request_fingerprint, state
               FROM factory.provider_effects
              WHERE session_id = $1 AND core_run_id = $2 AND effect_id = $3
              FOR UPDATE",
        )
        .bind(session_id.get())
        .bind(&request.core_run_id)
        .bind(i64::try_from(request.effect_id).map_err(|_| StoreError::InvalidProcessCommand {
            field: "provider effect ID",
        })?)
        .fetch_optional(&mut *tx)
        .await?;
        if let Some(existing) = existing {
            let matches = existing.try_get::<String, _>("harness_snapshot_id")? == request.harness_snapshot_id
                && existing.try_get::<String, _>("harness_revision_id")? == request.harness_revision_id
                && existing.try_get::<String, _>("model_harness_profile_id")? == request.model_harness_profile_id
                && existing.try_get::<Vec<u8>, _>("provider_surface_digest")? == provider_surface_digest.as_bytes()
                && existing.try_get::<String, _>("provider")? == request.provider
                && existing.try_get::<String, _>("model")? == request.model
                && existing.try_get::<Vec<u8>, _>("request_fingerprint")? == request_fingerprint.as_bytes();
            if !matches {
                return Err(StoreError::ProviderEffectConflict);
            }
            let state = provider_effect_state_from_code(existing.try_get("state")?)?;
            tx.commit().await?;
            return Ok(state);
        }
        sqlx::query(
            "INSERT INTO factory.provider_effects (
                session_id, assignment_id, core_run_id, effect_id,
                harness_snapshot_id, harness_revision_id, model_harness_profile_id,
                provider_surface_digest, provider, model, request_fingerprint, state
             ) VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12)",
        )
        .bind(session_id.get())
        .bind(assignment_id.get())
        .bind(&request.core_run_id)
        .bind(i64::try_from(request.effect_id).map_err(|_| StoreError::InvalidProcessCommand {
            field: "provider effect ID",
        })?)
        .bind(&request.harness_snapshot_id)
        .bind(&request.harness_revision_id)
        .bind(&request.model_harness_profile_id)
        .bind(provider_surface_digest.as_bytes().to_vec())
        .bind(&request.provider)
        .bind(&request.model)
        .bind(request_fingerprint.as_bytes().to_vec())
        .bind(ProviderEffectState::Started.code())
        .execute(&mut *tx)
        .await?;
        tx.commit().await?;
        Ok(ProviderEffectState::Started)
    }

    /// Settle one prior provider request. A duplicate byte-equivalent
    /// settlement is accepted; any conflicting replay remains fail-closed.
    pub async fn provider_effect_settle(
        &self,
        session_id: SessionId,
        assignment_id: AssignmentId,
        request: &factory_protocol::SessionProviderRequestSettleRequest,
    ) -> Result<ProviderEffectState, StoreError> {
        validate_provider_effect_settlement_identity(request)?;
        require_provider_effect_command("settle", session_id, assignment_id, request.client_command_id.as_str(), &request.core_run_id, request.effect_id)?;
        let state = ProviderEffectState::from_settlement(&request.outcome)?;
        let settlement = provider_effect_settlement(request, state)?;
        let mut tx = self.pool.begin().await?;
        lock_process_transaction(&mut tx).await?;
        let session = sqlx::query(
            "SELECT assignment_id, revision, lifecycle, model_provider, model_id
               FROM factory.sessions WHERE id = $1 FOR UPDATE",
        )
        .bind(session_id.get())
        .fetch_optional(&mut *tx)
        .await?
        .ok_or(StoreError::UnknownSession { session_id })?;
        provider_effect_session_matches_settlement(
            &session,
            session_id,
            assignment_id,
            request.expected_revision,
        )?;
        let existing = sqlx::query(
            "SELECT state, stop_reason, context_overflow, input_tokens, output_tokens,
                    cache_read_tokens, cache_write_tokens, reasoning_tokens,
                    reported_cost_micro_usd, failure_class
               FROM factory.provider_effects
              WHERE session_id = $1 AND core_run_id = $2 AND effect_id = $3
              FOR UPDATE",
        )
        .bind(session_id.get())
        .bind(&request.core_run_id)
        .bind(i64::try_from(request.effect_id).map_err(|_| StoreError::InvalidProcessCommand {
            field: "provider effect ID",
        })?)
        .fetch_optional(&mut *tx)
        .await?
        .ok_or(StoreError::ProviderEffectNotStarted)?;
        let existing_state = provider_effect_state_from_code(existing.try_get("state")?)?;
        if existing_state != ProviderEffectState::Started {
            if provider_effect_settlement_matches(&existing, &settlement, state)? {
                tx.commit().await?;
                return Ok(existing_state);
            }
            return Err(StoreError::ProviderEffectConflict);
        }
        sqlx::query(
            "UPDATE factory.provider_effects
                SET state = $1, stop_reason = $2, context_overflow = $3,
                    input_tokens = $4, output_tokens = $5, cache_read_tokens = $6,
                    cache_write_tokens = $7, reasoning_tokens = $8,
                    reported_cost_micro_usd = $9, failure_class = $10,
                    settled_at = CURRENT_TIMESTAMP
              WHERE session_id = $11 AND core_run_id = $12 AND effect_id = $13 AND state = $14",
        )
        .bind(state.code())
        .bind(&settlement.stop_reason)
        .bind(settlement.context_overflow)
        .bind(settlement.input_tokens)
        .bind(settlement.output_tokens)
        .bind(settlement.cache_read_tokens)
        .bind(settlement.cache_write_tokens)
        .bind(settlement.reasoning_tokens)
        .bind(settlement.reported_cost_micro_usd)
        .bind(&settlement.failure_class)
        .bind(session_id.get())
        .bind(&request.core_run_id)
        .bind(i64::try_from(request.effect_id).map_err(|_| StoreError::InvalidProcessCommand {
            field: "provider effect ID",
        })?)
        .bind(ProviderEffectState::Started.code())
        .execute(&mut *tx)
        .await?;
        tx.commit().await?;
        Ok(state)
    }

    /// Read the compact provider-effect ledger used during terminal
    /// reconciliation. No per-token events or response text are materialized.
    pub async fn provider_effect_ledger(
        &self,
        session_id: SessionId,
    ) -> Result<ProviderEffectLedger, StoreError> {
        let rows = sqlx::query(
            "SELECT state, input_tokens, output_tokens, cache_read_tokens,
                    cache_write_tokens, reasoning_tokens, reported_cost_micro_usd
               FROM factory.provider_effects
              WHERE session_id = $1
              ORDER BY id ASC",
        )
        .bind(session_id.get())
        .fetch_all(&self.pool)
        .await?;
        let ledger_rows = rows
            .iter()
            .map(provider_effect_ledger_row)
            .collect::<Result<Vec<_>, StoreError>>()?;
        provider_effect_ledger_from_rows(&ledger_rows)
    }

    pub async fn terminal_session(
        &self,
        principal: &str,
        command_id: &str,
        session_id: SessionId,
        report: &TerminalReportV2,
        evidence: VerifiedTerminalEvidence,
    ) -> Result<TerminalReceipt, StoreError> {
        validate_command(principal, command_id)?;
        let fingerprint = fingerprint_terminal(session_id, report, &evidence);
        let mut tx = self.pool.begin().await?;
        lock_process_transaction(&mut tx).await?;
        if let Some(receipt) = find_audit_by_key(
            &mut tx,
            principal,
            command_id,
            SESSION_TERMINAL,
            fingerprint,
        )
        .await?
        {
            require_subject(&receipt, SESSION_SUBJECT)?;
            let state = sqlx::query!("SELECT lifecycle, campaign_id, cost_state, cost_micro_usd FROM factory.sessions WHERE id = $1", session_id.get()).fetch_one(&mut *tx).await?;
            let campaign_revision =
                current_campaign_revision(&mut tx, CampaignId::new(state.campaign_id)?).await?;
            tx.commit().await?;
            return Ok(TerminalReceipt {
                session_id,
                session_state: session_state_from_code(state.lifecycle)?,
                cost: db_cost(state.cost_state, state.cost_micro_usd)?
                    .ok_or(StoreError::CorruptCostColumn)?,
                resulting_revision: receipt.resulting_revision,
                campaign_revision,
                audit_log_id: receipt.audit_log_id,
                was_idempotent_retry: true,
            });
        }
        let session = sqlx::query!(
            "SELECT s.assignment_id, s.campaign_id, s.revision, s.lifecycle,
                    s.input_price_micro_usd_per_million, s.output_price_micro_usd_per_million,
                    s.cache_read_price_micro_usd_per_million, s.cache_write_price_micro_usd_per_million,
                    a.packet_digest, a.terminal_operations_mask,
                    s.kernel_build_id,
                    a.required_read_manifest_artifact_id, a.revision AS assignment_revision,
                    c.revision AS campaign_revision, c.aggregate_budget_micro_usd,
                    c.measured_cost_micro_usd, c.cost_state AS campaign_cost_state,
                    c.lifecycle AS campaign_lifecycle
             FROM factory.sessions s JOIN factory.assignments a ON a.id = s.assignment_id
             JOIN factory.campaigns c ON c.id = s.campaign_id WHERE s.id = $1 FOR UPDATE",
            session_id.get()).fetch_optional(&mut *tx).await?
            .ok_or(StoreError::UnknownSession { session_id })?;
        let current_revision = storage::aggregate_revision_from_sql_for_process(session.revision)?;
        if report.expected_session_revision.get() != current_revision {
            return Err(StoreError::RevisionConflict {
                expected: report.expected_session_revision,
                current: current_revision,
            });
        }
        if session.lifecycle != SESSION_RUNNING {
            return Err(StoreError::SessionAlreadyTerminal { session_id });
        }
        if session.packet_digest.as_slice() != report.packet_digest.as_bytes() {
            return Err(StoreError::PacketIdentityMismatch);
        }
        match report.stop_reason {
            StopReasonV2::Completed => {
                if report.operation.is_none()
                    || operation_mask_one(report.operation) & session.terminal_operations_mask == 0
                {
                    return Err(StoreError::TerminalOperationNotAllowed);
                }
                if evidence.required_read_satisfied_count != evidence.required_read_expected_count {
                    return Err(StoreError::RequiredReadIncomplete);
                }
                if evidence.usage.is_none() {
                    return Err(StoreError::TerminalCostMismatch);
                }
            }
            StopReasonV2::UnknownCost => {
                if report.operation.is_some() {
                    return Err(StoreError::TerminalCostMismatch);
                }
            }
            _ if report.operation.is_some() => {
                return Err(StoreError::TerminalOperationNotAllowed);
            }
            _ => {}
        }
        if evidence.required_read_assertion_artifact_id.get()
            == session.required_read_manifest_artifact_id
        {
            return Err(StoreError::RequiredReadManifestMismatch);
        }
        let cost = match evidence.usage {
            Some(usage) => usage
                .provider_cost()
                .map_or(TerminalCostV2::Unknown, TerminalCostV2::Known),
            None => TerminalCostV2::Unknown,
        };
        let budget = u64::try_from(session.aggregate_budget_micro_usd)
            .map_err(|_| StoreError::CorruptCostColumn)?;
        let measured = u64::try_from(session.measured_cost_micro_usd)
            .map_err(|_| StoreError::CorruptCostColumn)?;
        let (next_cost_state, cost_value, next_measured, campaign_lifecycle) = match cost {
            TerminalCostV2::Known(value) if report.stop_reason == StopReasonV2::CostLimit => {
                let total = measured
                    .checked_add(value.get())
                    .ok_or(StoreError::CorruptCostColumn)?;
                (COST_EXCEEDED, Some(value.get()), total, FAILED)
            }
            TerminalCostV2::Known(value) if session.campaign_cost_state == COST_KNOWN => {
                let total = measured
                    .checked_add(value.get())
                    .ok_or(StoreError::CorruptCostColumn)?;
                if total > budget {
                    (COST_EXCEEDED, Some(value.get()), total, FAILED)
                } else {
                    (
                        COST_KNOWN,
                        Some(value.get()),
                        total,
                        session.campaign_lifecycle,
                    )
                }
            }
            TerminalCostV2::Known(value) => (
                session.campaign_cost_state,
                Some(value.get()),
                measured,
                session.campaign_lifecycle,
            ),
            TerminalCostV2::Exceeded(value) => (
                COST_EXCEEDED,
                Some(value.get()),
                measured.saturating_add(value.get()),
                FAILED,
            ),
            // An operator cancellation first terminates and reconciles the
            // exact active child, then commits the campaign cancellation. Keep
            // this brief bridge state running but cost-frozen so the scheduler
            // cannot admit another paid session before that typed command
            // completes. Other unknown-cost outcomes fail immediately.
            TerminalCostV2::Unknown if report.stop_reason == StopReasonV2::Cancelled => {
                (COST_UNKNOWN, None, measured, session.campaign_lifecycle)
            }
            TerminalCostV2::Unknown => (COST_UNKNOWN, None, measured, FAILED),
        };
        let state = session_state(report.stop_reason);
        let receipt_cost = match (next_cost_state, cost_value, cost) {
            (COST_EXCEEDED, Some(value), _) => TerminalCostV2::Exceeded(MicroUsd::new(value)),
            (_, _, value) => value,
        };
        let campaign_failure_reason = match (campaign_lifecycle, next_cost_state) {
            (FAILED, COST_UNKNOWN) => Some(UNKNOWN_TERMINAL_COST_FAILURE_REASON),
            (FAILED, COST_EXCEEDED) => Some(EXCEEDED_TERMINAL_COST_FAILURE_REASON),
            _ => None,
        };
        let next_revision = current_revision.next()?;
        if state == SessionState::Succeeded && evidence.execution_summary_artifact_id.is_none() {
            return Err(StoreError::InvalidProcessCommand {
                field: "execution summary evidence",
            });
        }
        let usage = evidence.usage.map(usage_sql).transpose()?;
        sqlx::query(
            "UPDATE factory.sessions SET lifecycle = $1, transcript_artifact_id = $2,
                 execution_summary_artifact_id = $3, stdout_artifact_id = $4, stderr_artifact_id = $5,
                 partial_transcript_artifact_id = $6, required_read_assertion_artifact_id = $7,
                 required_read_expected_count = $8, required_read_satisfied_count = $9,
                 input_tokens = $10, output_tokens = $11, cache_read_tokens = $12,
                 cache_write_tokens = $13, reasoning_tokens = $14,
                 reported_cost_micro_usd = $15, cost_state = $16, cost_micro_usd = $17,
                 stop_reason = $18, terminal_operation = $19, failure_class = $20,
                 terminal_at = CURRENT_TIMESTAMP, revision = $21
             WHERE id = $22 AND lifecycle = $23",
        )
        .bind(session_state_code(state))
        .bind(evidence.transcript_artifact_id.get())
        .bind(evidence.execution_summary_artifact_id.map(ArtifactId::get))
        .bind(evidence.stdout_artifact_id.get())
        .bind(evidence.stderr_artifact_id.get())
        .bind(evidence.partial_transcript_artifact_id.map(ArtifactId::get))
        .bind(evidence.required_read_assertion_artifact_id.get())
        .bind(i32::try_from(evidence.required_read_expected_count).map_err(|_| {
            StoreError::InvalidProcessCommand {
                field: "required read count",
            }
        })?)
        .bind(i32::try_from(evidence.required_read_satisfied_count).map_err(|_| {
            StoreError::InvalidProcessCommand {
                field: "required read count",
            }
        })?)
        .bind(usage.as_ref().map(|v| v.0))
        .bind(usage.as_ref().map(|v| v.1))
        .bind(usage.as_ref().map(|v| v.2))
        .bind(usage.as_ref().map(|v| v.3))
        .bind(usage.as_ref().and_then(|v| v.4))
        .bind(usage.as_ref().and_then(|v| v.5))
        .bind(next_cost_state)
        .bind(cost_value.and_then(|v| i64::try_from(v).ok()))
        .bind(stop_reason_code(report.stop_reason))
        .bind(report.operation.map(terminal_operation_code))
        .bind(failure_code(report.stop_reason))
        .bind(i64::try_from(next_revision.get()).map_err(|_| StoreError::RevisionOutOfRange)?)
        .bind(session_id.get())
        .bind(SESSION_RUNNING)
        .execute(&mut *tx)
        .await?;
        sqlx::query!("UPDATE factory.assignments SET lifecycle = $1, revision = revision + 1 WHERE id = $2 AND lifecycle = $3", assignment_state_code(state), session.assignment_id, SESSION_RUNNING).execute(&mut *tx).await?;
        let campaign_revision =
            storage::aggregate_revision_from_sql_for_process(session.campaign_revision)?.next()?;
        sqlx::query!("UPDATE factory.campaigns SET measured_cost_micro_usd = $1, cost_state = $2, lifecycle = $3, failure_reason = $4, revision = $5 WHERE id = $6", i64::try_from(next_measured).map_err(|_| StoreError::RevisionOutOfRange)?, next_cost_state.max(session.campaign_cost_state), campaign_lifecycle, campaign_failure_reason, i64::try_from(campaign_revision.get()).map_err(|_| StoreError::RevisionOutOfRange)?, session.campaign_id).execute(&mut *tx).await?;
        let audit_log_id = insert_audit(
            &mut tx,
            principal,
            command_id,
            SESSION_TERMINAL,
            fingerprint,
            SESSION_SUBJECT,
            session_id.get(),
            next_revision,
        )
        .await?;
        tx.commit().await?;
        Ok(TerminalReceipt {
            session_id,
            session_state: state,
            cost: receipt_cost,
            resulting_revision: next_revision,
            campaign_revision,
            audit_log_id,
            was_idempotent_retry: false,
        })
    }

    pub async fn session_status(&self, session_id: SessionId) -> Result<SessionStatus, StoreError> {
        let row = sqlx::query!("SELECT assignment_id, lifecycle, revision, cost_state, cost_micro_usd FROM factory.sessions WHERE id = $1", session_id.get()).fetch_optional(&self.pool).await?.ok_or(StoreError::UnknownSession { session_id })?;
        Ok(SessionStatus {
            session_id,
            assignment_id: AssignmentId::new(row.assignment_id)?,
            state: session_state_from_code(row.lifecycle)?,
            revision: storage::aggregate_revision_from_sql_for_process(row.revision)?,
            cost: db_cost(row.cost_state, row.cost_micro_usd)?,
        })
    }

    /// Derives aggregate admission/cost truth without writing a status receipt.
    pub async fn campaign_status(
        &self,
        campaign_id: CampaignId,
    ) -> Result<CampaignStatus, StoreError> {
        let row = sqlx::query!(
            "SELECT c.lifecycle, c.failure_reason, c.aggregate_budget_micro_usd, c.measured_cost_micro_usd,
                    c.cost_state, c.revision, c.application_revision_id, c.repository_id,
                    c.delivery_target,
                    FLOOR(EXTRACT(EPOCH FROM c.deadline) * 1000)::BIGINT AS \"deadline_unix_millis!\",
                    kb.build_digest
             FROM factory.campaigns AS c
             JOIN factory.kernel_builds AS kb ON kb.id = c.kernel_build_id
             WHERE c.id = $1",
            campaign_id.get()
        )
        .fetch_optional(&self.pool)
        .await?
        .ok_or(StoreError::UnknownCampaign { campaign_id })?;
        let measured = MicroUsd::new(
            u64::try_from(row.measured_cost_micro_usd)
                .map_err(|_| StoreError::CorruptCostColumn)?,
        );
        let measured_cost = match row.cost_state {
            COST_KNOWN => TerminalCostV2::Known(measured),
            COST_UNKNOWN => TerminalCostV2::Unknown,
            COST_EXCEEDED => TerminalCostV2::Exceeded(measured),
            _ => return Err(StoreError::CorruptCostColumn),
        };
        Ok(CampaignStatus {
            campaign_id,
            state: campaign_state_from_code(row.lifecycle)?,
            kernel_build_id: KernelBuildId::new(ContentDigest::from_bytes(
                row.build_digest
                    .as_slice()
                    .try_into()
                    .map_err(|_| StoreError::CorruptDigestColumn)?,
            )),
            application_revision_id: ApplicationRevisionId::new(row.application_revision_id)?,
            repository_id: RepositoryId::new(row.repository_id)?,
            aggregate_budget: MicroUsd::new(
                u64::try_from(row.aggregate_budget_micro_usd)
                    .map_err(|_| StoreError::CorruptCostColumn)?,
            ),
            measured_cost,
            revision: storage::aggregate_revision_from_sql_for_process(row.revision)?,
            deadline_unix_millis: u64::try_from(row.deadline_unix_millis)
                .map_err(|_| StoreError::InvalidProcessCommand { field: "deadline" })?,
            delivery_target: u32::try_from(row.delivery_target).map_err(|_| {
                StoreError::InvalidProcessCommand {
                    field: "delivery target",
                }
            })?,
            failure_reason: row.failure_reason,
        })
    }

    /// Returns the latest claimed/candidate identity together with the most
    /// recent completed delivery. This is a zero-write status projection;
    /// detailed candidate evidence remains on the navigation endpoint.
    pub async fn campaign_product_identity(
        &self,
        campaign_id: CampaignId,
    ) -> Result<CampaignProductIdentity, StoreError> {
        let row = sqlx::query!(
            "WITH latest_attempt AS (
                 SELECT id, claimed_commit
                 FROM factory.ticket_attempts
                 WHERE campaign_id = $1
                 ORDER BY id DESC
                 LIMIT 1
             ), latest_candidate AS (
                 SELECT candidate.base_commit, candidate.candidate_tree,
                        candidate.candidate_commit
                 FROM factory.candidates AS candidate
                 JOIN latest_attempt AS attempt
                   ON attempt.id = candidate.ticket_attempt_id
                 ORDER BY candidate.id DESC
                 LIMIT 1
             ), latest_delivery AS (
                 SELECT delivery.resulting_commit, delivery.factory_cost_micro_usd
                 FROM factory.deliveries AS delivery
                 JOIN factory.candidates AS candidate
                   ON candidate.id = delivery.candidate_id
                 JOIN factory.ticket_attempts AS attempt
                   ON attempt.id = candidate.ticket_attempt_id
                 WHERE attempt.campaign_id = $1
                 ORDER BY delivery.id DESC
                 LIMIT 1
             )
             SELECT COALESCE(
                        (SELECT base_commit FROM latest_candidate),
                        (SELECT claimed_commit FROM latest_attempt)
                    ) AS \"base_commit?\",
                    (SELECT candidate_tree FROM latest_candidate) AS \"candidate_tree?\",
                    (SELECT candidate_commit FROM latest_candidate) AS \"candidate_commit?\",
                    (SELECT resulting_commit FROM latest_delivery) AS \"delivered_commit?\",
                    (SELECT factory_cost_micro_usd FROM latest_delivery)
                        AS \"delivered_factory_cost_micro_usd?\"",
            campaign_id.get(),
        )
        .fetch_one(&self.pool)
        .await?;
        Ok(CampaignProductIdentity {
            base_commit: row.base_commit,
            candidate_tree: row.candidate_tree,
            candidate_commit: row.candidate_commit,
            delivered_commit: row.delivered_commit,
            delivered_factory_cost_micro_usd: row
                .delivered_factory_cost_micro_usd
                .map(|value| u64::try_from(value).map_err(|_| StoreError::CorruptCostColumn))
                .transpose()?,
        })
    }

    /// Pages the exact terminal/session facts used for assignment role, assignment,
    /// model, and outcome spend reporting. It is deliberately read-only and
    /// bounded; callers continue with the last returned `SessionId`.
    pub async fn campaign_session_costs(
        &self,
        campaign_id: CampaignId,
        after_session_id: Option<SessionId>,
        limit: u8,
    ) -> Result<Vec<SessionCostBreakdown>, StoreError> {
        if !(1..=100).contains(&limit) {
            return Err(StoreError::InvalidProcessCommand {
                field: "session cost breakdown limit",
            });
        }
        // Distinguish an empty campaign from an unknown campaign without a
        // status write or audit receipt.
        if sqlx::query_scalar!(
            "SELECT id FROM factory.campaigns WHERE id = $1",
            campaign_id.get()
        )
        .fetch_optional(&self.pool)
        .await?
        .is_none()
        {
            return Err(StoreError::UnknownCampaign { campaign_id });
        }
        let after = after_session_id.map_or(0, SessionId::get);
        let rows = sqlx::query(
            "SELECT id, assignment_id, assignment_role, model_provider, model_id,
                    lifecycle, cost_state, cost_micro_usd,
                    input_tokens, output_tokens, cache_read_tokens,
                    cache_write_tokens, reasoning_tokens,
                    CASE WHEN lifecycle = $4 THEN GREATEST(
                        0,
                        FLOOR(EXTRACT(EPOCH FROM CURRENT_TIMESTAMP - started_at) * 1000)::BIGINT
                    ) ELSE NULL END AS elapsed_millis
             FROM factory.sessions
             WHERE campaign_id = $1 AND id > $2
             ORDER BY id ASC
             LIMIT $3",
        )
        .bind(campaign_id.get())
        .bind(after)
        .bind(i64::from(limit))
        .bind(SESSION_RUNNING)
        .fetch_all(&self.pool)
        .await?;
        rows.into_iter()
            .map(|row| {
                let token = |field| {
                    row.try_get::<Option<i64>, _>(field)?
                        .map(u64::try_from)
                        .transpose()
                        .map_err(|_| StoreError::CorruptCostColumn)
                };
                Ok(SessionCostBreakdown {
                    session_id: SessionId::new(row.try_get("id")?)?,
                    assignment_id: AssignmentId::new(row.try_get("assignment_id")?)?,
                    assignment_role: assignment_role_from_code(row.try_get("assignment_role")?)?,
                    model_provider: row.try_get("model_provider")?,
                    model_id: row.try_get("model_id")?,
                    outcome: session_state_from_code(row.try_get("lifecycle")?)?,
                    cost: db_cost(row.try_get("cost_state")?, row.try_get("cost_micro_usd")?)?,
                    usage: UsageTotalsV2 {
                        input_tokens: token("input_tokens")?,
                        output_tokens: token("output_tokens")?,
                        cache_read_tokens: token("cache_read_tokens")?,
                        cache_write_tokens: token("cache_write_tokens")?,
                        reasoning_tokens: token("reasoning_tokens")?,
                        reported_cost_micro_usd: None,
                    },
                    elapsed_millis: row
                        .try_get::<Option<i64>, _>("elapsed_millis")?
                        .map(|value| {
                            u64::try_from(value).map_err(|_| StoreError::CorruptCostColumn)
                        })
                        .transpose()?,
                })
            })
            .collect()
    }

    /// Aggregates every session in the campaign without truncating spend at
    /// the recent-session display bound. The immutable application profile
    /// limits the result to eighteen assignment-role/model/outcome tuples.
    pub async fn campaign_session_cost_aggregates(
        &self,
        campaign_id: CampaignId,
    ) -> Result<Vec<SessionCostAggregate>, StoreError> {
        let rows = sqlx::query!(
            "SELECT assignment_role, model_provider, model_id, lifecycle,
                    COUNT(*)::BIGINT AS \"session_count!\",
                    COALESCE(SUM(cost_micro_usd), 0)::BIGINT
                        AS \"accounted_cost_micro_usd!\",
                    COUNT(*) FILTER (WHERE cost_state IS NULL)::BIGINT
                        AS \"pending_cost_session_count!\",
                    COUNT(*) FILTER (WHERE cost_state = $2)::BIGINT
                        AS \"unknown_cost_session_count!\",
                    COUNT(*) FILTER (WHERE cost_state = $3)::BIGINT
                        AS \"exceeded_cost_session_count!\"
             FROM factory.sessions
             WHERE campaign_id = $1
             GROUP BY assignment_role, model_provider, model_id, lifecycle
             ORDER BY assignment_role ASC, model_provider ASC, model_id ASC, lifecycle ASC",
            campaign_id.get(),
            COST_UNKNOWN,
            COST_EXCEEDED,
        )
        .fetch_all(&self.pool)
        .await?;
        if rows.len() > CAMPAIGN_SESSION_COST_AGGREGATE_MAXIMUM {
            return Err(StoreError::CorruptLifecycleColumn);
        }
        rows.into_iter()
            .map(|row| {
                Ok(SessionCostAggregate {
                    assignment_role: assignment_role_from_code(row.assignment_role)?,
                    model_provider: row.model_provider,
                    model_id: row.model_id,
                    outcome: session_state_from_code(row.lifecycle)?,
                    session_count: u32::try_from(row.session_count)
                        .map_err(|_| StoreError::CorruptCostColumn)?,
                    accounted_cost_micro_usd: u64::try_from(row.accounted_cost_micro_usd)
                        .map_err(|_| StoreError::CorruptCostColumn)?,
                    pending_cost_session_count: u32::try_from(row.pending_cost_session_count)
                        .map_err(|_| StoreError::CorruptCostColumn)?,
                    unknown_cost_session_count: u32::try_from(row.unknown_cost_session_count)
                        .map_err(|_| StoreError::CorruptCostColumn)?,
                    exceeded_cost_session_count: u32::try_from(row.exceeded_cost_session_count)
                        .map_err(|_| StoreError::CorruptCostColumn)?,
                })
            })
            .collect()
    }

    pub async fn process_audit_count(&self) -> Result<i64, StoreError> {
        Ok(sqlx::query_scalar!("SELECT count(*)::BIGINT AS \"count!\" FROM factory.audit_log WHERE operation IN ($1, $2, $3, $4, $5)", CAMPAIGN_START, CAMPAIGN_CANCEL, ASSIGNMENT_CREATE, SESSION_START, SESSION_TERMINAL).fetch_one(&self.pool).await?)
    }

    /// Read-only bounded fact counts used by provider-free acceptance tests;
    /// individual host events deliberately have no relation to these rows.
    pub async fn process_fact_counts(&self) -> Result<(i64, i64, i64, i64), StoreError> {
        let assignments =
            sqlx::query_scalar!("SELECT count(*)::BIGINT AS \"count!\" FROM factory.assignments")
                .fetch_one(&self.pool)
                .await?;
        let sessions =
            sqlx::query_scalar!("SELECT count(*)::BIGINT AS \"count!\" FROM factory.sessions")
                .fetch_one(&self.pool)
                .await?;
        let artifacts =
            sqlx::query_scalar!("SELECT count(*)::BIGINT AS \"count!\" FROM factory.artifacts")
                .fetch_one(&self.pool)
                .await?;
        let audits = self.process_audit_count().await?;
        Ok((assignments, sessions, artifacts, audits))
    }
}

async fn cancel_campaign_in_transaction(
    tx: &mut sqlx::Transaction<'_, Postgres>,
    command: &CancelCampaign,
    current_revision: AggregateRevision,
    fingerprint: ContentDigest,
) -> Result<CampaignReceipt, StoreError> {
    let resulting_revision = current_revision.next()?;
    sqlx::query!(
        "UPDATE factory.campaigns SET lifecycle = 3, revision = $1 WHERE id = $2",
        i64::try_from(resulting_revision.get()).map_err(|_| StoreError::RevisionOutOfRange)?,
        command.campaign_id.get()
    )
    .execute(&mut **tx)
    .await?;
    let audit_log_id = insert_audit(
        tx,
        &command.principal,
        &command.command_id,
        CAMPAIGN_CANCEL,
        fingerprint,
        CAMPAIGN_SUBJECT,
        command.campaign_id.get(),
        resulting_revision,
    )
    .await?;
    let pins = campaign_pinning(tx, command.campaign_id).await?;
    Ok(CampaignReceipt {
        campaign_id: command.campaign_id,
        resulting_revision,
        kernel_build_id: pins.kernel_build_id,
        application_revision_id: pins.application_revision_id,
        repository_id: pins.repository_id,
        audit_log_id,
        was_idempotent_retry: false,
    })
}

#[derive(Clone, Copy)]
struct CampaignPinning {
    kernel_build_id: KernelBuildId,
    application_revision_id: ApplicationRevisionId,
    repository_id: RepositoryId,
}

async fn campaign_pinning(
    tx: &mut sqlx::Transaction<'_, Postgres>,
    campaign_id: CampaignId,
) -> Result<CampaignPinning, StoreError> {
    let row = sqlx::query!(
        "SELECT kb.build_digest, c.application_revision_id, c.repository_id
         FROM factory.campaigns AS c
         JOIN factory.kernel_builds AS kb ON kb.id = c.kernel_build_id
         WHERE c.id = $1",
        campaign_id.get()
    )
    .fetch_optional(&mut **tx)
    .await?
    .ok_or(StoreError::UnknownCampaign { campaign_id })?;
    let build_digest: [u8; 32] = row
        .build_digest
        .as_slice()
        .try_into()
        .map_err(|_| StoreError::CorruptDigestColumn)?;
    Ok(CampaignPinning {
        kernel_build_id: KernelBuildId::new(ContentDigest::from_bytes(build_digest)),
        application_revision_id: ApplicationRevisionId::new(row.application_revision_id)?,
        repository_id: RepositoryId::new(row.repository_id)?,
    })
}
async fn current_campaign_revision(
    tx: &mut sqlx::Transaction<'_, Postgres>,
    id: CampaignId,
) -> Result<AggregateRevision, StoreError> {
    let value = sqlx::query_scalar!(
        "SELECT revision FROM factory.campaigns WHERE id = $1",
        id.get()
    )
    .fetch_one(&mut **tx)
    .await?;
    storage::aggregate_revision_from_sql_for_process(value)
}
async fn current_assignment_revision(
    tx: &mut sqlx::Transaction<'_, Postgres>,
    id: AssignmentId,
) -> Result<AggregateRevision, StoreError> {
    let value = sqlx::query_scalar!(
        "SELECT revision FROM factory.assignments WHERE id = $1",
        id.get()
    )
    .fetch_one(&mut **tx)
    .await?;
    storage::aggregate_revision_from_sql_for_process(value)
}

/// Rechecks the packet's closed target shape against the durable ticket graph
/// in the same transaction that creates the assignment.  Prompt `target`
/// remains descriptive only; it cannot redirect Engineering or Quality to a
/// different row.
async fn validate_assignment_target_in_transaction(
    tx: &mut sqlx::Transaction<'_, Postgres>,
    packet: &AssignmentPacketV2,
) -> Result<(), StoreError> {
    let target_exists = match (
        packet.assignment_role,
        packet.ticket_attempt_id,
        packet.candidate_id,
    ) {
        (AssignmentRole::ProductResearch, None, None) => true,
        (AssignmentRole::Engineering, Some(ticket_attempt_id), None) => sqlx::query_scalar!(
            "SELECT ta.id
                   FROM factory.ticket_attempts ta
                   JOIN factory.ticket_revisions tr ON tr.id = ta.ticket_revision_id
                  WHERE ta.id = $1
                    AND ta.campaign_id = $2
                    AND tr.application_revision_id = $3
                    AND ta.stage IN (0, 4)
                  FOR KEY SHARE",
            ticket_attempt_id.get(),
            packet.campaign_id.get(),
            packet.application_revision_id.get(),
        )
        .fetch_optional(&mut **tx)
        .await?
        .is_some(),
        (AssignmentRole::Quality, Some(ticket_attempt_id), Some(candidate_id)) => {
            sqlx::query_scalar!(
                "SELECT c.id
                   FROM factory.candidates c
                   JOIN factory.ticket_attempts ta ON ta.id = c.ticket_attempt_id
                   JOIN factory.ticket_revisions tr ON tr.id = ta.ticket_revision_id
                   LEFT JOIN factory.validations qv ON qv.candidate_id = c.id
                        AND qv.validation_scope = 1 AND qv.lifecycle = 1
                   LEFT JOIN factory.reviews qr ON qr.candidate_id = c.id
                  WHERE c.id = $1
                    AND c.ticket_attempt_id = $2
                    AND ta.campaign_id = $3
                    AND tr.application_revision_id = $4
                    AND c.lifecycle = 1 AND c.candidate_commit IS NOT NULL
                    AND (ta.stage IN (2, 6)
                         OR (ta.stage = 3 AND qv.id IS NOT NULL AND qr.id IS NULL))
                  FOR KEY SHARE OF c, ta, tr",
                candidate_id.get(),
                ticket_attempt_id.get(),
                packet.campaign_id.get(),
                packet.application_revision_id.get(),
            )
            .fetch_optional(&mut **tx)
            .await?
            .is_some()
        }
        _ => false,
    };
    if target_exists {
        Ok(())
    } else {
        Err(StoreError::PacketIdentityMismatch)
    }
}
async fn require_artifact(
    tx: &mut sqlx::Transaction<'_, Postgres>,
    id: ArtifactId,
    digest: Option<ContentDigest>,
) -> Result<(), StoreError> {
    let row = sqlx::query!(
        "SELECT a.digest, a.creating_kernel_build_id FROM factory.artifacts a WHERE a.id = $1",
        id.get()
    )
    .fetch_optional(&mut **tx)
    .await?
    .ok_or(StoreError::UnknownArtifact { artifact_id: id })?;
    if let Some(expected) = digest
        && row.digest.as_slice() != expected.as_bytes()
    {
        return Err(StoreError::PacketArtifactDigestMismatch);
    }
    Ok(())
}

async fn require_artifact_seal(
    tx: &mut sqlx::Transaction<'_, Postgres>,
    seal: CasArtifact,
    expected_length: i64,
) -> Result<ArtifactId, StoreError> {
    let row = sqlx::query!(
        "SELECT id, digest, byte_length FROM factory.artifacts
         WHERE digest = $1 AND byte_length = $2",
        &seal.digest().as_bytes()[..],
        expected_length
    )
    .fetch_optional(&mut **tx)
    .await?
    .ok_or(StoreError::UnregisteredTerminalArtifact)?;
    if row.digest.as_slice() != seal.digest().as_bytes()
        || row.byte_length != seal.byte_length() as i64
    {
        return Err(StoreError::PacketArtifactDigestMismatch);
    }
    Ok(ArtifactId::new(row.id)?)
}

async fn verify_prompt_artifacts(
    tx: &mut sqlx::Transaction<'_, Postgres>,
    wire: &factory_protocol::AssignmentPacketWireV2,
) -> Result<(), StoreError> {
    let system =
        decode_base64(&wire.system_prompt_bytes_b64).ok_or(StoreError::InvalidPacketDigest)?;
    let assignment =
        decode_base64(&wire.assignment_prompt_bytes_b64).ok_or(StoreError::InvalidPacketDigest)?;
    let system_digest = ContentDigest::from_str(&wire.system_prompt_digest)
        .map_err(|_| StoreError::InvalidPacketDigest)?;
    let assignment_digest = ContentDigest::from_str(&wire.assignment_prompt_digest)
        .map_err(|_| StoreError::InvalidPacketDigest)?;
    if ContentDigest::of_bytes(&system) != system_digest
        || ContentDigest::of_bytes(&assignment) != assignment_digest
    {
        return Err(StoreError::InvalidPacketDigest);
    }
    for (artifact_id, digest, bytes) in [
        (wire.system_prompt_artifact_id, system_digest, system),
        (
            wire.assignment_prompt_artifact_id,
            assignment_digest,
            assignment,
        ),
    ] {
        let row = sqlx::query!(
            "SELECT digest, byte_length FROM factory.artifacts WHERE id = $1",
            artifact_id
        )
        .fetch_optional(&mut **tx)
        .await?
        .ok_or(StoreError::UnknownArtifact {
            artifact_id: ArtifactId::new(artifact_id)?,
        })?;
        if row.digest.as_slice() != digest.as_bytes() || row.byte_length != bytes.len() as i64 {
            return Err(StoreError::PacketArtifactDigestMismatch);
        }
    }
    Ok(())
}

fn verify_wire_domain_mapping(
    wire: &factory_protocol::AssignmentPacketWireV2,
    packet: &AssignmentPacketV2,
) -> Result<(), StoreError> {
    let wire_build = ContentDigest::from_str(&wire.kernel_build_id)
        .map_err(|_| StoreError::InvalidPacketDigest)?;
    let wire_packet_digest = ContentDigest::from_str(&wire.packet_digest)
        .map_err(|_| StoreError::InvalidPacketDigest)?;
    if wire.campaign_id != packet.campaign_id.get()
        || wire.assignment_id != packet.assignment_id.get()
        || wire.application_revision_id != packet.application_revision_id.get()
        || wire_build != packet.kernel_build_id.digest()
        || wire.assignment_role != assignment_role_name(packet.assignment_role)
        || wire.target != packet.target
        || wire.ticket_attempt_id != packet.ticket_attempt_id.map(TicketAttemptId::get)
        || wire.candidate_id != packet.candidate_id.map(CandidateId::get)
        || wire.system_prompt_artifact_id != packet.system_prompt_artifact_id.get()
        || wire.assignment_prompt_artifact_id != packet.assignment_prompt_artifact_id.get()
        || wire.required_read_manifest_artifact_id
            != packet.required_read_manifest_artifact_id.get()
        || wire.workspace_root != packet.workspace_root.as_str()
        || wire.staging_root != packet.staging_root.as_str()
        || wire.remaining_campaign_allowance_micro_usd != packet.remaining_campaign_allowance.get()
        || wire.aggregate_revision != packet.revision.get()
        || wire_packet_digest != packet.packet_digest
    {
        return Err(StoreError::PacketIdentityMismatch);
    }
    if wire.model.provider != packet.model.provider
        || wire.model.model_id != packet.model.model_id
        || wire.model.thinking_level != thinking_name(packet.model.thinking_level)
        || wire.model.context_token_limit != packet.model.context_token_limit
        || wire.model.output_token_limit != packet.model.output_token_limit
        || wire.model.price_input_micro_usd_per_million_tokens
            != packet.model.price_input_micro_usd_per_million_tokens.get()
        || wire.model.price_output_micro_usd_per_million_tokens
            != packet.model.price_output_micro_usd_per_million_tokens.get()
        || wire.model.price_cache_read_micro_usd_per_million_tokens
            != packet
                .model
                .price_cache_read_micro_usd_per_million_tokens
                .get()
        || wire.model.price_cache_write_micro_usd_per_million_tokens
            != packet
                .model
                .price_cache_write_micro_usd_per_million_tokens
                .get()
        || wire.model.capability_flags.len() != packet.model.capability_flags.len()
        || wire
            .model
            .capability_flags
            .iter()
            .zip(&packet.model.capability_flags)
            .any(|(wire, domain)| {
                !matches!(
                    (wire.as_str(), domain),
                    ("reasoning", ModelCapabilityV2::Reasoning)
                )
            })
    {
        return Err(StoreError::PacketIdentityMismatch);
    }
    if wire.limits.wall_limit_millis != packet.limits.wall_limit.get()
        || wire.limits.output_byte_limit != packet.limits.output_byte_limit
        || wire.runtime.host_executable != packet.runtime.host_executable.as_str()
        || wire.runtime.core_head != packet.runtime.core_head
        || wire.runtime.core_source_digest != packet.runtime.core_source_digest.to_hex()
        || wire.runtime.rust_toolchain != packet.runtime.rust_toolchain
    {
        return Err(StoreError::PacketIdentityMismatch);
    }
    if wire.runtime.credential_env != packet.runtime.credential_env {
        return Err(StoreError::PacketIdentityMismatch);
    }
    let policy_bytes =
        decode_base64(&wire.policy_bytes_b64).ok_or(StoreError::InvalidPacketDigest)?;
    let policy_digest = ContentDigest::from_str(&wire.policy_digest)
        .map_err(|_| StoreError::InvalidPacketDigest)?;
    let policy_entrypoint = PolicyEntrypointV2::parse(&wire.policy_entrypoint)
        .map_err(|_| StoreError::PacketIdentityMismatch)?;
    if policy_digest != packet.policy_digest
        || wire.policy_byte_limit != packet.policy_byte_limit
        || policy_bytes != packet.policy_bytes
        || policy_entrypoint != packet.policy_entrypoint
    {
        return Err(StoreError::PacketIdentityMismatch);
    }
    // Repository/factory bases remain signed wire-only transport identity.
    // Ticket/candidate authority is deliberately duplicated as typed packet
    // IDs and checked again against the durable assignment relation above.
    if wire.required_reads.len() != packet.required_reads.len()
        || wire
            .required_reads
            .iter()
            .zip(&packet.required_reads)
            .any(|(wire, domain)| {
                wire.path != domain.path.as_str()
                    || wire.digest != domain.digest.to_hex()
                    || wire.reason != domain.reason
            })
        || wire.terminal_operations.len() != packet.terminal_operations.len()
        || wire
            .terminal_operations
            .iter()
            .zip(&packet.terminal_operations)
            .any(|(wire, domain)| wire != terminal_operation_name(*domain))
    {
        return Err(StoreError::PacketIdentityMismatch);
    }
    if wire.assignment_evidence.len() != packet.assignment_evidence.len()
        || wire
            .assignment_evidence
            .iter()
            .zip(&packet.assignment_evidence)
            .any(|(wire, domain)| {
                wire.role != domain.role.wire_name()
                    || wire.artifact_id != domain.artifact_id.get()
                    || wire.digest != domain.digest.to_hex()
                    || wire.byte_length != domain.byte_length
            })
    {
        return Err(StoreError::PacketIdentityMismatch);
    }
    Ok(())
}

/// Reconstitutes the persistence-facing packet from the exact signed wire
/// packet retained in CAS. This is used only for daemon-restart recovery;
/// ordinary assignment admission already owns both spellings. Every textual
/// discriminant is deliberately closed here even though the protocol parser
/// performed the same validation: restart must not turn a newer/unknown wire
/// value into a permissive local default.
fn assignment_packet_from_wire(
    wire: &factory_protocol::AssignmentPacketWireV2,
) -> Result<AssignmentPacketV2, StoreError> {
    let assignment_role = match wire.assignment_role.as_str() {
        "product_research" => AssignmentRole::ProductResearch,
        "engineering" => AssignmentRole::Engineering,
        "quality" => AssignmentRole::Quality,
        _ => return Err(StoreError::PacketIdentityMismatch),
    };
    let thinking_level = match wire.model.thinking_level.as_str() {
        "none" => ThinkingLevelV2::None,
        "low" => ThinkingLevelV2::Low,
        "medium" => ThinkingLevelV2::Medium,
        "high" => ThinkingLevelV2::High,
        "xhigh" => ThinkingLevelV2::XHigh,
        _ => return Err(StoreError::PacketIdentityMismatch),
    };
    let capability_flags = wire
        .model
        .capability_flags
        .iter()
        .map(|flag| match flag.as_str() {
            "reasoning" => Ok(ModelCapabilityV2::Reasoning),
            _ => Err(StoreError::PacketIdentityMismatch),
        })
        .collect::<Result<Vec<_>, _>>()?;
    let required_reads = wire
        .required_reads
        .iter()
        .map(|read| {
            Ok(ReadExactFileV2 {
                path: RepositoryRelativePath::parse(read.path.clone())?,
                digest: ContentDigest::from_str(&read.digest)?,
                reason: read.reason.clone(),
            })
        })
        .collect::<Result<Vec<_>, factory_protocol::ContractError>>()?;
    let policy_bytes =
        decode_base64(&wire.policy_bytes_b64).ok_or(StoreError::InvalidPacketDigest)?;
    let policy_digest = ContentDigest::from_str(&wire.policy_digest)
        .map_err(|_| StoreError::InvalidPacketDigest)?;
    let policy_entrypoint = PolicyEntrypointV2::parse(&wire.policy_entrypoint)
        .map_err(|_| StoreError::PacketIdentityMismatch)?;
    let terminal_operations = wire
        .terminal_operations
        .iter()
        .map(|operation| match operation.as_str() {
            "work_complete" => Ok(TerminalOperationV2::WorkComplete),
            "candidate_submit" => Ok(TerminalOperationV2::CandidateSubmit),
            "quality_submit_review" => Ok(TerminalOperationV2::QualitySubmitReview),
            _ => Err(StoreError::PacketIdentityMismatch),
        })
        .collect::<Result<Vec<_>, _>>()?;
    // The domain packet intentionally does not duplicate the host tool list.
    // Its exact signed bytes remain the authority, while this second closed
    // match prevents recovery from accepting a parser regression as an
    // unbounded tool set.
    for tool in &wire.tools {
        if !matches!(
            tool.as_str(),
            "workspace_read"
                | "workspace_write"
                | "workspace_edit"
                | "workspace_search"
                | "workspace_list"
                | "shell"
                | "forum_search"
                | "forum_list_topics"
                | "forum_list_threads"
                | "forum_read_thread"
                | "publication_create"
                | "artifact_seal"
                | "artifact_read"
                | "product_submit_ticket"
                | "candidate_checkpoint_regression"
                | "candidate_submit"
                | "quality_run_full_suite"
                | "quality_submit_review"
                | "work_complete"
        ) {
            return Err(StoreError::PacketIdentityMismatch);
        }
    }
    let packet = AssignmentPacketV2 {
        format_version: wire.format_version,
        campaign_id: CampaignId::new(wire.campaign_id)?,
        assignment_id: AssignmentId::new(wire.assignment_id)?,
        kernel_build_id: KernelBuildId::new(ContentDigest::from_str(&wire.kernel_build_id)?),
        application_revision_id: ApplicationRevisionId::new(wire.application_revision_id)?,
        assignment_role,
        target: wire.target.clone(),
        ticket_attempt_id: wire
            .ticket_attempt_id
            .map(TicketAttemptId::new)
            .transpose()?,
        candidate_id: wire.candidate_id.map(CandidateId::new).transpose()?,
        system_prompt_artifact_id: ArtifactId::new(wire.system_prompt_artifact_id)?,
        assignment_prompt_artifact_id: ArtifactId::new(wire.assignment_prompt_artifact_id)?,
        required_read_manifest_artifact_id: ArtifactId::new(
            wire.required_read_manifest_artifact_id,
        )?,
        policy_digest,
        policy_byte_limit: wire.policy_byte_limit,
        policy_bytes,
        policy_entrypoint,
        workspace_root: AbsoluteHostPath::parse(wire.workspace_root.clone())?,
        staging_root: AbsoluteHostPath::parse(wire.staging_root.clone())?,
        model: ModelProfileV2 {
            provider: wire.model.provider.clone(),
            model_id: wire.model.model_id.clone(),
            thinking_level,
            context_token_limit: wire.model.context_token_limit,
            output_token_limit: wire.model.output_token_limit,
            price_input_micro_usd_per_million_tokens: MicroUsd::new(
                wire.model.price_input_micro_usd_per_million_tokens,
            ),
            price_output_micro_usd_per_million_tokens: MicroUsd::new(
                wire.model.price_output_micro_usd_per_million_tokens,
            ),
            price_cache_read_micro_usd_per_million_tokens: MicroUsd::new(
                wire.model.price_cache_read_micro_usd_per_million_tokens,
            ),
            price_cache_write_micro_usd_per_million_tokens: MicroUsd::new(
                wire.model.price_cache_write_micro_usd_per_million_tokens,
            ),
            capability_flags,
        },
        limits: SessionLimitsV2 {
            wall_limit: DurationMillis::new(wire.limits.wall_limit_millis),
            output_byte_limit: wire.limits.output_byte_limit,
        },
        runtime: RuntimeIdentityV2 {
            host_executable: AbsoluteHostPath::parse(wire.runtime.host_executable.clone())?,
            core_head: wire.runtime.core_head.clone(),
            core_source_digest: ContentDigest::from_str(&wire.runtime.core_source_digest)?,
            rust_toolchain: wire.runtime.rust_toolchain.clone(),
            credential_env: wire.runtime.credential_env.clone(),
        },
        required_reads,
        assignment_evidence: wire
            .assignment_evidence
            .iter()
            .map(|evidence| {
                Ok(AssignmentEvidenceV2 {
                    role: AssignmentEvidenceRoleV2::parse_wire_name(&evidence.role)?,
                    artifact_id: ArtifactId::new(evidence.artifact_id)?,
                    digest: ContentDigest::from_str(&evidence.digest)?,
                    byte_length: evidence.byte_length,
                })
            })
            .collect::<Result<Vec<_>, factory_protocol::ContractError>>()?,
        terminal_operations,
        remaining_campaign_allowance: MicroUsd::new(wire.remaining_campaign_allowance_micro_usd),
        revision: AggregateRevision::from_persisted(wire.aggregate_revision),
        packet_digest: ContentDigest::from_str(&wire.packet_digest)?,
    };
    packet.validate()?;
    Ok(packet)
}

fn decode_base64(value: &str) -> Option<Vec<u8>> {
    if !value.len().is_multiple_of(4) {
        return None;
    }
    let mut out = Vec::with_capacity(value.len() / 4 * 3);
    let bytes = value.as_bytes();
    for chunk in bytes.as_chunks::<4>().0 {
        let a = base64_value(chunk[0])?;
        let b = base64_value(chunk[1])?;
        let c = if chunk[2] == b'=' {
            0
        } else {
            base64_value(chunk[2])?
        };
        let d = if chunk[3] == b'=' {
            0
        } else {
            base64_value(chunk[3])?
        };
        out.push((a << 2) | (b >> 4));
        if chunk[2] != b'=' {
            out.push((b << 4) | (c >> 2));
        }
        if chunk[3] != b'=' {
            out.push((c << 6) | d);
        }
    }
    Some(out)
}

fn base64_value(value: u8) -> Option<u8> {
    match value {
        b'A'..=b'Z' => Some(value - b'A'),
        b'a'..=b'z' => Some(value - b'a' + 26),
        b'0'..=b'9' => Some(value - b'0' + 52),
        b'+' => Some(62),
        b'/' => Some(63),
        _ => None,
    }
}

async fn artifact_id_for_seal(pool: &PgPool, seal: CasArtifact) -> Result<ArtifactId, StoreError> {
    let byte_length =
        i64::try_from(seal.byte_length()).map_err(|_| StoreError::ArtifactLengthOutOfRange)?;
    let row = sqlx::query_scalar!(
        "SELECT id FROM factory.artifacts WHERE digest = $1 AND byte_length = $2",
        &seal.digest().as_bytes()[..],
        byte_length
    )
    .fetch_optional(pool)
    .await?
    .ok_or(StoreError::UnregisteredTerminalArtifact)?;
    Ok(ArtifactId::new(row)?)
}

/// Canonical immutable required-read manifest bytes. Assignment materialization
/// seals precisely this spelling before it writes the packet; terminal
/// verification reuses it rather than accepting a second manifest format.
pub(crate) fn canonical_required_manifest(
    required: &[factory_protocol::ReadExactFileV2],
) -> Vec<u8> {
    let mut bytes = b"factory-read-manifest-v1\0".to_vec();
    bytes.extend_from_slice(&(required.len() as u32).to_be_bytes());
    for item in required {
        bytes.extend_from_slice(&(item.path.as_str().len() as u32).to_be_bytes());
        bytes.extend_from_slice(item.path.as_str().as_bytes());
        bytes.extend_from_slice(&item.digest.as_bytes());
        bytes.extend_from_slice(&(item.reason.len() as u32).to_be_bytes());
        bytes.extend_from_slice(item.reason.as_bytes());
    }
    bytes
}

#[derive(Clone, Copy)]
struct Audit {
    audit_log_id: i64,
    subject_kind: i16,
    subject_id: i64,
    resulting_revision: AggregateRevision,
}
async fn find_audit<T>(
    tx: &mut sqlx::Transaction<'_, Postgres>,
    command: &T,
    operation: &'static str,
    fingerprint: ContentDigest,
) -> Result<Option<Audit>, StoreError>
where
    T: CommandKey,
{
    find_audit_by_key(
        tx,
        command.principal(),
        command.command_id(),
        operation,
        fingerprint,
    )
    .await
}
async fn find_audit_by_key(
    tx: &mut sqlx::Transaction<'_, Postgres>,
    principal: &str,
    command_id: &str,
    operation: &'static str,
    fingerprint: ContentDigest,
) -> Result<Option<Audit>, StoreError> {
    let row = sqlx::query!("SELECT id, operation, command_fingerprint, subject_kind, subject_id, resulting_revision FROM factory.audit_log WHERE principal = $1 AND command_id = $2", principal, command_id).fetch_optional(&mut **tx).await?;
    let Some(row) = row else { return Ok(None) };
    if row.operation != operation || row.command_fingerprint.as_slice() != fingerprint.as_bytes() {
        return Err(StoreError::IdempotencyConflict {
            principal: principal.to_owned(),
            command_id: command_id.to_owned(),
        });
    }
    Ok(Some(Audit {
        audit_log_id: row.id,
        subject_kind: row.subject_kind,
        subject_id: row.subject_id,
        resulting_revision: storage::aggregate_revision_from_sql_for_process(
            row.resulting_revision,
        )?,
    }))
}
async fn insert_audit(
    tx: &mut sqlx::Transaction<'_, Postgres>,
    principal: &str,
    command_id: &str,
    operation: &'static str,
    fingerprint: ContentDigest,
    subject_kind: i16,
    subject_id: i64,
    revision: AggregateRevision,
) -> Result<i64, StoreError> {
    let digest = fingerprint.as_bytes();
    Ok(sqlx::query_scalar!("INSERT INTO factory.audit_log (principal, command_id, operation, command_fingerprint, subject_kind, subject_id, resulting_revision) VALUES ($1, $2, $3, $4, $5, $6, $7) RETURNING id", principal, command_id, operation, &digest[..], subject_kind, subject_id, i64::try_from(revision.get()).map_err(|_| StoreError::RevisionOutOfRange)?).fetch_one(&mut **tx).await?)
}
trait CommandKey {
    fn principal(&self) -> &str;
    fn command_id(&self) -> &str;
}
impl CommandKey for StartCampaign {
    fn principal(&self) -> &str {
        &self.principal
    }
    fn command_id(&self) -> &str {
        &self.command_id
    }
}
impl CommandKey for CancelCampaign {
    fn principal(&self) -> &str {
        &self.principal
    }
    fn command_id(&self) -> &str {
        &self.command_id
    }
}
impl CommandKey for FailCampaign {
    fn principal(&self) -> &str {
        &self.principal
    }

    fn command_id(&self) -> &str {
        &self.command_id
    }
}
impl CommandKey for CreateAssignment {
    fn principal(&self) -> &str {
        &self.principal
    }
    fn command_id(&self) -> &str {
        &self.command_id
    }
}
impl CommandKey for StartSession {
    fn principal(&self) -> &str {
        &self.principal
    }
    fn command_id(&self) -> &str {
        &self.command_id
    }
}
fn require_subject(audit: &Audit, expected: i16) -> Result<(), StoreError> {
    if audit.subject_kind == expected {
        Ok(())
    } else {
        Err(StoreError::AuditSubjectKindMismatch)
    }
}
async fn lock_process_transaction(
    tx: &mut sqlx::Transaction<'_, Postgres>,
) -> Result<(), StoreError> {
    sqlx::query!("SELECT pg_advisory_xact_lock($1)", i64::MIN + 5)
        .execute(&mut **tx)
        .await?;
    Ok(())
}
fn validate_command(principal: &str, command_id: &str) -> Result<(), StoreError> {
    for (field, value) in [("principal", principal), ("command ID", command_id)] {
        if value.is_empty()
            || value.len() > 160
            || !value
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b':' | b'_' | b'-'))
        {
            return Err(StoreError::InvalidProcessCommand { field });
        }
    }
    Ok(())
}

/// A daemon fault is explainable through the deterministic command identity,
/// but it is not a free-form report channel. Keep it bounded and printable so
/// the same fact cannot become an unbounded audit payload.
fn validate_failure_reason(reason: &str) -> Result<(), StoreError> {
    if reason.is_empty() || reason.len() > 240 || reason.contains('\0') {
        return Err(StoreError::InvalidProcessCommand {
            field: "campaign failure reason",
        });
    }
    Ok(())
}
fn hash_str(h: &mut blake3::Hasher, value: &str) {
    h.update(&(value.len() as u64).to_be_bytes());
    h.update(value.as_bytes());
}
fn hash_i64(h: &mut blake3::Hasher, value: i64) {
    h.update(&value.to_be_bytes());
}
fn hash_u32(h: &mut blake3::Hasher, value: u32) {
    h.update(&value.to_be_bytes());
}
fn hash_u64(h: &mut blake3::Hasher, value: u64) {
    h.update(&value.to_be_bytes());
}
fn hash_digest(h: &mut blake3::Hasher, value: ContentDigest) {
    h.update(&value.as_bytes());
}
fn fingerprint_campaign(c: &StartCampaign) -> ContentDigest {
    let mut h = blake3::Hasher::new();
    hash_str(&mut h, CAMPAIGN_START);
    hash_str(&mut h, &c.principal);
    hash_str(&mut h, &c.command_id);
    hash_u64(&mut h, c.expected_application_revision.get().get());
    hash_i64(&mut h, c.application_revision_id.get());
    hash_u64(&mut h, c.aggregate_budget.get());
    hash_u64(&mut h, c.deadline_unix_millis);
    hash_u32(&mut h, c.delivery_target);
    ContentDigest::from_bytes(*h.finalize().as_bytes())
}
fn fingerprint_cancel_campaign(c: &CancelCampaign) -> ContentDigest {
    let mut h = blake3::Hasher::new();
    hash_str(&mut h, CAMPAIGN_CANCEL);
    hash_str(&mut h, &c.principal);
    hash_str(&mut h, &c.command_id);
    hash_u64(&mut h, c.expected_revision.get().get());
    hash_i64(&mut h, c.campaign_id.get());
    ContentDigest::from_bytes(*h.finalize().as_bytes())
}
fn fingerprint_fail_campaign(c: &FailCampaign) -> ContentDigest {
    let mut h = blake3::Hasher::new();
    hash_str(&mut h, CAMPAIGN_FAIL);
    hash_str(&mut h, &c.principal);
    hash_str(&mut h, &c.command_id);
    hash_u64(&mut h, c.expected_revision.get().get());
    hash_i64(&mut h, c.campaign_id.get());
    hash_str(&mut h, &c.reason);
    ContentDigest::from_bytes(*h.finalize().as_bytes())
}
fn fingerprint_assignment(c: &CreateAssignment) -> ContentDigest {
    let mut h = blake3::Hasher::new();
    hash_str(&mut h, ASSIGNMENT_CREATE);
    hash_str(&mut h, &c.principal);
    hash_str(&mut h, &c.command_id);
    hash_u64(&mut h, c.expected_campaign_revision.get().get());
    // The canonical unsigned wire digest is the sole packet identity. The
    // persistence DTO is only a checked projection and has no independent
    // hash algorithm.
    hash_digest(&mut h, c.packet.packet_digest);
    hash_digest(&mut h, c.packet_artifact.digest());
    hash_i64(&mut h, c.required_read_manifest_artifact_id.get());
    hash_u32(&mut h, c.attempt_ordinal);
    ContentDigest::from_bytes(*h.finalize().as_bytes())
}

/// A harness is kernel-derived compiler output. Its visible packet artifacts
/// must be the same seals that the assignment transition is about to admit;
/// otherwise a caller could pair a valid packet with an unrelated narrative
/// of how it was supposedly compiled.
fn validate_harness_for_assignment(
    command: &CreateAssignment,
    harness: &RecordHarnessCompilation,
) -> Result<(), StoreError> {
    harness.validate()?;
    if harness.assignment_id != command.identity.assignment_id()
        || harness.application_revision_id != command.packet.application_revision_id
        || harness.assignment_role != command.packet.assignment_role
        || harness.system_prompt_artifact_id != command.packet.system_prompt_artifact_id
        || harness.assignment_prompt_artifact_id != command.packet.assignment_prompt_artifact_id
        || harness.packet_digest != command.packet.packet_digest
    {
        return Err(StoreError::PacketIdentityMismatch);
    }
    Ok(())
}

fn fingerprint_session_start(c: &StartSession) -> ContentDigest {
    let mut h = blake3::Hasher::new();
    hash_str(&mut h, SESSION_START);
    hash_str(&mut h, &c.principal);
    hash_str(&mut h, &c.command_id);
    hash_u64(&mut h, c.expected_assignment_revision.get().get());
    hash_i64(&mut h, c.assignment_id.get());
    hash_digest(&mut h, c.packet_digest);
    hash_u32(&mut h, c.custody.pid);
    hash_u32(&mut h, c.custody.pgid);
    hash_u64(&mut h, c.custody.started_at_unix_millis);
    ContentDigest::from_bytes(*h.finalize().as_bytes())
}
fn fingerprint_terminal(
    id: SessionId,
    r: &TerminalReportV2,
    evidence: &VerifiedTerminalEvidence,
) -> ContentDigest {
    let mut h = blake3::Hasher::new();
    hash_str(&mut h, SESSION_TERMINAL);
    hash_i64(&mut h, id.get());
    hash_digest(&mut h, r.packet_digest);
    hash_u64(&mut h, r.expected_session_revision.get().get());
    hash_i64(&mut h, evidence.transcript_artifact_id.get());
    hash_optional_artifact_id(&mut h, evidence.execution_summary_artifact_id);
    hash_i64(&mut h, evidence.required_read_assertion_artifact_id.get());
    hash_u32(&mut h, evidence.required_read_expected_count);
    hash_u32(&mut h, evidence.required_read_satisfied_count);
    hash_u32(&mut h, r.stop_reason as u32);
    if let Some(operation) = r.operation {
        hash_u32(&mut h, operation as u32);
    } else {
        hash_u32(&mut h, u32::MAX);
    }
    if let Some(usage) = evidence.usage {
        hash_optional_u64(&mut h, usage.input_tokens);
        hash_optional_u64(&mut h, usage.output_tokens);
        hash_optional_u64(&mut h, usage.cache_read_tokens);
        hash_optional_u64(&mut h, usage.cache_write_tokens);
        hash_optional_u64(&mut h, usage.reasoning_tokens);
        hash_optional_u64(&mut h, usage.reported_cost_micro_usd.map(MicroUsd::get));
    } else {
        hash_u64(&mut h, u64::MAX);
    }
    hash_digest(&mut h, r.report_digest);
    ContentDigest::from_bytes(*h.finalize().as_bytes())
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct ProviderEffectSettlement {
    stop_reason: String,
    context_overflow: bool,
    input_tokens: Option<i64>,
    output_tokens: Option<i64>,
    cache_read_tokens: Option<i64>,
    cache_write_tokens: Option<i64>,
    reasoning_tokens: Option<i64>,
    reported_cost_micro_usd: Option<i64>,
    failure_class: Option<String>,
}

#[derive(Clone, Copy, Debug)]
struct ProviderEffectLedgerRow {
    state: ProviderEffectState,
    input_tokens: Option<u64>,
    output_tokens: Option<u64>,
    cache_read_tokens: Option<u64>,
    cache_write_tokens: Option<u64>,
    reasoning_tokens: Option<u64>,
    reported_cost_micro_usd: Option<MicroUsd>,
}

fn validate_provider_effect_start(
    request: &factory_protocol::SessionProviderRequestStartRequest,
) -> Result<(), StoreError> {
    for (field, value, maximum) in [
        ("provider effect core run ID", request.core_run_id.as_str(), 60),
        (
            "provider effect harness snapshot ID",
            request.harness_snapshot_id.as_str(),
            240,
        ),
        (
            "provider effect harness revision ID",
            request.harness_revision_id.as_str(),
            240,
        ),
        (
            "provider effect model harness profile ID",
            request.model_harness_profile_id.as_str(),
            240,
        ),
        ("provider effect provider", request.provider.as_str(), 160),
        ("provider effect model", request.model.as_str(), 240),
    ] {
        if value.is_empty() || value.len() > maximum || value.contains('\0') {
            return Err(StoreError::InvalidProcessCommand { field });
        }
    }
    Ok(())
}

fn validate_provider_effect_settlement_identity(
    request: &factory_protocol::SessionProviderRequestSettleRequest,
) -> Result<(), StoreError> {
    if request.core_run_id.is_empty()
        || request.core_run_id.len() > 60
        || request.core_run_id.contains('\0')
    {
        return Err(StoreError::InvalidProcessCommand {
            field: "provider effect core run ID",
        });
    }
    Ok(())
}

fn require_provider_effect_command(
    phase: &str,
    session_id: SessionId,
    assignment_id: AssignmentId,
    command_id: &str,
    core_run_id: &str,
    effect_id: u64,
) -> Result<(), StoreError> {
    let expected = format!(
        "provider-effect-{phase}-session-{}-assignment-{}-run-{core_run_id}-effect-{effect_id}",
        session_id.get(),
        assignment_id.get(),
    );
    if command_id != expected {
        return Err(StoreError::InvalidProcessCommand {
            field: "provider effect client command ID",
        });
    }
    validate_command("actor", command_id)
}

fn provider_effect_settlement(
    request: &factory_protocol::SessionProviderRequestSettleRequest,
    state: ProviderEffectState,
) -> Result<ProviderEffectSettlement, StoreError> {
    let stop_reason = match request.stop_reason.as_str() {
        "stop" | "tool_use" | "length" | "aborted" | "cancelled" | "error" => {
            request.stop_reason.clone()
        }
        _ => {
            return Err(StoreError::InvalidProcessCommand {
                field: "provider effect stop reason",
            });
        }
    };
    if state == ProviderEffectState::Settled && request.failure_class.is_some() {
        return Err(StoreError::InvalidProcessCommand {
            field: "provider effect failure class",
        });
    }
    let failure_class = match request.failure_class.as_deref() {
        None => None,
        Some("provider_response_error" | "provider_transport_error") => request.failure_class.clone(),
        Some(_) => {
            return Err(StoreError::InvalidProcessCommand {
                field: "provider effect failure class",
            });
        }
    };
    let convert = |value: Option<u64>| {
        value
            .map(i64::try_from)
            .transpose()
            .map_err(|_| StoreError::InvalidProcessCommand {
                field: "provider effect usage",
            })
    };
    Ok(ProviderEffectSettlement {
        stop_reason,
        context_overflow: request.context_overflow,
        input_tokens: convert(request.input_tokens)?,
        output_tokens: convert(request.output_tokens)?,
        cache_read_tokens: convert(request.cache_read_tokens)?,
        cache_write_tokens: convert(request.cache_write_tokens)?,
        reasoning_tokens: convert(request.reasoning_tokens)?,
        reported_cost_micro_usd: convert(request.reported_cost_micro_usd)?,
        failure_class,
    })
}

fn provider_effect_session_matches(
    row: &sqlx::postgres::PgRow,
    session_id: SessionId,
    assignment_id: AssignmentId,
    expected_revision: u64,
    provider: &str,
    model: &str,
) -> Result<(), StoreError> {
    provider_effect_session_matches_settlement(row, session_id, assignment_id, expected_revision)?;
    if row.try_get::<String, _>("model_provider")? != provider
        || row.try_get::<String, _>("model_id")? != model
    {
        return Err(StoreError::PacketIdentityMismatch);
    }
    Ok(())
}

fn provider_effect_session_matches_settlement(
    row: &sqlx::postgres::PgRow,
    session_id: SessionId,
    assignment_id: AssignmentId,
    expected_revision: u64,
) -> Result<(), StoreError> {
    if row.try_get::<i64, _>("assignment_id")? != assignment_id.get() {
        return Err(StoreError::PacketIdentityMismatch);
    }
    let current = storage::aggregate_revision_from_sql_for_process(row.try_get("revision")?)?;
    if current.get() != expected_revision {
        return Err(StoreError::RevisionConflict {
            expected: ExpectedRevision::new(AggregateRevision::from_persisted(expected_revision)),
            current,
        });
    }
    if row.try_get::<i16, _>("lifecycle")? != SESSION_RUNNING {
        return Err(StoreError::SessionAlreadyTerminal {
            session_id,
        });
    }
    Ok(())
}

fn provider_effect_state_from_code(value: i16) -> Result<ProviderEffectState, StoreError> {
    match value {
        0 => Ok(ProviderEffectState::Started),
        1 => Ok(ProviderEffectState::Settled),
        2 => Ok(ProviderEffectState::Failed),
        3 => Ok(ProviderEffectState::Cancelled),
        _ => Err(StoreError::CorruptLifecycleColumn),
    }
}

fn provider_effect_settlement_matches(
    row: &sqlx::postgres::PgRow,
    settlement: &ProviderEffectSettlement,
    state: ProviderEffectState,
) -> Result<bool, StoreError> {
    Ok(provider_effect_state_from_code(row.try_get("state")?)? == state
        && row.try_get::<Option<String>, _>("stop_reason")? == Some(settlement.stop_reason.clone())
        && row.try_get::<Option<bool>, _>("context_overflow")? == Some(settlement.context_overflow)
        && row.try_get::<Option<i64>, _>("input_tokens")? == settlement.input_tokens
        && row.try_get::<Option<i64>, _>("output_tokens")? == settlement.output_tokens
        && row.try_get::<Option<i64>, _>("cache_read_tokens")? == settlement.cache_read_tokens
        && row.try_get::<Option<i64>, _>("cache_write_tokens")? == settlement.cache_write_tokens
        && row.try_get::<Option<i64>, _>("reasoning_tokens")? == settlement.reasoning_tokens
        && row.try_get::<Option<i64>, _>("reported_cost_micro_usd")?
            == settlement.reported_cost_micro_usd
        && row.try_get::<Option<String>, _>("failure_class")? == settlement.failure_class)
}

fn provider_effect_ledger_row(
    row: &sqlx::postgres::PgRow,
) -> Result<ProviderEffectLedgerRow, StoreError> {
    let value = |field| {
        row.try_get::<Option<i64>, _>(field)?
            .map(u64::try_from)
            .transpose()
            .map_err(|_| StoreError::CorruptCostColumn)
    };
    Ok(ProviderEffectLedgerRow {
        state: provider_effect_state_from_code(row.try_get("state")?)?,
        input_tokens: value("input_tokens")?,
        output_tokens: value("output_tokens")?,
        cache_read_tokens: value("cache_read_tokens")?,
        cache_write_tokens: value("cache_write_tokens")?,
        reasoning_tokens: value("reasoning_tokens")?,
        reported_cost_micro_usd: value("reported_cost_micro_usd")?.map(MicroUsd::new),
    })
}

fn provider_effect_ledger_from_rows(
    rows: &[ProviderEffectLedgerRow],
) -> Result<ProviderEffectLedger, StoreError> {
    let complete_settlement = rows
        .iter()
        .all(|row| row.state == ProviderEffectState::Settled);
    let sum = |values: Vec<Option<u64>>| -> Result<Option<u64>, StoreError> {
        values.into_iter().try_fold(Some(0_u64), |total, value| match (total, value) {
            (Some(total), Some(value)) => total.checked_add(value).map(Some).ok_or(
                StoreError::InvalidProcessCommand {
                    field: "provider effect usage aggregate",
                },
            ),
            _ => Ok(None),
        })
    };
    let input_tokens = sum(rows.iter().map(|row| row.input_tokens).collect())?;
    let output_tokens = sum(rows.iter().map(|row| row.output_tokens).collect())?;
    let cache_read_tokens = sum(rows.iter().map(|row| row.cache_read_tokens).collect())?;
    let cache_write_tokens = sum(rows.iter().map(|row| row.cache_write_tokens).collect())?;
    let reasoning_tokens = sum(rows.iter().map(|row| row.reasoning_tokens).collect())?;
    let reported_cost_micro_usd = sum(
        rows.iter()
            .map(|row| row.reported_cost_micro_usd.map(MicroUsd::get))
            .collect(),
    )?;
    let complete_usage = complete_settlement
        && input_tokens.is_some()
        && output_tokens.is_some()
        && cache_read_tokens.is_some()
        && cache_write_tokens.is_some()
        && reasoning_tokens.is_some();
    let complete_cost = complete_settlement && reported_cost_micro_usd.is_some();
    Ok(ProviderEffectLedger {
        provider_effect_count: u32::try_from(rows.len()).map_err(|_| StoreError::InvalidProcessCommand {
            field: "provider effect count",
        })?,
        settled_provider_effect_count: u32::try_from(
            rows.iter()
                .filter(|row| row.state != ProviderEffectState::Started)
                .count(),
        )
        .map_err(|_| StoreError::InvalidProcessCommand {
            field: "settled provider effect count",
        })?,
        complete_usage,
        complete_cost,
        usage: UsageTotalsV2 {
            input_tokens: complete_settlement.then_some(input_tokens).flatten(),
            output_tokens: complete_settlement.then_some(output_tokens).flatten(),
            cache_read_tokens: complete_settlement.then_some(cache_read_tokens).flatten(),
            cache_write_tokens: complete_settlement.then_some(cache_write_tokens).flatten(),
            reasoning_tokens: complete_settlement.then_some(reasoning_tokens).flatten(),
            reported_cost_micro_usd: complete_settlement
                .then_some(reported_cost_micro_usd)
                .flatten()
                .map(MicroUsd::new),
        },
    })
}
fn assignment_role_code(assignment_role: factory_protocol::AssignmentRole) -> i16 {
    match assignment_role {
        factory_protocol::AssignmentRole::ProductResearch => 0,
        factory_protocol::AssignmentRole::Engineering => 1,
        factory_protocol::AssignmentRole::Quality => 2,
    }
}

fn assignment_role_from_code(value: i16) -> Result<AssignmentRole, StoreError> {
    match value {
        0 => Ok(AssignmentRole::ProductResearch),
        1 => Ok(AssignmentRole::Engineering),
        2 => Ok(AssignmentRole::Quality),
        _ => Err(StoreError::CorruptLifecycleColumn),
    }
}

fn assignment_role_name(assignment_role: factory_protocol::AssignmentRole) -> &'static str {
    match assignment_role {
        factory_protocol::AssignmentRole::ProductResearch => "product_research",
        factory_protocol::AssignmentRole::Engineering => "engineering",
        factory_protocol::AssignmentRole::Quality => "quality",
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

fn terminal_operation_name(value: TerminalOperationV2) -> &'static str {
    match value {
        TerminalOperationV2::WorkComplete => "work_complete",
        TerminalOperationV2::CandidateSubmit => "candidate_submit",
        TerminalOperationV2::QualitySubmitReview => "quality_submit_review",
    }
}
fn thinking_code(value: factory_protocol::ThinkingLevelV2) -> i16 {
    match value {
        factory_protocol::ThinkingLevelV2::None => 0,
        factory_protocol::ThinkingLevelV2::Low => 1,
        factory_protocol::ThinkingLevelV2::Medium => 2,
        factory_protocol::ThinkingLevelV2::High => 3,
        factory_protocol::ThinkingLevelV2::XHigh => 4,
    }
}
fn operation_mask(ops: &[TerminalOperationV2]) -> i64 {
    ops.iter()
        .fold(0, |mask, op| mask | operation_mask_one(Some(*op)))
}
fn operation_mask_one(operation: Option<TerminalOperationV2>) -> i64 {
    operation.map_or(0, |op| 1_i64 << terminal_operation_code(op))
}
fn session_state(reason: StopReasonV2) -> SessionState {
    match reason {
        StopReasonV2::Completed => SessionState::Succeeded,
        StopReasonV2::Cancelled => SessionState::Cancelled,
        StopReasonV2::CostLimit => SessionState::Failed,
        StopReasonV2::DaemonDisconnected | StopReasonV2::Deadline => SessionState::Interrupted,
        _ => SessionState::Failed,
    }
}
fn assignment_state_code(state: SessionState) -> i16 {
    match state {
        SessionState::Succeeded => 2,
        SessionState::Cancelled => 4,
        SessionState::Interrupted => 5,
        _ => 3,
    }
}
fn session_state_code(state: SessionState) -> i16 {
    match state {
        SessionState::Prepared => 0,
        SessionState::Running => 1,
        SessionState::Succeeded => 2,
        SessionState::Failed => 3,
        SessionState::Cancelled => 4,
        SessionState::Interrupted => 5,
    }
}
fn session_state_from_code(value: i16) -> Result<SessionState, StoreError> {
    match value {
        0 => Ok(SessionState::Prepared),
        1 => Ok(SessionState::Running),
        2 => Ok(SessionState::Succeeded),
        3 => Ok(SessionState::Failed),
        4 => Ok(SessionState::Cancelled),
        5 => Ok(SessionState::Interrupted),
        _ => Err(StoreError::CorruptLifecycleColumn),
    }
}
fn campaign_state_from_code(value: i16) -> Result<factory_protocol::CampaignState, StoreError> {
    match value {
        0 => Ok(factory_protocol::CampaignState::Running),
        1 => Ok(factory_protocol::CampaignState::Completed),
        2 => Ok(factory_protocol::CampaignState::Failed),
        3 => Ok(factory_protocol::CampaignState::Cancelled),
        _ => Err(StoreError::CorruptLifecycleColumn),
    }
}
fn stop_reason_code(value: StopReasonV2) -> i16 {
    value as i16
}
fn terminal_operation_code(value: TerminalOperationV2) -> i16 {
    match value {
        TerminalOperationV2::WorkComplete => 0,
        TerminalOperationV2::CandidateSubmit => 1,
        TerminalOperationV2::QualitySubmitReview => 2,
    }
}
fn failure_code(value: StopReasonV2) -> Option<i16> {
    (value != StopReasonV2::Completed).then_some(value as i16)
}
fn usage_sql(
    value: UsageTotalsV2,
) -> Result<
    (
        Option<i64>,
        Option<i64>,
        Option<i64>,
        Option<i64>,
        Option<i64>,
        Option<i64>,
    ),
    StoreError,
> {
    let range = |_| StoreError::InvalidProcessCommand {
        field: "usage total",
    };
    Ok((
        value.input_tokens.map(i64::try_from).transpose().map_err(range)?,
        value.output_tokens.map(i64::try_from).transpose().map_err(range)?,
        value
            .cache_read_tokens
            .map(i64::try_from)
            .transpose()
            .map_err(range)?,
        value
            .cache_write_tokens
            .map(i64::try_from)
            .transpose()
            .map_err(range)?,
        value
            .reasoning_tokens
            .map(i64::try_from)
            .transpose()
            .map_err(range)?,
        value
            .reported_cost_micro_usd
            .map(|amount| i64::try_from(amount.get()))
            .transpose()
            .map_err(range)?,
    ))
}

fn hash_optional_u64(hasher: &mut blake3::Hasher, value: Option<u64>) {
    match value {
        Some(value) => {
            hash_u32(hasher, 1);
            hash_u64(hasher, value);
        }
        None => hash_u32(hasher, 0),
    }
}

fn hash_optional_artifact_id(hasher: &mut blake3::Hasher, value: Option<ArtifactId>) {
    match value {
        Some(value) => {
            hash_u32(hasher, 1);
            hash_i64(hasher, value.get());
        }
        None => hash_u32(hasher, 0),
    }
}
fn db_cost(state: Option<i16>, value: Option<i64>) -> Result<Option<TerminalCostV2>, StoreError> {
    match (state, value) {
        (None, None) => Ok(None),
        (Some(COST_KNOWN), Some(v)) => Ok(Some(TerminalCostV2::Known(MicroUsd::new(
            u64::try_from(v).map_err(|_| StoreError::CorruptCostColumn)?,
        )))),
        (Some(COST_UNKNOWN), None) => Ok(Some(TerminalCostV2::Unknown)),
        (Some(COST_EXCEEDED), Some(v)) => Ok(Some(TerminalCostV2::Exceeded(MicroUsd::new(
            u64::try_from(v).map_err(|_| StoreError::CorruptCostColumn)?,
        )))),
        _ => Err(StoreError::CorruptCostColumn),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn terminal_state_mapping_keeps_infrastructure_stops_interrupted() {
        assert_eq!(
            session_state(StopReasonV2::Completed),
            SessionState::Succeeded
        );
        assert_eq!(
            session_state(StopReasonV2::DaemonDisconnected),
            SessionState::Interrupted
        );
        assert_eq!(
            session_state(StopReasonV2::UnknownCost),
            SessionState::Failed
        );
        assert_eq!(session_state(StopReasonV2::CostLimit), SessionState::Failed);
    }

    #[test]
    fn terminal_operation_mask_is_closed_and_bounded() {
        let mask = operation_mask(&[
            TerminalOperationV2::WorkComplete,
            TerminalOperationV2::QualitySubmitReview,
        ]);
        assert_eq!(mask, 1 | (1 << 2));
        assert_eq!(operation_mask_one(None), 0);
    }

    #[test]
    fn provider_cost_is_unknown_when_absent_even_with_token_usage() {
        let usage = UsageTotalsV2 {
            input_tokens: Some(100),
            output_tokens: Some(100),
            reported_cost_micro_usd: None,
            ..UsageTotalsV2::default()
        };
        assert!(usage.provider_cost().is_err());
    }

    fn provider_row(
        state: ProviderEffectState,
        tokens: Option<u64>,
        cost: Option<u64>,
    ) -> ProviderEffectLedgerRow {
        ProviderEffectLedgerRow {
            state,
            input_tokens: tokens,
            output_tokens: tokens,
            cache_read_tokens: tokens,
            cache_write_tokens: tokens,
            reasoning_tokens: tokens,
            reported_cost_micro_usd: cost.map(MicroUsd::new),
        }
    }

    #[test]
    fn provider_effect_ledger_recovers_multiple_complete_turns() {
        let ledger = provider_effect_ledger_from_rows(&[
            provider_row(ProviderEffectState::Settled, Some(2), Some(3)),
            provider_row(ProviderEffectState::Settled, Some(5), Some(7)),
        ])
        .expect("complete provider ledger");
        assert_eq!(ledger.provider_effect_count, 2);
        assert_eq!(ledger.settled_provider_effect_count, 2);
        assert!(ledger.complete_usage);
        assert!(ledger.complete_cost);
        assert_eq!(ledger.usage.input_tokens, Some(7));
        assert_eq!(ledger.usage.cache_read_tokens, Some(7));
        assert_eq!(ledger.usage.reported_cost_micro_usd, Some(MicroUsd::new(10)));
    }

    #[test]
    fn provider_effect_ledger_distinguishes_known_zero_from_unknown() {
        let zero = provider_effect_ledger_from_rows(&[provider_row(
            ProviderEffectState::Settled,
            Some(0),
            Some(0),
        )])
        .expect("known zero provider ledger");
        assert!(zero.complete_cost);
        assert_eq!(zero.usage.reported_cost_micro_usd, Some(MicroUsd::new(0)));

        let unknown = provider_effect_ledger_from_rows(&[provider_row(
            ProviderEffectState::Settled,
            None,
            None,
        )])
        .expect("unknown provider ledger remains representable");
        assert!(!unknown.complete_usage);
        assert!(!unknown.complete_cost);
        assert_eq!(unknown.usage.input_tokens, None);
        assert_eq!(unknown.usage.reported_cost_micro_usd, None);
    }

    #[test]
    fn started_provider_effect_keeps_terminal_cost_incomplete() {
        let ledger = provider_effect_ledger_from_rows(&[provider_row(
            ProviderEffectState::Started,
            Some(1),
            Some(1),
        )])
        .expect("started effect ledger");
        assert_eq!(ledger.settled_provider_effect_count, 0);
        assert!(!ledger.complete_usage);
        assert!(!ledger.complete_cost);
        assert_eq!(ledger.usage.reported_cost_micro_usd, None);
    }
}
