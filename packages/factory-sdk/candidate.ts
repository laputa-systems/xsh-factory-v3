/**
 * Engineering's closed terminal candidate submission.
 *
 * An Engineering actor supplies only its proposed message and regression
 * identity. The kernel—not this SDK and not the actor—captures the worktree,
 * patches, report, risks, and hard validation. It attaches the candidate
 * commit only after the terminal transcript is sealed for provenance.
 */

import type {
  CandidateCheckpointRegressionCall,
  CandidateReceiptResponse,
  CandidateSubmitCall,
  LocalProtocolClient,
  RegressionCheckpointReceiptResponse,
  SealedArtifactReferenceV1,
} from "./protocol.ts";

export const CANDIDATE_LIMITS_V1 = {
  commitSubjectByteLimit: 120,
  commitBodyByteLimit: 8 * 1024,
  regressionTestIdentityByteLimit: 4 * 1024,
} as const;

/** Exact Pi custom-tool input for Engineering's sole terminal action. */
export const CANDIDATE_SUBMIT_INPUT_SCHEMA_V1 = {
  type: "object",
  additionalProperties: false,
  required: [
    "client_command_id",
    "expected_revision",
    "commit_subject",
    "commit_body",
    "regression_test_identity",
  ],
  properties: {
    client_command_id: { type: "string", minLength: 1, maxLength: 160 },
    expected_revision: { type: "integer", minimum: 0 },
    commit_subject: {
      type: "string",
      minLength: 1,
      maxLength: CANDIDATE_LIMITS_V1.commitSubjectByteLimit,
    },
    commit_body: { type: "string", maxLength: CANDIDATE_LIMITS_V1.commitBodyByteLimit },
    regression_test_identity: {
      type: "string",
      minLength: 1,
      maxLength: CANDIDATE_LIMITS_V1.regressionTestIdentityByteLimit,
    },
  },
} as const;

/** Exact Pi custom-tool input for Engineering's one nonterminal checkpoint. */
export const CANDIDATE_CHECKPOINT_REGRESSION_INPUT_SCHEMA_V1 = {
  type: "object",
  additionalProperties: false,
  required: ["client_command_id", "expected_revision", "regression_command", "expected_failure"],
  properties: {
    client_command_id: { type: "string", minLength: 1, maxLength: 160 },
    expected_revision: { type: "integer", minimum: 0 },
    regression_command: { type: "string", minLength: 1, maxLength: 160 },
    expected_failure: {
      type: "string",
      minLength: 1,
      maxLength: CANDIDATE_LIMITS_V1.regressionTestIdentityByteLimit,
    },
  },
} as const;

export interface CandidateSubmissionV1 extends CandidateSubmitCall {}
export interface CandidateRegressionCheckpointV1 extends CandidateCheckpointRegressionCall {}

export class CandidateAdapterV1 {
  readonly #client: LocalProtocolClient;

  constructor(client: LocalProtocolClient) {
    this.#client = client;
  }

  async checkpointRegression(
    input: CandidateRegressionCheckpointV1,
  ): Promise<RegressionCheckpointReceiptResponse> {
    validateCandidateRegressionCheckpointV1(input);
    return await this.#client.candidateCheckpointRegression(input);
  }

  async submit(input: CandidateSubmissionV1): Promise<CandidateReceiptResponse> {
    validateCandidateSubmissionV1(input);
    return await this.#client.candidateSubmit(input);
  }
}

export function validateCandidateSubmissionV1(input: CandidateSubmissionV1): void {
  exactObject(input, "candidate submission", [
    "client_command_id",
    "expected_revision",
    "commit_subject",
    "commit_body",
    "regression_test_identity",
  ]);
  command(input.client_command_id, input.expected_revision);
  text(
    input.commit_subject,
    "candidate commit subject",
    CANDIDATE_LIMITS_V1.commitSubjectByteLimit,
  );
  if (input.commit_subject.includes("\n") || input.commit_subject.includes("\r")) {
    fail("candidate commit subject must be one line");
  }
  text(
    input.commit_body,
    "candidate commit body",
    CANDIDATE_LIMITS_V1.commitBodyByteLimit,
    true,
  );
  text(
    input.regression_test_identity,
    "candidate regression test identity",
    CANDIDATE_LIMITS_V1.regressionTestIdentityByteLimit,
  );
}

export function validateCandidateRegressionCheckpointV1(
  input: CandidateRegressionCheckpointV1,
): void {
  exactObject(input, "candidate regression checkpoint", [
    "client_command_id",
    "expected_revision",
    "regression_command",
    "expected_failure",
  ]);
  command(input.client_command_id, input.expected_revision);
  text(input.regression_command, "candidate regression command", 160);
  text(
    input.expected_failure,
    "candidate expected regression failure",
    CANDIDATE_LIMITS_V1.regressionTestIdentityByteLimit,
  );
}

export function validateSealedArtifactReferenceV1(
  value: SealedArtifactReferenceV1,
  field: string,
  maximum: number,
  allowEmpty: boolean,
): void {
  sealedArtifact(value, field, maximum, allowEmpty);
}

export function validateCommandIdentityV1(clientCommandId: string, expectedRevision: number): void {
  command(clientCommandId, expectedRevision);
}

export function exactObject(
  value: unknown,
  field: string,
  keys: readonly string[],
): asserts value is Record<string, unknown> {
  if (value === null || typeof value !== "object" || Array.isArray(value)) {
    fail(`${field} must be an object`);
  }
  const actual = Object.keys(value);
  if (actual.length !== keys.length || actual.some((key) => !keys.includes(key))) {
    fail(`${field} has an unknown or missing field`);
  }
}

function command(clientCommandId: string, expectedRevision: number): void {
  text(clientCommandId, "client command ID", 160);
  if (!Number.isSafeInteger(expectedRevision) || expectedRevision < 0) {
    fail("expected revision must be a nonnegative safe integer");
  }
}

function sealedArtifact(
  value: SealedArtifactReferenceV1,
  field: string,
  maximum: number,
  allowEmpty: boolean,
): void {
  exactObject(value, field, ["artifact_id", "digest", "byte_length"]);
  if (!Number.isSafeInteger(value.artifact_id) || value.artifact_id < 1) {
    fail(`${field} artifact ID is invalid`);
  }
  if (!/^[0-9a-f]{64}$/.test(value.digest)) fail(`${field} digest is invalid`);
  if (
    !Number.isSafeInteger(value.byte_length) || value.byte_length < 0 ||
    value.byte_length > maximum || (!allowEmpty && value.byte_length === 0)
  ) fail(`${field} byte limit is invalid`);
}

function text(value: string, field: string, maximum: number, allowEmpty = false): void {
  if (typeof value !== "string" || (!allowEmpty && value.length === 0) || value.includes("\0")) {
    fail(`${field} must be nonempty UTF-8 without NUL`);
  }
  if (new TextEncoder().encode(value).byteLength > maximum) fail(`${field} exceeds byte limit`);
}

function fail(message: string): never {
  throw new TypeError(`invalid candidate submission: ${message}`);
}
