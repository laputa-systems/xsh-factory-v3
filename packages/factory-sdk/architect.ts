/**
 * External Grand Architect decision surface.
 *
 * This adapter is for an operator connection, never a Pi actor connection.
 * The daemon binds that transport as operator authority; the `principal`
 * field records provenance but cannot turn an actor socket into an Architect.
 */

import type {
  ArchitectDecideCandidateCall,
  ArchitectDecisionReceiptResponse,
  ArchitectReleaseTicketAttemptCall,
  ArchitectSponsorTicketRevisionCall,
  LocalProtocolClient,
} from "./protocol.ts";
import {
  exactObject,
  validateCommandIdentityV1,
  validateSealedArtifactReferenceV1,
} from "./candidate.ts";

export const ARCHITECT_LIMITS_V1 = {
  principalByteLimit: 240,
  rationaleByteLimit: 128 * 1024,
} as const;

export interface ArchitectSponsorshipV1 extends ArchitectSponsorTicketRevisionCall {}
export interface ArchitectReleaseV1 extends ArchitectReleaseTicketAttemptCall {}
export interface ArchitectCandidateDecisionV1 extends ArchitectDecideCandidateCall {}

export class ArchitectAdapterV1 {
  readonly #operatorClient: LocalProtocolClient;

  /** `operatorClient` must have been created from the daemon's operator socket. */
  constructor(operatorClient: LocalProtocolClient) {
    this.#operatorClient = operatorClient;
  }

  async sponsorTicketRevision(
    input: ArchitectSponsorshipV1,
  ): Promise<ArchitectDecisionReceiptResponse> {
    validateArchitectSponsorshipV1(input);
    return await this.#operatorClient.architectSponsorTicketRevision(input);
  }

  async releaseTicketAttempt(input: ArchitectReleaseV1): Promise<ArchitectDecisionReceiptResponse> {
    validateArchitectReleaseV1(input);
    return await this.#operatorClient.architectReleaseTicketAttempt(input);
  }

  async decideCandidate(
    input: ArchitectCandidateDecisionV1,
  ): Promise<ArchitectDecisionReceiptResponse> {
    validateArchitectCandidateDecisionV1(input);
    return await this.#operatorClient.architectDecideCandidate(input);
  }
}

export function validateArchitectSponsorshipV1(input: ArchitectSponsorshipV1): void {
  exactObject(input, "Architect sponsorship", [
    "client_command_id",
    "expected_revision",
    "ticket_revision_id",
    "rationale",
    "principal",
  ]);
  validateCommandIdentityV1(input.client_command_id, input.expected_revision);
  positive(input.ticket_revision_id, "ticket revision ID");
  rationale(input.rationale);
  principal(input.principal);
}

export function validateArchitectReleaseV1(input: ArchitectReleaseV1): void {
  exactObject(input, "Architect release", [
    "client_command_id",
    "expected_revision",
    "ticket_attempt_id",
    "rationale",
    "principal",
  ]);
  validateCommandIdentityV1(input.client_command_id, input.expected_revision);
  positive(input.ticket_attempt_id, "ticket attempt ID");
  rationale(input.rationale);
  principal(input.principal);
}

export function validateArchitectCandidateDecisionV1(input: ArchitectCandidateDecisionV1): void {
  exactObject(input, "Architect candidate decision", [
    "client_command_id",
    "expected_revision",
    "candidate_id",
    "review_id",
    "decision",
    "rationale",
    "quality_rejection_override_review_id",
    "principal",
  ]);
  validateCommandIdentityV1(input.client_command_id, input.expected_revision);
  positive(input.candidate_id, "candidate ID");
  positive(input.review_id, "review ID");
  if (!["deliver", "rework", "reject"].includes(input.decision)) {
    fail("decision must be deliver, rework, or reject");
  }
  if (input.quality_rejection_override_review_id !== null) {
    positive(input.quality_rejection_override_review_id, "Quality override review ID");
    if (input.decision !== "deliver") {
      fail("a Quality rejection override is legal only for delivery");
    }
  }
  rationale(input.rationale);
  principal(input.principal);
}

function rationale(value: ArchitectSponsorshipV1["rationale"]): void {
  validateSealedArtifactReferenceV1(
    value,
    "Architect rationale",
    ARCHITECT_LIMITS_V1.rationaleByteLimit,
    false,
  );
}

function principal(value: string): void {
  if (typeof value !== "string" || value.length === 0 || value.includes("\0")) {
    fail("principal must be nonempty UTF-8 without NUL");
  }
  if (new TextEncoder().encode(value).byteLength > ARCHITECT_LIMITS_V1.principalByteLimit) {
    fail("principal exceeds byte limit");
  }
}

function positive(value: number, field: string): void {
  if (!Number.isSafeInteger(value) || value < 1) fail(`${field} is invalid`);
}

function fail(message: string): never {
  throw new TypeError(`invalid Architect operation: ${message}`);
}
