//! Deterministic ticket-buffer composition.
//!
//! This module turns a bounded, read-only [`TicketBufferStatus`] into the one
//! next daemon action. It does not launch an actor or persist polling state.
//! A returned engineering action carries both aggregate revisions, so an old
//! read can only become a typed, fenced claim through [`TicketStore`].

use factory_protocol::{AggregateRevision, CampaignId, ExpectedRevision};

use crate::{
    storage::{KernelStore, StoreError},
    ticket_store::{
        ClaimSponsoredTicket, ClaimTicketReceipt, CompleteCampaignAtDeliveryTarget,
        CurrentHeadRequalification, DownstreamActionContext, SponsoredTicketClaimContext,
        TicketBufferStatus, TicketStore,
    },
};

/// The MVP permits one application-global Engineering claim at a time. This
/// is a kernel concurrency invariant, not a replaceable product policy: V2
/// concurrency requires a separate aggregate-cost reservation design.
pub const IN_FLIGHT_TICKET_MAXIMUM: u32 = 1;

/// Narrow read/action facade for the resident daemon. It owns no process
/// handles and therefore cannot start a paid actor by itself.
#[derive(Clone, Debug)]
pub struct TicketScheduler {
    tickets: TicketStore,
}

impl KernelStore {
    #[must_use]
    pub fn ticket_scheduler(&self) -> TicketScheduler {
        TicketScheduler {
            tickets: self.ticket_store(),
        }
    }
}

/// A campaign terminal transition, fenced by the campaign revision observed
/// when the buffer was read.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct CompleteCampaignAction {
    pub campaign_id: CampaignId,
    pub expected_campaign_revision: ExpectedRevision,
}

/// One exact sponsored revision to requalify and claim. This is not an
/// assignment or a process-launch instruction.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ClaimReadyTicketAction {
    pub campaign_id: CampaignId,
    pub expected_campaign_revision: ExpectedRevision,
    pub ticket: SponsoredTicketClaimContext,
}

/// A read-only derived constraint. The daemon may expose this verbatim in
/// status without manufacturing a waiting state in PostgreSQL.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum SchedulerConstraint {
    CampaignTerminal,
    AggregateCostFrozen,
    CampaignDeadlineElapsed,
    PaidSessionActive,
    InFlightTicketLimitReached { in_flight_count: u32, maximum: u32 },
    ReadyBufferMaximumExceeded { ready_count: u32, maximum: u32 },
    ProposalBufferMaximumExceeded { proposed_count: u32, maximum: u32 },
    DownstreamActionHeadMissing,
    DownstreamActionHeadUnexpected,
    ReadyBufferHeadMissing,
    ReadyBufferHeadUnexpected,
}

/// The daemon's next deterministic action. `ReplenishProduct` and
/// `ClaimReadyTicket` request later packet/assignment construction; neither
/// variant starts a process. `AwaitArchitectDecision` likewise has no write
/// side effect.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum SchedulerNextAction {
    CompleteCampaign(CompleteCampaignAction),
    ReplenishProduct {
        campaign_id: CampaignId,
        expected_campaign_revision: ExpectedRevision,
    },
    ClaimReadyTicket(ClaimReadyTicketAction),
    ContinueDownstream(DownstreamActionContext),
    AwaitArchitectDecision {
        campaign_id: CampaignId,
        proposed_count: u32,
    },
    Blocked(SchedulerConstraint),
    Idle {
        campaign_id: CampaignId,
    },
}

impl TicketScheduler {
    /// Reads only bounded ticket-buffer state. Calling this repeatedly after a
    /// daemon restart has no write amplification and produces the same action
    /// for the same durable snapshot.
    pub async fn next_action(
        &self,
        campaign_id: CampaignId,
    ) -> Result<SchedulerNextAction, StoreError> {
        let status = self.tickets.ticket_buffer_status(campaign_id).await?;
        Ok(Self::decide(&status))
    }

