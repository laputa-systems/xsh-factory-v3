import { assertEquals, assertThrows } from "@std/assert";

import { OperatorAdapterV1, validateAuditShowV1, validateCampaignStartV1 } from "./operator.ts";
import {
  decodeJsonFrame,
  encodeJsonFrame,
  type FrameTransport,
  LocalProtocolClient,
  OPERATION,
} from "./protocol.ts";

class OperatorTransport implements FrameTransport {
  readonly requests: Record<string, unknown>[] = [];

  exchange(frame: Uint8Array): Promise<Uint8Array> {
    const request = decodeJsonFrame<Record<string, unknown>>(frame, "operator request");
    this.requests.push(request);
    const envelope = {
      protocol_version: 1,
      request_id: request.request_id,
      operation: request.operation,
    };
    const response = (() => {
      switch (request.operation) {
        case OPERATION.factorydStatus:
          return {
            ...envelope,
            state: "ready",
            current_kernel_build_id: "a".repeat(64),
            aggregate_revision: 3,
          };
        case OPERATION.operatorCampaignStart:
        case OPERATION.operatorCampaignCancel:
          return {
            ...envelope,
            audit_id: 7,
            aggregate_revision: 3,
            campaign_id: 4,
            kernel_build_id: "a".repeat(64),
            application_revision_id: 5,
            repository_id: 6,
            was_idempotent_retry: false,
          };
        case OPERATION.operatorCampaignStatus:
          return {
            ...envelope,
            campaign_id: 4,
            state: "running",
            aggregate_revision: 3,
            kernel_build_id: "a".repeat(64),
            application_revision_id: 5,
            repository_id: 6,
            aggregate_budget_micro_usd: 100,
            measured_cost_state: "known",
            measured_cost_micro_usd: 10,
            remaining_budget_micro_usd: 90,
            deadline_unix_millis: 4_000_000_000_000,
            delivery_target: 1,
            failure_reason: null,
            base_commit: "b".repeat(40),
            candidate_tree: "c".repeat(40),
            candidate_commit: null,
            delivered_commit: null,
            delivered_factory_cost_micro_usd: null,
            delivered_attempt_count: 0,
            ready_ticket_count: 0,
            proposed_ticket_count: 0,
            in_flight_ticket_count: 1,
            downstream_ticket_attempt_count: 1,
            downstream_action_stage: "quality",
            downstream_ticket_attempt_id: 12,
            downstream_ticket_attempt_revision: 8,
            downstream_candidate_id: 13,
            downstream_candidate_revision: 4,
            downstream_evidence: {
              candidate_commit: null,
              latest_validation: null,
              review: null,
              architect_decision: null,
            },
            ready_low_water: 1,
            ready_target: 2,
            ready_maximum: 3,
            proposal_maximum: 2,
            oldest_sponsored_ticket_revision_id: null,
            oldest_sponsored_ticket_revision: null,
            scheduler_next_action: "continue_downstream",
            scheduler_constraint: null,
            session_costs: [{
              session_id: 14,
              assignment_id: 15,
              assignment_role: "quality",
              model_provider: "openai",
              model_id: "gpt-5.6",
              outcome: "running",
              cost_state: "pending",
              cost_micro_usd: null,
              elapsed_millis: 1,
            }],
            session_cost_aggregates: [{
              assignment_role: "quality",
              model_provider: "openai",
              model_id: "gpt-5.6",
              outcome: "running",
              session_count: 1,
              accounted_cost_micro_usd: 0,
              pending_cost_session_count: 1,
              unknown_cost_session_count: 0,
              exceeded_cost_session_count: 0,
            }],
          };
        case OPERATION.operatorTicketList:
          return { ...envelope, items: [] };
        case OPERATION.operatorTicketShow:
          return {
            ...envelope,
            ticket_id: 9,
            ticket_revision_id: 10,
            ticket_revision: 3,
            application_revision_id: 5,
            state: "sponsored",
            sponsorship_reason: null,
            blocked_reason: null,
            evidence: [],
            attempts: [],
          };
        case OPERATION.operatorCandidateShow:
          return {
            ...envelope,
            candidate_id: 13,
            candidate_revision: 4,
            state: "validated",
            ticket_attempt_id: 12,
            ticket_revision_id: 10,
            ticket_revision: 3,
            base_commit: "a".repeat(40),
            candidate_tree: "b".repeat(40),
            candidate_commit: null,
            evidence: [],
            validations: [],
            review: null,
            latest_architect_decision: null,
            delivery_receipt: null,
            delivery: null,
          };
        case OPERATION.operatorAuditShow:
          return { ...envelope, selector: request.selector, items: [] };
        default:
          throw new Error(`unexpected operation ${String(request.operation)}`);
      }
    })();
    return Promise.resolve(encodeJsonFrame(response));
  }
}

Deno.test("Operator adapter mirrors the closed daemon control and navigation surface", async () => {
  const transport = new OperatorTransport();
  const operator = new OperatorAdapterV1(new LocalProtocolClient(transport));
  assertEquals((await operator.daemonStatus()).state, "ready");
  assertEquals(
    (await operator.startCampaign({
      client_command_id: "campaign-start-1",
      expected_application_revision: 2,
      application_revision_id: 5,
      aggregate_budget_micro_usd: 100,
      deadline_unix_millis: 4_000_000_000_000,
      delivery_target: 1,
      principal: "operator",
    })).campaign_id,
    4,
  );
  assertEquals((await operator.campaignStatus({ campaign_id: 4 })).downstream_candidate_id, 13);
  await operator.cancelCampaign({
    client_command_id: "campaign-cancel-1",
    expected_revision: 3,
    campaign_id: 4,
    principal: "operator",
  });
  await operator.listTickets({ state: null });
  assertEquals((await operator.showTicket({ ticket_id: 9 })).ticket_revision_id, 10);
  assertEquals((await operator.showCandidate({ candidate_id: 13 })).ticket_attempt_id, 12);
  await operator.showAudit({ selector: "candidate:13" });
  assertEquals(
    transport.requests.map((request) => request.operation),
    [
      OPERATION.factorydStatus,
      OPERATION.operatorCampaignStart,
      OPERATION.operatorCampaignStatus,
      OPERATION.operatorCampaignCancel,
      OPERATION.operatorTicketList,
      OPERATION.operatorTicketShow,
      OPERATION.operatorCandidateShow,
      OPERATION.operatorAuditShow,
    ],
  );
});

Deno.test("Operator adapter rejects non-closed inputs before the transport", () => {
  assertThrows(
    () => validateAuditShowV1({ selector: "ticket:1; DELETE FROM factory.audit_log" }),
    TypeError,
    "closed positive subject",
  );
  assertThrows(
    () =>
      validateCampaignStartV1(
        {
          client_command_id: "campaign-start-1",
          expected_application_revision: 2,
          application_revision_id: 5,
          aggregate_budget_micro_usd: 100,
          deadline_unix_millis: 4_000_000_000_000,
          delivery_target: 1,
          principal: "operator",
          kernel_build_id: "caller cannot choose this",
        } as unknown as Parameters<typeof validateCampaignStartV1>[0],
      ),
    TypeError,
    "unknown or missing",
  );
});
