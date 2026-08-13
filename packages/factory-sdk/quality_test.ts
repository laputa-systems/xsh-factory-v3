import { assert, assertEquals, assertThrows } from "@std/assert";
import {
  QUALITY_RUN_FULL_SUITE_INPUT_SCHEMA_V1,
  QUALITY_SUBMIT_REVIEW_INPUT_SCHEMA_V1,
  QualityAdapterV1,
  validateQualityReviewSubmissionV1,
} from "./quality.ts";
import type { FrameTransport } from "./protocol.ts";
import { encodeJsonFrame, LocalProtocolClient, OPERATION } from "./protocol.ts";

const digest = (character: string): string => character.repeat(64);
const artifact = (artifact_id: number, character: string, byte_length = 1) => ({
  artifact_id,
  digest: digest(character),
  byte_length,
});

const review = () => ({
  client_command_id: "quality-review-1",
  expected_revision: 8,
  full_suite_validation_id: 30,
  verdict: "reject" as const,
  rationale: artifact(1, "a", 64),
  risks: artifact(2, "b", 32),
  additional_probes: artifact(3, "c", 64),
});

Deno.test("Quality has one nonterminal validation tool and one terminal sealed review", () => {
  assertEquals(QUALITY_RUN_FULL_SUITE_INPUT_SCHEMA_V1.additionalProperties, false);
  assertEquals(QUALITY_SUBMIT_REVIEW_INPUT_SCHEMA_V1.additionalProperties, false);
  assert(QUALITY_RUN_FULL_SUITE_INPUT_SCHEMA_V1.required.includes("client_command_id"));
  assert(QUALITY_SUBMIT_REVIEW_INPUT_SCHEMA_V1.required.includes("full_suite_validation_id"));
  validateQualityReviewSubmissionV1(review());
  assertThrows(
    () =>
      validateQualityReviewSubmissionV1({
        ...review(),
        verdict: "waive",
      } as unknown as ReturnType<typeof review>),
    TypeError,
    "accept or reject",
  );
  assertThrows(
    () =>
      validateQualityReviewSubmissionV1({
        ...review(),
        reasons: "inline prose",
      } as unknown as ReturnType<typeof review>),
    TypeError,
    "unknown",
  );
});

Deno.test("Quality adapter binds review to its kernel-owned full-suite receipt", async () => {
  const operations: string[] = [];
  class Transport implements FrameTransport {
    exchange(frame: Uint8Array): Promise<Uint8Array> {
      const request = JSON.parse(new TextDecoder().decode(frame.slice(4))) as Record<
        string,
        unknown
      >;
      operations.push(request.operation as string);
      const full = request.operation === OPERATION.qualityRunFullSuite;
      return Promise.resolve(encodeJsonFrame(
        full
          ? {
            protocol_version: 1,
            request_id: request.request_id,
            operation: request.operation,
            audit_id: 12,
            aggregate_revision: 9,
            validation_id: 30,
            candidate_id: 9,
            candidate_tree: "a".repeat(40),
          }
          : {
            protocol_version: 1,
            request_id: request.request_id,
            operation: request.operation,
            audit_id: 13,
            aggregate_revision: 10,
            review_id: 31,
            candidate_id: 9,
            verdict: "reject",
          },
        4 * 1024 * 1024,
      ));
    }
  }
  const adapter = new QualityAdapterV1(new LocalProtocolClient(new Transport()));
  const validation = await adapter.runFullSuite({
    client_command_id: "quality-full-1",
    expected_revision: 8,
    validation_profile: "full",
  });
  const receipt = await adapter.submitReview({
    ...review(),
    full_suite_validation_id: validation.validation_id,
  });
  assertEquals(operations, [OPERATION.qualityRunFullSuite, OPERATION.qualitySubmitReview]);
  assertEquals(receipt.verdict, "reject");
});
