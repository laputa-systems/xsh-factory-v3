//! One bounded resident-daemon step from durable scheduler state to custody.
//!
//! [`CampaignDriver`] is deliberately a direct composition, not a workflow
//! engine or a second scheduler. Each call rereads the one running campaign,
//! asks [`TicketScheduler`] for its already-deterministic next action, and
//! performs exactly that action through the existing typed authorities. It
//! retains no polling cursor, session handle, or retry queue: PostgreSQL and
//! the process/session transitions remain the recovery boundary.

use std::{env, ffi::OsString, sync::Arc};

use factory_protocol::{AggregateRevision, CampaignId, ExpectedRevision};
use thiserror::Error;

use crate::{
    assignment_runtime::{
        AssignmentLaunchOutcome, AssignmentMaterializationRequest, AssignmentRuntimeError,
        materialize_and_launch_assignment,
    },
    cas::CasStore,
    durable_authority::{
        DeliverAcceptedCandidate, DurableAssignmentTarget, DurableAuthorityResolver,
    },
    installed_runtime::{InstalledKernelBuildReceiptV1, InstalledKernelExecutionTools},
    local_transport::LocalDaemon,
    scheduler::{
        ClaimReadyTicketAction, CompleteCampaignAction, SchedulerConstraint, SchedulerNextAction,
        TicketScheduler,
    },
    storage::{KernelStore, StoreError},
    ticket_store::{
        ClaimOutcome, ClaimTicketReceipt, DownstreamActionContext, DownstreamActionStage,
        FailTicketAttempt,
    },
};

const DRIVER_PRINCIPAL: &str = "factoryd-campaign-driver";

/// The result of one durable driver pass. Non-actionable outcomes are values,
/// not errors, so a resident daemon can wait without manufacturing a polling
/// row or treating an Architect gate as infrastructure failure.
pub enum CampaignDriverOutcome {
    NoRunningCampaign,
    Assignment(AssignmentLaunchOutcome),
    CampaignFailed {
        campaign_id: CampaignId,
        /// Bounded durable fault detail. This mirrors the reason recorded by
        /// the transition so callers can distinguish a rejected packet from a
        /// completed-but-unsuccessful actor without opening SQL.
        failure_detail: String,
    },
    TicketAttemptFailed {
        campaign_id: CampaignId,
        ticket_attempt_id: factory_protocol::TicketAttemptId,
        /// Bounded durable fault detail recorded on the exact attempt.
        failure_detail: String,
    },
    ClaimSettled(ClaimTicketReceipt),
    HardValidationResumed,
    CandidateCommitAttached,
    Delivered,
    CampaignCompleted {
        campaign_id: CampaignId,
        revision: AggregateRevision,
    },
    AwaitingArchitect {
        campaign_id: CampaignId,
        proposed_count: u32,
    },
    Idle {
        campaign_id: CampaignId,
    },
    Blocked(SchedulerConstraint),
}