    /// Pure decision ordering used by [`Self::next_action`] and deterministic
    /// tests. Durable transition methods remain the sole race boundary.
    #[must_use]
    pub fn decide(status: &TicketBufferStatus) -> SchedulerNextAction {
        if !status.campaign_is_running {
            return SchedulerNextAction::Blocked(SchedulerConstraint::CampaignTerminal);
        }
        if status.delivered_attempt_count >= status.delivery_target {
            return SchedulerNextAction::CompleteCampaign(CompleteCampaignAction {
                campaign_id: status.campaign_id,
                expected_campaign_revision: ExpectedRevision::new(status.campaign_revision),
            });
        }
        if !status.campaign_cost_known {
            return SchedulerNextAction::Blocked(SchedulerConstraint::AggregateCostFrozen);
        }
        if status.paid_session_active {
            return SchedulerNextAction::Blocked(SchedulerConstraint::PaidSessionActive);
        }
        if !status.campaign_deadline_open {
            // A deadline stops paid discovery and implementation work. It
            // must not strand a candidate that has already passed Quality and
            // received the Architect's delivery decision: local delivery is
            // deterministic, costs no provider budget, and is the durable
            // closeout of work admitted before the deadline.
            if status.downstream_attempt_count > 0
                && let Some(action) = status.downstream_action
                && action.stage == crate::ticket_store::DownstreamActionStage::DeliverAccepted
            {
                return SchedulerNextAction::ContinueDownstream(action);
            }
            return SchedulerNextAction::Blocked(SchedulerConstraint::CampaignDeadlineElapsed);
        }
        // The read projection pairs the downstream count with its exact FIFO
        // head. A disagreement must stop scheduling rather than skip a
        // partially observed Engineering/Quality flow and open new work.
        match (status.downstream_attempt_count, status.downstream_action) {
            (0, Some(_)) => {
                return SchedulerNextAction::Blocked(
                    SchedulerConstraint::DownstreamActionHeadUnexpected,
                );
            }
            (1.., None) => {
                return SchedulerNextAction::Blocked(
                    SchedulerConstraint::DownstreamActionHeadMissing,
                );
            }
            (_, Some(action)) => return SchedulerNextAction::ContinueDownstream(action),
            (0, None) => {}
        }
        if status.in_flight_count >= IN_FLIGHT_TICKET_MAXIMUM {
            return SchedulerNextAction::Blocked(SchedulerConstraint::InFlightTicketLimitReached {
                in_flight_count: status.in_flight_count,
                maximum: IN_FLIGHT_TICKET_MAXIMUM,
            });
        }
        if status.ready_count > status.maximum {
            return SchedulerNextAction::Blocked(SchedulerConstraint::ReadyBufferMaximumExceeded {
                ready_count: status.ready_count,
                maximum: status.maximum,
            });
        }
        if status.proposed_count > status.proposal_maximum {
            return SchedulerNextAction::Blocked(
                SchedulerConstraint::ProposalBufferMaximumExceeded {
                    proposed_count: status.proposed_count,
                    maximum: status.proposal_maximum,
                },
            );
        }

        // The bounded FIFO read must agree with the aggregate ready count
        // before choosing either a sponsored Engineering claim or Product
        // replenishment. A cross-query snapshot race therefore fails closed
        // rather than discarding ready work or inventing another request.
        match (status.ready_count, status.oldest_sponsored_ticket) {
            (0, Some(_)) => {
                return SchedulerNextAction::Blocked(
                    SchedulerConstraint::ReadyBufferHeadUnexpected,
                );
            }
            (1.., None) => {
                return SchedulerNextAction::Blocked(SchedulerConstraint::ReadyBufferHeadMissing);
            }
            _ => {}
        }

        let expected_campaign_revision = ExpectedRevision::new(status.campaign_revision);
        // A sponsored revision is the oldest approved delivery work. Claim it
        // before refilling the discovery buffer: otherwise a low-water buffer
        // can indefinitely spend the sole paid-session slot on Product while
        // a ready implementation waits behind it.
        if let Some(ticket) = status.oldest_sponsored_ticket {
            return SchedulerNextAction::ClaimReadyTicket(ClaimReadyTicketAction {
                campaign_id: status.campaign_id,
                expected_campaign_revision,
                ticket,
            });
        }
        if status.ready_count < status.low_water && status.proposed_count == 0 {
            return SchedulerNextAction::ReplenishProduct {
                campaign_id: status.campaign_id,
                expected_campaign_revision,
            };
        }
        if status.proposed_count > 0 {
            return SchedulerNextAction::AwaitArchitectDecision {
                campaign_id: status.campaign_id,
                proposed_count: status.proposed_count,
            };
        }
        if status.ready_count < status.target {
            return SchedulerNextAction::ReplenishProduct {
                campaign_id: status.campaign_id,
                expected_campaign_revision,
            };
        }
        SchedulerNextAction::Idle {
            campaign_id: status.campaign_id,
        }
    }

