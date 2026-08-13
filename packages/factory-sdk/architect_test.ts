import { assertEquals, assertThrows } from "@std/assert";
import {
  ArchitectAdapterV1,
  validateArchitectCandidateDecisionV1,
  validateArchitectSponsorshipV1,
} from "./architect.ts";
import type { FrameTransport } from "./protocol.ts";
import { encodeJsonFrame, LocalProtocolClient, OPERATION } from "./protocol.ts";

const digest = (character: string): string => character.repeat(64);
const rationale = { artifact_id: 1, digest: digest("a"), byte_length: 100 };

Deno.test("Architect accepts only explicit qualitative-review delivery override relation", () => {
  validateArchitectCandidateDecisionV1({
    client_command_id: "architect-decision-1",
    expected_revision: 9,
    candidate_id: 3,
    review_id: 4,
    decision: "deliver",
    rationale,
    quality_rejection_override_review_id: 4,
    principal: "grand-architect",
  });
  assertThrows(
    () =>
      validateArchitectCandidateDecisionV1({
        client_command_id: "architect-decision-2",
        expected_revision: 9,
        candidate_id: 3,
        review_id: 4,
        decision: "rework",
        rationale,
        quality_rejection_override_review_id: 4,
        principal: "grand-architect",
      }),
    TypeError,
    "only for delivery",
  );
});

Deno.test("Architect adapter uses an explicit operator decision operation", async () => {
  class Transport implements FrameTransport {
    exchange(frame: Uint8Array): Promise<Uint8Array> {
      const request = JSON.parse(new TextDecoder().decode(frame.slice(4))) as Record<
        string,
        unknown
      >;
      return Promise.resolve(encodeJsonFrame({
        protocol_version: 1,
        request_id: request.request_id,
        operation: request.operation,
        audit_id: 20,
        aggregate_revision: 9,
        architect_decision_id: 21,
        decision_kind: "sponsor",
      }, 4 * 1024 * 1024));
    }
  }
  const adapter = new ArchitectAdapterV1(new LocalProtocolClient(new Transport()));
  const input = {
    client_command_id: "architect-sponsor-1",
    expected_revision: 8,
    ticket_revision_id: 2,
    rationale,
    principal: "grand-architect",
  };
  validateArchitectSponsorshipV1(input);
  const receipt = await adapter.sponsorTicketRevision(input);
  assertEquals(receipt.operation, OPERATION.architectSponsorTicketRevision);
  assertEquals(receipt.decision_kind, "sponsor");
});
