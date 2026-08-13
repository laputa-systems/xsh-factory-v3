import { assert, assertEquals, assertThrows } from "@std/assert";
import {
  CANDIDATE_CHECKPOINT_REGRESSION_INPUT_SCHEMA_V1,
  CANDIDATE_SUBMIT_INPUT_SCHEMA_V1,
  CandidateAdapterV1,
  validateCandidateRegressionCheckpointV1,
  validateCandidateSubmissionV1,
} from "./candidate.ts";
import type { FrameTransport } from "./protocol.ts";
import { encodeJsonFrame, LocalProtocolClient, OPERATION } from "./protocol.ts";

const digest = (character: string): string => character.repeat(64);
const artifact = (artifact_id: number, character: string, byte_length = 1) => ({
  artifact_id,
  digest: digest(character),
  byte_length,
});

const submission = () => ({
  client_command_id: "candidate-1",
  expected_revision: 7,
  engineering_report: artifact(1, "a", 64),
  commit_subject: "Correct user-visible behavior",
  commit_body: "",
  regression_test_identity: "cargo test --locked regression",
  risks: artifact(2, "b", 32),
});

const checkpoint = () => ({
  client_command_id: "checkpoint-1",
  expected_revision: 7,
  regression_command: "targeted-regression",
  expected_failure: "must fail before the candidate change",
});

Deno.test("candidate checkpoint is a closed nonterminal adapter input", () => {
  validateCandidateRegressionCheckpointV1(checkpoint());
  assertEquals(CANDIDATE_CHECKPOINT_REGRESSION_INPUT_SCHEMA_V1.additionalProperties, false);
  assertThrows(
    () =>
      validateCandidateRegressionCheckpointV1(
        { ...checkpoint(), candidate_tree: "actor-claim" } as unknown as ReturnType<
          typeof checkpoint
        >,
      ),
    TypeError,
    "unknown",
  );
});

Deno.test("candidate terminal submission excludes actor-supplied trees and validates sealed evidence", () => {
  validateCandidateSubmissionV1(submission());
  assertEquals(CANDIDATE_SUBMIT_INPUT_SCHEMA_V1.additionalProperties, false);
  assert(
    !(CANDIDATE_SUBMIT_INPUT_SCHEMA_V1.required as readonly string[]).includes(
      "candidate_tree_artifact_id",
    ),
  );
  assertThrows(
    () =>
      validateCandidateSubmissionV1({
        ...submission(),
        candidate_tree: "actor-claim",
      } as unknown as ReturnType<typeof submission>),
    TypeError,
    "unknown",
  );
  assertThrows(
    () => validateCandidateSubmissionV1({ ...submission(), commit_subject: "bad\nsubject" }),
    TypeError,
    "one line",
  );
});

Deno.test("candidate adapter returns a kernel-owned candidate receipt", async () => {
  class Transport implements FrameTransport {
    exchange(frame: Uint8Array): Promise<Uint8Array> {
      const request = JSON.parse(new TextDecoder().decode(frame.slice(4))) as Record<
        string,
        unknown
      >;
      return Promise.resolve(encodeJsonFrame({
        protocol_version: 1,
        request_id: request.request_id,
        operation: OPERATION.candidateSubmit,
        audit_id: 8,
        aggregate_revision: 8,
        candidate_id: 9,
        validation_id: 10,
        candidate_tree: "a".repeat(40),
      }, 4 * 1024 * 1024));
    }
  }
  const receipt = await new CandidateAdapterV1(new LocalProtocolClient(new Transport())).submit(
    submission(),
  );
  assertEquals(receipt.candidate_id, 9);
  assertEquals(receipt.operation, OPERATION.candidateSubmit);
});

Deno.test("candidate adapter returns only checkpoint evidence navigation", async () => {
  class Transport implements FrameTransport {
    exchange(frame: Uint8Array): Promise<Uint8Array> {
      const request = JSON.parse(new TextDecoder().decode(frame.slice(4))) as Record<
        string,
        unknown
      >;
      return Promise.resolve(encodeJsonFrame({
        protocol_version: 1,
        request_id: request.request_id,
        operation: OPERATION.candidateCheckpointRegression,
        regression_tree: "a".repeat(40),
        regression_patch_artifact_id: 8,
        regression_command_set_artifact_id: 9,
        regression_log_artifact_id: 10,
      }, 4 * 1024 * 1024));
    }
  }
  const receipt = await new CandidateAdapterV1(new LocalProtocolClient(new Transport()))
    .checkpointRegression(checkpoint());
  assertEquals(receipt.regression_tree, "a".repeat(40));
  assertEquals(receipt.operation, OPERATION.candidateCheckpointRegression);
});
