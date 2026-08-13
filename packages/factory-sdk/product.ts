/**
 * Product's closed ticket-proposal adapter.
 *
 * This module validates the actor-visible shape early. The Rust kernel repeats
 * every authority check, including CAS custody and application-revision
 * bounds, when it admits the repeatable `product.submit_ticket` operation.
 */

import type { TicketPolicyV1 } from "./application.ts";
import {
  type CommandObservationV1,
  type DuplicateSearchInputV1,
  EXACT_OBSERVATION_COMPARISON_V1,
  type LocalProtocolClient,
  type OperationReceiptResponse,
  type ProductTicketProposalV1,
  type SealedArtifactReferenceV1,
  type TicketContractReadV1,
  type TwoRunReproducerV1,
} from "./protocol.ts";

export const PRODUCT_PROPOSAL_LIMITS_V1 = {
  titleByteLimit: 240,
  missionValueByteLimit: 4096,
  scopeByteLimit: 4096,
  contractOwnerByteLimit: 240,
  riskByteLimit: 4096,
  acceptanceItemByteLimit: 4096,
  contractReadReasonByteLimit: 4096,
  evidenceByteLimit: 64 * 1024,
  reproducerCommandByteLimit: 64 * 1024,
  reproducerStdinByteLimit: 256 * 1024,
  reproducerStreamByteLimit: 4 * 1024 * 1024,
  reproducerProfileByteLimit: 160,
  duplicateSearchQueryByteLimit: 4096,
  duplicateSearchLimitMaximum: 20,
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

const observationSchema = {
  type: "object",
  additionalProperties: false,
  required: ["exit_status", "stdout", "stderr"],
  properties: {
    exit_status: { type: "integer" },
    stdout: sealedArtifactSchema,
    stderr: sealedArtifactSchema,
  },
} as const;

/**
 * The exact Pi custom-tool input surface for repeatable Product submission.
 * It intentionally exposes no sponsorship, lifecycle, actor identity, or
 * opaque metadata field. The host forwards this to the connection-bound
 * daemon; only `work.complete` terminates the Product assignment.
 */
export const PRODUCT_SUBMIT_TICKET_INPUT_SCHEMA_V1 = {
  type: "object",
  additionalProperties: false,
  required: [
    "client_command_id",
    "expected_revision",
    "title",
    "mission_value",
    "scope",
    "contract_owner",
    "risk",
    "narrative",
    "evidence",
    "acceptance_criteria",
    "contract_reads",
    "duplicate_search",
    "reproducer_profile",
    "reproducer",
  ],
  properties: {
    client_command_id: { type: "string", minLength: 1, maxLength: 160 },
    expected_revision: { type: "integer", minimum: 0 },
    title: { type: "string", minLength: 1, maxLength: PRODUCT_PROPOSAL_LIMITS_V1.titleByteLimit },
    mission_value: {
      type: "string",
      minLength: 1,
      maxLength: PRODUCT_PROPOSAL_LIMITS_V1.missionValueByteLimit,
    },
    scope: { type: "string", minLength: 1, maxLength: PRODUCT_PROPOSAL_LIMITS_V1.scopeByteLimit },
    contract_owner: {
      type: "string",
      minLength: 1,
      maxLength: PRODUCT_PROPOSAL_LIMITS_V1.contractOwnerByteLimit,
    },
    risk: { type: "string", minLength: 1, maxLength: PRODUCT_PROPOSAL_LIMITS_V1.riskByteLimit },
    narrative: sealedArtifactSchema,
    evidence: sealedArtifactSchema,
    acceptance_criteria: {
      type: "array",
      minItems: 1,
      items: {
        type: "string",
        minLength: 1,
        maxLength: PRODUCT_PROPOSAL_LIMITS_V1.acceptanceItemByteLimit,
      },
    },
    contract_reads: {
      type: "array",
      minItems: 1,
      items: {
        type: "object",
        additionalProperties: false,
        required: ["path", "reason"],
        properties: {
          path: { type: "string", minLength: 1 },
          reason: {
            type: "string",
            minLength: 1,
            maxLength: PRODUCT_PROPOSAL_LIMITS_V1.contractReadReasonByteLimit,
          },
        },
      },
    },
    duplicate_search: {
      type: "object",
      additionalProperties: false,
      required: ["query", "limit"],
      properties: {
        query: {
          type: "string",
          minLength: 1,
          maxLength: PRODUCT_PROPOSAL_LIMITS_V1.duplicateSearchQueryByteLimit,
        },
        limit: {
          type: "integer",
          minimum: 1,
          maximum: PRODUCT_PROPOSAL_LIMITS_V1.duplicateSearchLimitMaximum,
        },
      },
    },
    reproducer_profile: {
      type: "string",
      minLength: 1,
      maxLength: PRODUCT_PROPOSAL_LIMITS_V1.reproducerProfileByteLimit,
    },
    reproducer: {
      type: "object",
      additionalProperties: false,
      required: [
        "comparison_rule_version",
        "command",
        "stdin",
        "expected_observation",
        "first_observation",
        "second_observation",
      ],
      properties: {
        comparison_rule_version: { type: "integer", const: EXACT_OBSERVATION_COMPARISON_V1 },
        command: sealedArtifactSchema,
        stdin: { anyOf: [sealedArtifactSchema, { type: "null" }] },
        expected_observation: observationSchema,
        first_observation: observationSchema,
        second_observation: observationSchema,
      },
    },
  },
} as const;

export interface ProductSubmissionV1 {
  readonly client_command_id: string;
  readonly expected_revision: number;
  readonly proposal: ProductTicketProposalV1;
}

/**
 * Product's exact proposal adapter. `submitTicket` is repeatable while the
 * Product assignment runs; `work.complete` remains its only terminal actor
 * operation. It has no sponsor/approve operation: sponsorship remains an
 * explicit external Architect decision in the kernel.
 */
export class ProductAdapterV1 {
  readonly #client: LocalProtocolClient;
  readonly #ticketPolicy: TicketPolicyV1;

  constructor(client: LocalProtocolClient, ticketPolicy: TicketPolicyV1) {
    validateTicketPolicy(ticketPolicy);
    this.#client = client;
    this.#ticketPolicy = ticketPolicy;
  }

  async submitTicket(input: ProductSubmissionV1): Promise<OperationReceiptResponse> {
    exactObject(input, "Product submission", [
      "client_command_id",
      "expected_revision",
      "proposal",
    ]);
    commandIdentity(input);
    validateProductTicketProposalV1(input.proposal, this.#ticketPolicy);
    return await this.#client.productSubmitTicket({
      client_command_id: input.client_command_id,
      expected_revision: input.expected_revision,
      ...input.proposal,
    });
  }
}