    /// Applies a previously returned claim action through the ticket
    /// authority. The current-head requalification is supplied by the daemon
    /// after its deterministic product-boundary runner has produced both
    /// observations; this module never runs that command or launches Pi.
    pub async fn claim_ready_ticket(
        &self,
        principal: &str,
        command_id: &str,
        action: ClaimReadyTicketAction,
        requalification: CurrentHeadRequalification,
    ) -> Result<ClaimTicketReceipt, StoreError> {
        self.tickets
            .claim_sponsored_ticket(&ClaimSponsoredTicket {
                principal: principal.to_owned(),
                command_id: command_id.to_owned(),
                campaign_id: action.campaign_id,
                expected_campaign_revision: action.expected_campaign_revision,
                ticket_revision_id: action.ticket.ticket_revision_id,
                expected_ticket_revision: ExpectedRevision::new(action.ticket.revision),
                requalification,
            })
            .await
    }

    /// Applies the immediate-delivery-target completion action through the
    /// same typed campaign transition. A stale status cannot complete a
    /// campaign because the expected revision and durable delivered count are
    /// both checked by [`TicketStore`].
    pub async fn complete_campaign(
        &self,
        principal: &str,
        command_id: &str,
        action: CompleteCampaignAction,
    ) -> Result<AggregateRevision, StoreError> {
        self.tickets
            .complete_campaign_at_delivery_target(&CompleteCampaignAtDeliveryTarget {
                principal: principal.to_owned(),
                command_id: command_id.to_owned(),
                campaign_id: action.campaign_id,
                expected_campaign_revision: action.expected_campaign_revision,
            })
            .await
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use factory_protocol::{CandidateId, TicketAttemptId, TicketRevisionId};

    fn status() -> TicketBufferStatus {
        TicketBufferStatus {
            campaign_id: CampaignId::new(11).expect("positive campaign"),
            campaign_revision: AggregateRevision::from_persisted(7),
            campaign_is_running: true,
            campaign_deadline_open: true,
            campaign_cost_known: true,
            delivery_target: 2,
            delivered_attempt_count: 0,
            ready_count: 0,
            proposed_count: 0,
            in_flight_count: 0,
            downstream_attempt_count: 0,
            downstream_action: None,
            downstream_evidence: None,
            paid_session_active: false,
            low_water: 2,
            target: 3,
            maximum: 5,
            proposal_maximum: 3,
            oldest_sponsored_ticket: None,
        }
    }

    fn ready_ticket(id: i64, revision: u64) -> SponsoredTicketClaimContext {
        SponsoredTicketClaimContext {
            ticket_revision_id: TicketRevisionId::new(id).expect("positive ticket revision"),
            revision: AggregateRevision::from_persisted(revision),
        }
    }

    fn downstream_action(
        stage: crate::ticket_store::DownstreamActionStage,
        attempt_id: i64,
        attempt_revision: u64,
        candidate_id: i64,
        candidate_revision: u64,
    ) -> DownstreamActionContext {
        DownstreamActionContext {
            stage,
            ticket_attempt_id: TicketAttemptId::new(attempt_id).expect("positive attempt"),
            ticket_attempt_revision: AggregateRevision::from_persisted(attempt_revision),
            ticket_revision: AggregateRevision::from_persisted(1),
            candidate_id: CandidateId::new(candidate_id).expect("positive candidate"),
            candidate_revision: AggregateRevision::from_persisted(candidate_revision),
        }
    }

    #[test]
    fn empty_low_target_and_full_buffers_have_one_deterministic_next_action() {
        let empty = status();
        assert!(matches!(
            TicketScheduler::decide(&empty),
            SchedulerNextAction::ReplenishProduct { .. }
        ));

        let low = TicketBufferStatus {
            ready_count: 1,
            oldest_sponsored_ticket: Some(ready_ticket(30, 4)),
            ..status()
        };
        assert_eq!(
            TicketScheduler::decide(&low),
            SchedulerNextAction::ClaimReadyTicket(ClaimReadyTicketAction {
                campaign_id: low.campaign_id,
                expected_campaign_revision: ExpectedRevision::new(low.campaign_revision),
                ticket: ready_ticket(30, 4),
            }),
            "a sponsored ready ticket is implementation work, not a reason to spend on buffer refill"
        );

        let target = TicketBufferStatus {
            ready_count: 3,
            oldest_sponsored_ticket: Some(ready_ticket(30, 4)),
            ..status()
        };
        assert_eq!(
            TicketScheduler::decide(&target),
            SchedulerNextAction::ClaimReadyTicket(ClaimReadyTicketAction {
                campaign_id: target.campaign_id,
                expected_campaign_revision: ExpectedRevision::new(target.campaign_revision),
                ticket: ready_ticket(30, 4),
            })
        );

        let full = TicketBufferStatus {
            ready_count: 5,
            oldest_sponsored_ticket: Some(ready_ticket(9, 2)),
            ..status()
        };
        assert_eq!(
            TicketScheduler::decide(&full),
            SchedulerNextAction::ClaimReadyTicket(ClaimReadyTicketAction {
                campaign_id: full.campaign_id,
                expected_campaign_revision: ExpectedRevision::new(full.campaign_revision),
                ticket: ready_ticket(9, 2),
            })
        );
    }

    #[test]
    fn proposal_backpressure_and_in_flight_work_precede_new_product_or_claims() {
        let awaiting_architect = TicketBufferStatus {
            proposed_count: 1,
            ..status()
        };
        assert!(matches!(
            TicketScheduler::decide(&awaiting_architect),
            SchedulerNextAction::AwaitArchitectDecision { .. }
        ));

        let mixed_ready_and_proposed = TicketBufferStatus {
            ready_count: 3,
            proposed_count: 1,
            oldest_sponsored_ticket: Some(ready_ticket(31, 5)),
            ..status()
        };
        assert_eq!(
            TicketScheduler::decide(&mixed_ready_and_proposed),
            SchedulerNextAction::ClaimReadyTicket(ClaimReadyTicketAction {
                campaign_id: mixed_ready_and_proposed.campaign_id,
                expected_campaign_revision: ExpectedRevision::new(
                    mixed_ready_and_proposed.campaign_revision,
                ),
                ticket: ready_ticket(31, 5),
            }),
            "sponsored FIFO work is ahead of unrelated proposed tickets"
        );

        let at_proposal_maximum = TicketBufferStatus {
            proposed_count: 3,
            ready_count: 1,
            oldest_sponsored_ticket: Some(ready_ticket(30, 4)),
            ..status()
        };
        assert_eq!(
            TicketScheduler::decide(&at_proposal_maximum),
            SchedulerNextAction::ClaimReadyTicket(ClaimReadyTicketAction {
                campaign_id: at_proposal_maximum.campaign_id,
                expected_campaign_revision: ExpectedRevision::new(
                    at_proposal_maximum.campaign_revision,
                ),
                ticket: ready_ticket(30, 4),
            }),
            "proposal pressure stops Product replenishment, never a sponsored FIFO claim"
        );

        let in_flight = TicketBufferStatus {
            ready_count: 5,
            in_flight_count: 1,
            oldest_sponsored_ticket: Some(ready_ticket(30, 4)),
            ..status()
        };
        assert_eq!(
            TicketScheduler::decide(&in_flight),
            SchedulerNextAction::Blocked(SchedulerConstraint::InFlightTicketLimitReached {
                in_flight_count: 1,
                maximum: IN_FLIGHT_TICKET_MAXIMUM,
            })
        );

        let engineering_rework = downstream_action(
            crate::ticket_store::DownstreamActionStage::ReworkEngineering,
            7,
            4,
            12,
            8,
        );
        let downstream = TicketBufferStatus {
            in_flight_count: 1,
            downstream_attempt_count: 1,
            downstream_action: Some(engineering_rework),
            ..status()
        };
        assert_eq!(
            TicketScheduler::decide(&downstream),
            SchedulerNextAction::ContinueDownstream(engineering_rework)
        );
    }

    #[test]
    fn downstream_fifo_actions_preserve_stage_and_all_concurrency_revisions() {
        let first_quality = downstream_action(
            crate::ticket_store::DownstreamActionStage::Quality,
            17,
            2,
            20,
            3,
        );
        let quality = TicketBufferStatus {
            downstream_attempt_count: 1,
            downstream_action: Some(first_quality),
            ..status()
        };
        assert_eq!(
            TicketScheduler::decide(&quality),
            SchedulerNextAction::ContinueDownstream(first_quality)
        );

        let rework_quality = downstream_action(
            crate::ticket_store::DownstreamActionStage::ReworkQuality,
            17,
            9,
            24,
            4,
        );
        let rework = TicketBufferStatus {
            downstream_attempt_count: 1,
            downstream_action: Some(rework_quality),
            ..status()
        };
        assert_eq!(
            TicketScheduler::decide(&rework),
            SchedulerNextAction::ContinueDownstream(rework_quality)
        );

        let attach_candidate_commit = downstream_action(
            crate::ticket_store::DownstreamActionStage::CandidateCommitAttachRequired,
            17,
            10,
            24,
            5,
        );
        let attach = TicketBufferStatus {
            downstream_attempt_count: 1,
            downstream_action: Some(attach_candidate_commit),
            ..status()
        };
        assert_eq!(
            TicketScheduler::decide(&attach),
            SchedulerNextAction::ContinueDownstream(attach_candidate_commit)
        );

        let require_quality_review = downstream_action(
            crate::ticket_store::DownstreamActionStage::QualityReviewRequired,
            17,
            11,
            24,
            6,
        );
        let review = TicketBufferStatus {
            downstream_attempt_count: 1,
            downstream_action: Some(require_quality_review),
            ..status()
        };
        assert_eq!(
            TicketScheduler::decide(&review),
            SchedulerNextAction::ContinueDownstream(require_quality_review)
        );

        let deliver_accepted = downstream_action(
            crate::ticket_store::DownstreamActionStage::DeliverAccepted,
            17,
            10,
            24,
            5,
        );
        let delivery = TicketBufferStatus {
            downstream_attempt_count: 1,
            downstream_action: Some(deliver_accepted),
            ..status()
        };
        assert_eq!(
            TicketScheduler::decide(&delivery),
            SchedulerNextAction::ContinueDownstream(deliver_accepted)
        );
    }

    #[test]
    fn cost_and_paid_session_gates_precede_expired_campaign_closeout() {
        let complete = TicketBufferStatus {
            delivered_attempt_count: 2,
            campaign_cost_known: false,
            ..status()
        };
        assert!(matches!(
            TicketScheduler::decide(&complete),
            SchedulerNextAction::CompleteCampaign(_)
        ));

        let frozen = TicketBufferStatus {
            campaign_cost_known: false,
            ..status()
        };
        assert_eq!(
            TicketScheduler::decide(&frozen),
            SchedulerNextAction::Blocked(SchedulerConstraint::AggregateCostFrozen)
        );
        let expired = TicketBufferStatus {
            campaign_deadline_open: false,
            ..status()
        };
        assert_eq!(
            TicketScheduler::decide(&expired),
            SchedulerNextAction::Blocked(SchedulerConstraint::CampaignDeadlineElapsed)
        );
        let paid = TicketBufferStatus {
            paid_session_active: true,
            ..status()
        };
        assert_eq!(
            TicketScheduler::decide(&paid),
            SchedulerNextAction::Blocked(SchedulerConstraint::PaidSessionActive)
        );

        let delivery = downstream_action(
            crate::ticket_store::DownstreamActionStage::DeliverAccepted,
            17,
            10,
            24,
            5,
        );
        let expired_delivery = TicketBufferStatus {
            campaign_deadline_open: false,
            downstream_attempt_count: 1,
            downstream_action: Some(delivery),
            ..status()
        };
        assert_eq!(
            TicketScheduler::decide(&expired_delivery),
            SchedulerNextAction::ContinueDownstream(delivery),
            "deadline expiry blocks paid work, not deterministic local delivery"
        );
        let expired_delivery_with_paid_session = TicketBufferStatus {
            paid_session_active: true,
            ..expired_delivery
        };
        assert_eq!(
            TicketScheduler::decide(&expired_delivery_with_paid_session),
            SchedulerNextAction::Blocked(SchedulerConstraint::PaidSessionActive)
        );
    }

    #[test]
    fn restart_and_stale_claim_reads_are_deterministic_and_revision_fenced() {
        let snapshot = TicketBufferStatus {
            ready_count: 3,
            oldest_sponsored_ticket: Some(ready_ticket(21, 8)),
            ..status()
        };
        let before_restart = TicketScheduler::decide(&snapshot);
        let after_restart = TicketScheduler::decide(&snapshot);
        assert_eq!(before_restart, after_restart);

        let SchedulerNextAction::ClaimReadyTicket(action) = before_restart else {
            panic!("ready FIFO head must produce a claim action");
        };
        assert_eq!(action.ticket, ready_ticket(21, 8));
        let changed_head = TicketBufferStatus {
            oldest_sponsored_ticket: Some(ready_ticket(21, 9)),
            ..snapshot
        };
        let SchedulerNextAction::ClaimReadyTicket(changed_action) =
            TicketScheduler::decide(&changed_head)
        else {
            panic!("ready FIFO head must produce a claim action");
        };
        assert_ne!(action.ticket.revision, changed_action.ticket.revision);
    }

    #[test]
    fn inconsistent_buffer_reads_fail_closed_without_writes() {
        let missing_head = TicketBufferStatus {
            ready_count: 1,
            ..status()
        };
        assert_eq!(
            TicketScheduler::decide(&missing_head),
            SchedulerNextAction::Blocked(SchedulerConstraint::ReadyBufferHeadMissing)
        );
        let unexpected_head = TicketBufferStatus {
            oldest_sponsored_ticket: Some(ready_ticket(1, 0)),
            ..status()
        };
        assert_eq!(
            TicketScheduler::decide(&unexpected_head),
            SchedulerNextAction::Blocked(SchedulerConstraint::ReadyBufferHeadUnexpected)
        );
        let missing_downstream = TicketBufferStatus {
            downstream_attempt_count: 1,
            ..status()
        };
        assert_eq!(
            TicketScheduler::decide(&missing_downstream),
            SchedulerNextAction::Blocked(SchedulerConstraint::DownstreamActionHeadMissing)
        );
        let unexpected_downstream = TicketBufferStatus {
            downstream_action: Some(downstream_action(
                crate::ticket_store::DownstreamActionStage::Quality,
                1,
                0,
                1,
                0,
            )),
            ..status()
        };
        assert_eq!(
            TicketScheduler::decide(&unexpected_downstream),
            SchedulerNextAction::Blocked(SchedulerConstraint::DownstreamActionHeadUnexpected)
        );
        let overfull = TicketBufferStatus {
            ready_count: 6,
            oldest_sponsored_ticket: Some(ready_ticket(1, 0)),
            ..status()
        };
        assert!(matches!(
            TicketScheduler::decide(&overfull),
            SchedulerNextAction::Blocked(SchedulerConstraint::ReadyBufferMaximumExceeded { .. })
        ));
    }
}