#[derive(Debug, Error)]
pub enum CampaignDriverError {
    #[error(transparent)]
    Store(#[from] StoreError),

    #[error(transparent)]
    Assignment(#[from] AssignmentRuntimeError),

    #[error("durable campaign action was rejected: {0}")]
    Durable(String),

    #[error("configured OpenRouter credential environment {name:?} is absent or empty")]
    CredentialUnavailable { name: String },
}

/// Exact installed services retained by the resident daemon. The driver has
/// no database pool, application callback, or actor-provided selector; every
/// identity is reread from the scheduler/authority at the transition that
/// consumes it.
#[derive(Clone)]
pub struct CampaignDriver {
    store: KernelStore,
    cas: CasStore,
    installed: InstalledKernelBuildReceiptV1,
    execution: InstalledKernelExecutionTools,
    resolver: Arc<DurableAuthorityResolver>,
    credential_lookup: Arc<dyn Fn(&str) -> Option<OsString> + Send + Sync>,
}

impl CampaignDriver {
    #[must_use]
    pub fn new(
        store: KernelStore,
        cas: CasStore,
        installed: InstalledKernelBuildReceiptV1,
        execution: InstalledKernelExecutionTools,
        resolver: Arc<DurableAuthorityResolver>,
    ) -> Self {
        Self::with_credential_lookup(store, cas, installed, execution, resolver, |name| {
            env::var_os(name)
        })
    }

    /// Production reads only the admitted environment name at the launch
    /// boundary. Provider-free callers inject an inert value instead of
    /// mutating the process-global environment.
    pub fn with_credential_lookup<F>(
        store: KernelStore,
        cas: CasStore,
        installed: InstalledKernelBuildReceiptV1,
        execution: InstalledKernelExecutionTools,
        resolver: Arc<DurableAuthorityResolver>,
        credential_lookup: F,
    ) -> Self
    where
        F: Fn(&str) -> Option<OsString> + Send + Sync + 'static,
    {
        Self {
            store,
            cas,
            installed,
            execution,
            resolver,
            credential_lookup: Arc::new(credential_lookup),
        }
    }

    /// Reads and executes at most one current campaign action. A caller may
    /// invoke this after an operator starts a campaign or periodically while
    /// waiting; the call itself never persists poll state.
    pub async fn run_next(
        &self,
        daemon: &LocalDaemon,
    ) -> Result<CampaignDriverOutcome, CampaignDriverError> {
        let process = self.store.process_store();
        let Some(campaign_id) = process.current_running_campaign_id().await? else {
            return Ok(CampaignDriverOutcome::NoRunningCampaign);
        };
        let campaign = process.campaign_status(campaign_id).await?;
        let scheduler = self.store.ticket_scheduler();
        let action = scheduler.next_action(campaign_id).await?;
        self.execute(
            daemon,
            &scheduler,
            campaign_id,
            campaign.application_revision_id,
            action,
        )
        .await
    }

    async fn execute(
        &self,
        daemon: &LocalDaemon,
        scheduler: &TicketScheduler,
        campaign_id: CampaignId,
        application_revision_id: factory_protocol::ApplicationRevisionId,
        action: SchedulerNextAction,
    ) -> Result<CampaignDriverOutcome, CampaignDriverError> {
        match action {
            SchedulerNextAction::CompleteCampaign(action) => {
                let revision = self.complete(scheduler, action).await?;
                Ok(CampaignDriverOutcome::CampaignCompleted {
                    campaign_id: action.campaign_id,
                    revision,
                })
            }
            SchedulerNextAction::ReplenishProduct {
                campaign_id,
                expected_campaign_revision,
            } => {
                self.launch(
                    daemon,
                    campaign_id,
                    application_revision_id,
                    expected_campaign_revision,
                    DurableAssignmentTarget::Product,
                    "product",
                )
                .await
            }
            SchedulerNextAction::ClaimReadyTicket(action) => {
                self.claim_then_launch(daemon, scheduler, application_revision_id, action)
                    .await
            }
            SchedulerNextAction::ContinueDownstream(context) => {
                self.continue_downstream(daemon, campaign_id, application_revision_id, context)
                    .await
            }
            SchedulerNextAction::AwaitArchitectDecision {
                campaign_id,
                proposed_count,
            } => Ok(CampaignDriverOutcome::AwaitingArchitect {
                campaign_id,
                proposed_count,
            }),
            SchedulerNextAction::Idle { campaign_id } => {
                Ok(CampaignDriverOutcome::Idle { campaign_id })
            }
            SchedulerNextAction::Blocked(constraint) => {
                Ok(CampaignDriverOutcome::Blocked(constraint))
            }
        }
    }

    async fn complete(
        &self,
        scheduler: &TicketScheduler,
        action: CompleteCampaignAction,
    ) -> Result<AggregateRevision, CampaignDriverError> {
        Ok(scheduler
            .complete_campaign(
                DRIVER_PRINCIPAL,
                &command_id(
                    "complete",
                    action.campaign_id,
                    action.expected_campaign_revision,
                ),
                action,
            )
            .await?)
    }

    async fn claim_then_launch(
        &self,
        daemon: &LocalDaemon,
        scheduler: &TicketScheduler,
        application_revision_id: factory_protocol::ApplicationRevisionId,
        action: ClaimReadyTicketAction,
    ) -> Result<CampaignDriverOutcome, CampaignDriverError> {
        let requalification = self
            .resolver
            .requalify_sponsored_ticket(action)
            .await
            .map_err(CampaignDriverError::Durable)?;
        let claim = scheduler
            .claim_ready_ticket(
                DRIVER_PRINCIPAL,
                &command_id(
                    "claim",
                    action.campaign_id,
                    action.expected_campaign_revision,
                ),
                action,
                requalification,
            )
            .await?;
        match claim.outcome {
            ClaimOutcome::Claimed { ticket_attempt_id } => {
                // Claiming changes ticket state but not campaign revision, so
                // the scheduler's campaign fence remains the exact packet
                // construction fence. `create_assignment` rechecks it.
                self.launch(
                    daemon,
                    action.campaign_id,
                    application_revision_id,
                    action.expected_campaign_revision,
                    DurableAssignmentTarget::Engineering { ticket_attempt_id },
                    "engineering",
                )
                .await
            }
            ClaimOutcome::Resolved | ClaimOutcome::Blocked => {
                Ok(CampaignDriverOutcome::ClaimSettled(claim))
            }
        }
    }

    async fn continue_downstream(
        &self,
        daemon: &LocalDaemon,
        campaign_id: CampaignId,
        application_revision_id: factory_protocol::ApplicationRevisionId,
        context: DownstreamActionContext,
    ) -> Result<CampaignDriverOutcome, CampaignDriverError> {
        match context.stage {
            DownstreamActionStage::HardValidation | DownstreamActionStage::ReworkValidation => {
                self.resolver
                    .resume_hard_validation(context)
                    .await
                    .map_err(CampaignDriverError::Durable)?;
                Ok(CampaignDriverOutcome::HardValidationResumed)
            }
            DownstreamActionStage::CandidateCommitAttachRequired => {
                self.resolver
                    .resume_candidate_commit_attach(context)
                    .await
                    .map_err(CampaignDriverError::Durable)?;
                Ok(CampaignDriverOutcome::CandidateCommitAttached)
            }
            DownstreamActionStage::Quality
            | DownstreamActionStage::QualityReviewRequired
            | DownstreamActionStage::ReworkQuality => {
                let campaign = self
                    .store
                    .process_store()
                    .campaign_status(campaign_id)
                    .await?;
                self.launch(
                    daemon,
                    campaign.campaign_id,
                    application_revision_id,
                    ExpectedRevision::new(campaign.revision),
                    DurableAssignmentTarget::Quality {
                        ticket_attempt_id: context.ticket_attempt_id,
                        candidate_id: context.candidate_id,
                    },
                    "quality",
                )
                .await
            }
            DownstreamActionStage::ReworkEngineering => {
                let campaign = self
                    .store
                    .process_store()
                    .campaign_status(campaign_id)
                    .await?;
                self.launch(
                    daemon,
                    campaign.campaign_id,
                    application_revision_id,
                    ExpectedRevision::new(campaign.revision),
                    DurableAssignmentTarget::Engineering {
                        ticket_attempt_id: context.ticket_attempt_id,
                    },
                    "rework-engineering",
                )
                .await
            }
            DownstreamActionStage::AwaitingArchitect => {
                Ok(CampaignDriverOutcome::AwaitingArchitect {
                    campaign_id,
                    proposed_count: 0,
                })
            }
            DownstreamActionStage::DeliverAccepted => {
                self.resolver
                    .deliver_accepted_candidate(DeliverAcceptedCandidate {
                        principal: DRIVER_PRINCIPAL.to_owned(),
                        command_id: downstream_command_id("deliver", context),
                        candidate_id: context.candidate_id,
                    })
                    .await
                    .map_err(CampaignDriverError::Durable)?;
                Ok(CampaignDriverOutcome::Delivered)
            }
        }
    }

    async fn launch(
        &self,
        daemon: &LocalDaemon,
        campaign_id: CampaignId,
        application_revision_id: factory_protocol::ApplicationRevisionId,
        expected_campaign_revision: factory_protocol::ExpectedRevision,
        target: DurableAssignmentTarget,
        action: &'static str,
    ) -> Result<CampaignDriverOutcome, CampaignDriverError> {
        let credential_environment_value = match credential_environment_value(
            self.installed.openrouter_credential_environment(),
            &self.credential_lookup,
        ) {
            Ok(value) => value,
            Err(error) => {
                let reason = failure_reason(action, &error.to_string());
                return self
                    .terminalize_failed_launch(campaign_id, target, &reason)
                    .await;
            }
        };
        let assignment = materialize_and_launch_assignment(
            &self.store,
            &self.cas,
            daemon,
            &self.installed,
            &self.execution,
            Arc::clone(&self.resolver),
            AssignmentMaterializationRequest {
                principal: DRIVER_PRINCIPAL.to_owned(),
                command_id: command_id(action, campaign_id, expected_campaign_revision),
                expected_campaign_revision,
                campaign_id,
                application_revision_id,
                target,
                credential_environment_value,
            },
        )
        .await;
        match assignment {
            Ok(assignment)
                if assignment.session.terminal.session_state
                    == factory_protocol::SessionState::Succeeded =>
            {
                Ok(CampaignDriverOutcome::Assignment(assignment))
            }
            Ok(_) => {
                let reason =
                    failure_reason(action, "session reached a non-succeeded terminal state");
                self.terminalize_failed_launch(campaign_id, target, &reason)
                    .await
            }
            Err(error) => {
                let reason = failure_reason(action, &error.to_string());
                self.terminalize_failed_launch(campaign_id, target, &reason)
                    .await
            }
        }
    }

    async fn terminalize_failed_launch(
        &self,
        campaign_id: CampaignId,
        target: DurableAssignmentTarget,
        reason: &str,
    ) -> Result<CampaignDriverOutcome, CampaignDriverError> {
        match target {
            DurableAssignmentTarget::Product => {
                self.fail_running_campaign(campaign_id, reason).await?;
                Ok(CampaignDriverOutcome::CampaignFailed {
                    campaign_id,
                    failure_detail: reason.to_owned(),
                })
            }
            DurableAssignmentTarget::Engineering { ticket_attempt_id }
            | DurableAssignmentTarget::Quality {
                ticket_attempt_id, ..
            } => {
                self.fail_ticket_attempt(ticket_attempt_id, reason).await?;
                Ok(CampaignDriverOutcome::TicketAttemptFailed {
                    campaign_id,
                    ticket_attempt_id,
                    failure_detail: reason.to_owned(),
                })
            }
        }
    }

    async fn fail_running_campaign(
        &self,
        campaign_id: CampaignId,
        reason: &str,
    ) -> Result<(), CampaignDriverError> {
        let process = self.store.process_store();
        let campaign = process.campaign_status(campaign_id).await?;
        if campaign.state != factory_protocol::CampaignState::Running {
            return Ok(());
        }
        process
            .fail_campaign(&crate::process::FailCampaign {
                principal: DRIVER_PRINCIPAL.to_owned(),
                command_id: command_id(
                    "fault",
                    campaign_id,
                    ExpectedRevision::new(campaign.revision),
                ),
                expected_revision: ExpectedRevision::new(campaign.revision),
                campaign_id,
                reason: reason.to_owned(),
            })
            .await?;
        Ok(())
    }

    async fn fail_ticket_attempt(
        &self,
        ticket_attempt_id: factory_protocol::TicketAttemptId,
        reason: &str,
    ) -> Result<(), CampaignDriverError> {
        let ticket = self.store.ticket_store();
        let context = ticket.failure_context(ticket_attempt_id).await?;
        ticket
            .fail_ticket_attempt(&FailTicketAttempt {
                principal: DRIVER_PRINCIPAL.to_owned(),
                command_id: format!(
                    "attempt-{}-fault-ar{}-tr{}",
                    ticket_attempt_id.get(),
                    context.attempt_revision.get(),
                    context.ticket_revision.get(),
                ),
                ticket_attempt_id,
                expected_attempt_revision: ExpectedRevision::new(context.attempt_revision),
                expected_ticket_revision: ExpectedRevision::new(context.ticket_revision),
                reason: reason.to_owned(),
            })
            .await?;
        Ok(())
    }
}

fn credential_environment_value(
    name: &str,
    lookup: &Arc<dyn Fn(&str) -> Option<OsString> + Send + Sync>,
) -> Result<OsString, CampaignDriverError> {
    lookup(name)
        .filter(|value| !value.is_empty())
        .ok_or_else(|| CampaignDriverError::CredentialUnavailable {
            name: name.to_owned(),
        })
}

fn failure_reason(action: &str, detail: &str) -> String {
    let mut reason = format!("daemon {action} assignment fault: {detail}");
    if reason.len() > 240 {
        reason.truncate(240);
    }
    reason
}

fn command_id(
    action: &str,
    campaign_id: CampaignId,
    revision: factory_protocol::ExpectedRevision,
) -> String {
    format!(
        "campaign-{}-{}-r{}",
        campaign_id.get(),
        action,
        revision.get().get()
    )
}

fn downstream_command_id(action: &str, context: DownstreamActionContext) -> String {
    format!(
        "campaign-{}-{}-attempt-{}-candidate-{}-ar{}-cr{}",
        action,
        context.stage.name(),
        context.ticket_attempt_id.get(),
        context.candidate_id.get(),
        context.ticket_attempt_revision.get(),
        context.candidate_revision.get(),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use factory_protocol::{AggregateRevision, CandidateId, ExpectedRevision, TicketAttemptId};

    fn downstream(stage: DownstreamActionStage) -> DownstreamActionContext {
        DownstreamActionContext {
            stage,
            ticket_attempt_id: TicketAttemptId::new(7).expect("positive attempt"),
            ticket_attempt_revision: AggregateRevision::from_persisted(3),
            ticket_revision: AggregateRevision::from_persisted(2),
            candidate_id: CandidateId::new(11).expect("positive candidate"),
            candidate_revision: AggregateRevision::from_persisted(5),
        }
    }

    #[test]
    fn downstream_idempotency_keys_bind_every_actionable_scheduler_revision() {
        let first = downstream_command_id(
            "deliver",
            downstream(DownstreamActionStage::DeliverAccepted),
        );
        let changed_candidate = downstream_command_id(
            "deliver",
            DownstreamActionContext {
                candidate_revision: AggregateRevision::from_persisted(6),
                ..downstream(DownstreamActionStage::DeliverAccepted)
            },
        );
        assert_ne!(first, changed_candidate);
        assert!(first.contains("deliver_accepted"));
    }

    #[test]
    fn campaign_action_idempotency_key_binds_its_optimistic_campaign_revision() {
        let campaign = CampaignId::new(19).expect("positive campaign");
        assert_ne!(
            command_id(
                "product",
                campaign,
                ExpectedRevision::new(AggregateRevision::initial())
            ),
            command_id(
                "product",
                campaign,
                ExpectedRevision::new(AggregateRevision::from_persisted(1)),
            ),
        );
    }
}
