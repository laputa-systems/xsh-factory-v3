/** Independent Quality tools and review contract. */

import type {
  LocalProtocolClient,
  QualityReviewReceiptResponse,
  QualityRunFullSuiteCall,
  QualitySubmitReviewCall,
  QualityValidationReceiptResponse,
} from "./protocol.ts";
import {
  exactObject,
  validateCommandIdentityV1,
  validateSealedArtifactReferenceV1,
} from "./candidate.ts";

export const QUALITY_LIMITS_V1 = {
  validationProfileByteLimit: 160,
  rationaleByteLimit: 128 * 1024,
  risksByteLimit: 64 * 1024,
  additionalProbesByteLimit: 128 * 1024,
} as const;

const sealedArtifactSchema = {
  type: "object",
  additionalProperties: false,
  required: ["artifact_id", "digest", "byte_length"],
  properties: {
    artifact_id: { type: "integer", minimum: 1 },
    digest: { type: "string", pattern: "^[0-9a-f]{64}$" },
    byte_length: { type: "integer", minimum: 0 },
  },
} as const;

/** Nonterminal: it creates the kernel-owned receipt later required by review. */
export const QUALITY_RUN_FULL_SUITE_INPUT_SCHEMA_V1 = {
  type: "object",
  additionalProperties: false,
  required: ["client_command_id", "expected_revision", "validation_profile"],
  properties: {
    client_command_id: { type: "string", minLength: 1, maxLength: 160 },
    expected_revision: { type: "integer", minimum: 0 },
    validation_profile: {
      type: "string",
      minLength: 1,
      maxLength: QUALITY_LIMITS_V1.validationProfileByteLimit,
    },
  },
} as const;

/** Quality's sole terminal actor tool. All prose is separately sealed. */
export const QUALITY_SUBMIT_REVIEW_INPUT_SCHEMA_V1 = {
  type: "object",
  additionalProperties: false,
  required: [
    "client_command_id",
    "expected_revision",
    "full_suite_validation_id",
    "verdict",
    "rationale",
    "risks",
    "additional_probes",
  ],
  properties: {
    client_command_id: { type: "string", minLength: 1, maxLength: 160 },
    expected_revision: { type: "integer", minimum: 0 },
    full_suite_validation_id: { type: "integer", minimum: 1 },
    verdict: { type: "string", enum: ["accept", "reject"] },
    rationale: sealedArtifactSchema,
    risks: sealedArtifactSchema,
    additional_probes: sealedArtifactSchema,
  },
} as const;

export interface QualityFullSuiteInvocationV1 extends QualityRunFullSuiteCall {}
export interface QualityReviewSubmissionV1 extends QualitySubmitReviewCall {}

export class QualityAdapterV1 {
  readonly #client: LocalProtocolClient;

  constructor(client: LocalProtocolClient) {
    this.#client = client;
  }

  async runFullSuite(
    input: QualityFullSuiteInvocationV1,
  ): Promise<QualityValidationReceiptResponse> {
    validateQualityFullSuiteInvocationV1(input);
    return await this.#client.qualityRunFullSuite(input);
  }

  async submitReview(input: QualityReviewSubmissionV1): Promise<QualityReviewReceiptResponse> {
    validateQualityReviewSubmissionV1(input);
    return await this.#client.qualitySubmitReview(input);
  }
}

export function validateQualityFullSuiteInvocationV1(input: QualityFullSuiteInvocationV1): void {
  exactObject(input, "Quality full-suite invocation", [
    "client_command_id",
    "expected_revision",
    "validation_profile",
  ]);
  validateCommandIdentityV1(input.client_command_id, input.expected_revision);
  text(
    input.validation_profile,
    "Quality validation profile",
    QUALITY_LIMITS_V1.validationProfileByteLimit,
  );
}

export function validateQualityReviewSubmissionV1(input: QualityReviewSubmissionV1): void {
  exactObject(input, "Quality review submission", [
    "client_command_id",
    "expected_revision",
    "full_suite_validation_id",
    "verdict",
    "rationale",
    "risks",
    "additional_probes",
  ]);
  validateCommandIdentityV1(input.client_command_id, input.expected_revision);
  if (!Number.isSafeInteger(input.full_suite_validation_id) || input.full_suite_validation_id < 1) {
    fail("full-suite validation ID is invalid");
  }
  if (input.verdict !== "accept" && input.verdict !== "reject") {
    fail("verdict must be accept or reject");
  }
  validateSealedArtifactReferenceV1(
    input.rationale,
    "Quality rationale",
    QUALITY_LIMITS_V1.rationaleByteLimit,
    false,
  );
  validateSealedArtifactReferenceV1(
    input.risks,
    "Quality risks",
    QUALITY_LIMITS_V1.risksByteLimit,
    false,
  );
  validateSealedArtifactReferenceV1(
    input.additional_probes,
    "Quality additional probes",
    QUALITY_LIMITS_V1.additionalProbesByteLimit,
    false,
  );
}

function text(value: string, field: string, maximum: number): void {
  if (typeof value !== "string" || value.length === 0 || value.includes("\0")) {
    fail(`${field} must be nonempty UTF-8 without NUL`);
  }
  if (new TextEncoder().encode(value).byteLength > maximum) fail(`${field} exceeds byte limit`);
}

function fail(message: string): never {
  throw new TypeError(`invalid Quality operation: ${message}`);
}