/** Validates the closed ticket-reproducer proposal against pinned policy. */
export function validateProductTicketProposalV1(
  proposal: ProductTicketProposalV1,
  ticketPolicy: TicketPolicyV1,
): void {
  validateTicketPolicy(ticketPolicy);
  exactObject(proposal, "Product ticket proposal", [
    "title",
    "mission_value",
    "scope",
    "contract_owner",
    "risk",
    "narrative",
    "evidence",
    "acceptance_criteria",
    "contract_reads",
    "duplicate_search",
    "reproducer_profile",
    "reproducer",
  ]);
  boundedText(proposal.title, "ticket title", PRODUCT_PROPOSAL_LIMITS_V1.titleByteLimit);
  boundedText(
    proposal.mission_value,
    "ticket mission value",
    PRODUCT_PROPOSAL_LIMITS_V1.missionValueByteLimit,
  );
  boundedText(proposal.scope, "ticket scope", PRODUCT_PROPOSAL_LIMITS_V1.scopeByteLimit);
  boundedText(
    proposal.contract_owner,
    "ticket contract owner",
    PRODUCT_PROPOSAL_LIMITS_V1.contractOwnerByteLimit,
  );
  boundedText(proposal.risk, "ticket risk", PRODUCT_PROPOSAL_LIMITS_V1.riskByteLimit);
  sealedArtifact(
    proposal.narrative,
    "ticket narrative",
    ticketPolicy.ticket_bounds.narrative_byte_limit,
    false,
  );
  sealedArtifact(
    proposal.evidence,
    "ticket evidence",
    PRODUCT_PROPOSAL_LIMITS_V1.evidenceByteLimit,
    false,
  );

  if (
    !Array.isArray(proposal.acceptance_criteria) ||
    proposal.acceptance_criteria.length === 0 ||
    proposal.acceptance_criteria.length > ticketPolicy.ticket_bounds.acceptance_criteria_limit
  ) {
    fail("ticket acceptance criteria count is outside the application bound");
  }
  for (const criterion of proposal.acceptance_criteria) {
    boundedText(
      criterion,
      "ticket acceptance criterion",
      PRODUCT_PROPOSAL_LIMITS_V1.acceptanceItemByteLimit,
    );
  }

  if (
    !Array.isArray(proposal.contract_reads) ||
    proposal.contract_reads.length === 0 ||
    proposal.contract_reads.length > ticketPolicy.ticket_bounds.contract_read_limit
  ) {
    fail("ticket contract-read count is outside the application bound");
  }
  const readPaths = new Set<string>();
  for (const read of proposal.contract_reads) {
    ticketContractRead(read);
    if (readPaths.has(read.path)) fail("ticket contract reads repeat a path");
    readPaths.add(read.path);
  }

  validateDuplicateSearchInputV1(proposal.duplicate_search);
  boundedText(
    proposal.reproducer_profile,
    "reproducer profile",
    PRODUCT_PROPOSAL_LIMITS_V1.reproducerProfileByteLimit,
  );
  twoRunReproducer(proposal.reproducer);
}

/** Validates the exact input the kernel uses for a live duplicate lookup. */
export function validateDuplicateSearchInputV1(input: DuplicateSearchInputV1): void {
  exactObject(input, "duplicate search input", ["query", "limit"]);
  boundedText(
    input.query,
    "duplicate search query",
    PRODUCT_PROPOSAL_LIMITS_V1.duplicateSearchQueryByteLimit,
  );
  if (
    !Number.isSafeInteger(input.limit) ||
    input.limit < 1 ||
    input.limit > PRODUCT_PROPOSAL_LIMITS_V1.duplicateSearchLimitMaximum
  ) {
    fail("duplicate search limit must be between 1 and 20");
  }
}

function validateTicketPolicy(ticketPolicy: TicketPolicyV1): void {
  exactObject(ticketPolicy, "ticket policy", [
    "low_water",
    "target",
    "maximum",
    "proposal_maximum",
    "ticket_bounds",
  ]);
  positiveInteger(ticketPolicy.low_water, "ticket policy low_water");
  positiveInteger(ticketPolicy.target, "ticket policy target");
  positiveInteger(ticketPolicy.maximum, "ticket policy maximum");
  positiveInteger(ticketPolicy.proposal_maximum, "ticket policy proposal_maximum");
  if (ticketPolicy.low_water > ticketPolicy.target || ticketPolicy.target > ticketPolicy.maximum) {
    fail("ticket policy must satisfy low_water <= target <= maximum");
  }
  exactObject(ticketPolicy.ticket_bounds, "ticket bounds", [
    "narrative_byte_limit",
    "acceptance_criteria_limit",
    "contract_read_limit",
  ]);
  positiveInteger(ticketPolicy.ticket_bounds.narrative_byte_limit, "ticket narrative byte limit");
  positiveInteger(
    ticketPolicy.ticket_bounds.acceptance_criteria_limit,
    "ticket acceptance-criteria limit",
  );
  positiveInteger(ticketPolicy.ticket_bounds.contract_read_limit, "ticket contract-read limit");
}

function ticketContractRead(read: TicketContractReadV1): void {
  exactObject(read, "ticket contract read", ["path", "reason"]);
  repositoryRelativePath(read.path, "ticket contract read path");
  boundedText(
    read.reason,
    "ticket contract read reason",
    PRODUCT_PROPOSAL_LIMITS_V1.contractReadReasonByteLimit,
  );
}

function twoRunReproducer(reproducer: TwoRunReproducerV1): void {
  exactObject(reproducer, "two-run reproducer", [
    "comparison_rule_version",
    "command",
    "stdin",
    "expected_observation",
    "first_observation",
    "second_observation",
  ]);
  if (reproducer.comparison_rule_version !== EXACT_OBSERVATION_COMPARISON_V1) {
    fail("reproducer comparison rule version is unsupported");
  }
  sealedArtifact(
    reproducer.command,
    "reproducer command",
    PRODUCT_PROPOSAL_LIMITS_V1.reproducerCommandByteLimit,
    false,
  );
  if (reproducer.stdin !== null) {
    sealedArtifact(
      reproducer.stdin,
      "reproducer stdin",
      PRODUCT_PROPOSAL_LIMITS_V1.reproducerStdinByteLimit,
      false,
    );
  }
  commandObservation(reproducer.expected_observation, "expected reproducer observation");
  commandObservation(reproducer.first_observation, "first reproducer observation");
  commandObservation(reproducer.second_observation, "second reproducer observation");
  if (!sameObservation(reproducer.first_observation, reproducer.second_observation)) {
    fail("the two reproducer observations do not match");
  }
  if (sameObservation(reproducer.first_observation, reproducer.expected_observation)) {
    fail("the reproducer already matches the expected behavior");
  }
}

function commandObservation(observation: CommandObservationV1, location: string): void {
  exactObject(observation, location, ["exit_status", "stdout", "stderr"]);
  if (!Number.isSafeInteger(observation.exit_status)) fail(`${location} exit_status is invalid`);
  sealedArtifact(
    observation.stdout,
    `${location} stdout`,
    PRODUCT_PROPOSAL_LIMITS_V1.reproducerStreamByteLimit,
    true,
  );
  sealedArtifact(
    observation.stderr,
    `${location} stderr`,
    PRODUCT_PROPOSAL_LIMITS_V1.reproducerStreamByteLimit,
    true,
  );
}

function sameObservation(left: CommandObservationV1, right: CommandObservationV1): boolean {
  return left.exit_status === right.exit_status &&
    sameArtifactBytes(left.stdout, right.stdout) &&
    sameArtifactBytes(left.stderr, right.stderr);
}

function sameArtifactBytes(
  left: SealedArtifactReferenceV1,
  right: SealedArtifactReferenceV1,
): boolean {
  return left.digest === right.digest && left.byte_length === right.byte_length;
}

function sealedArtifact(
  reference: SealedArtifactReferenceV1,
  location: string,
  maximum: number,
  allowEmpty: boolean,
): void {
  exactObject(reference, location, ["artifact_id", "digest", "byte_length"]);
  positiveInteger(reference.artifact_id, `${location} artifact_id`);
  digest(reference.digest, `${location} digest`);
  if (!Number.isSafeInteger(reference.byte_length) || reference.byte_length < 0) {
    fail(`${location} byte_length is invalid`);
  }
  if (!allowEmpty && reference.byte_length === 0) fail(`${location} must not be empty`);
  if (reference.byte_length > maximum) fail(`${location} exceeds its byte limit`);
}

function commandIdentity(input: ProductSubmissionV1): void {
  boundedText(input.client_command_id, "client command ID", 160);
  if (!Number.isSafeInteger(input.expected_revision) || input.expected_revision < 0) {
    fail("expected revision is invalid");
  }
}

function repositoryRelativePath(value: string, location: string): void {
  if (
    typeof value !== "string" || value.length === 0 || value.includes("\0") ||
    value.startsWith("/") || value.includes("\\") ||
    (value !== "." && value.split("/").some((part) => part === "" || part === "." || part === ".."))
  ) {
    fail(`${location} is not a safe repository-relative path`);
  }
}

function boundedText(value: string, location: string, maximum: number): void {
  if (
    typeof value !== "string" || value.length === 0 || value.includes("\0") ||
    new TextEncoder().encode(value).byteLength > maximum
  ) {
    fail(`${location} must be nonempty, bounded UTF-8 without NUL`);
  }
}

function positiveInteger(value: number, location: string): void {
  if (!Number.isSafeInteger(value) || value <= 0) fail(`${location} must be a positive integer`);
}

function digest(value: string, location: string): void {
  if (typeof value !== "string" || !/^[0-9a-f]{64}$/.test(value)) {
    fail(`${location} must be a lower-case BLAKE3 digest`);
  }
}

function exactObject(value: unknown, location: string, keys: readonly string[]): void {
  if (typeof value !== "object" || value === null || Array.isArray(value)) {
    fail(`${location} must be an object`);
  }
  if (Object.getPrototypeOf(value) !== Object.prototype) {
    fail(`${location} has an unsupported prototype`);
  }
  const actual = Reflect.ownKeys(value).map((key) => {
    if (typeof key !== "string") fail(`${location} has a symbol field`);
    return key;
  }).sort();
  const expected = [...keys].sort();
  if (actual.length !== expected.length || actual.some((key, index) => key !== expected[index])) {
    fail(`${location} has an unknown, missing, or non-enumerable field`);
  }
}

function fail(message: string): never {
  throw new TypeError(`invalid Product ticket proposal: ${message}`);
}
