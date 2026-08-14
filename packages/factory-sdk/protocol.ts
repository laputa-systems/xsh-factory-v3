/**
 * Framed local protocol client.
 *
 * The transport owns an already-connected Unix socket. This module never
 * opens a socket, talks HTTP, or serializes actor identity. For actor calls,
 * the daemon binds office/session/assignment/application context when it
 * creates the connection; only the operation payload crosses this API.
 */

export const PROTOCOL_VERSION_V1 = 1 as const;
export const REQUEST_FRAME_MAX_BYTES = 1 << 20;
export const RESPONSE_FRAME_MAX_BYTES = 4 << 20;
export const FRAME_PREFIX_BYTES = 4;

export const OPERATION = {
  workspaceRead: "workspace.read",
  artifactSealWorkspaceFile: "artifact.seal_workspace_file",
  artifactRead: "artifact.read",
  productSubmitTicket: "product.submit_ticket",
  candidateCheckpointRegression: "candidate.checkpoint_regression",
  candidateSubmit: "candidate.submit",
  qualityRunFullSuite: "quality.run_full_suite",
  qualitySubmitReview: "quality.submit_review",
  workComplete: "work.complete",
  architectSponsorTicketRevision: "architect.sponsor_ticket_revision",
  architectReleaseTicketAttempt: "architect.release_ticket_attempt",
  architectDecideCandidate: "architect.decide_candidate",
  operatorApplicationShow: "operator.application.show",
  operatorApplicationRegister: "operator.application.register",
  operatorApplicationActivate: "operator.application.activate",
  operatorArtifactSeal: "operator.artifact.seal",
  factorydStatus: "factoryd.status",
  operatorCampaignStart: "operator.campaign.start",
  operatorCampaignStatus: "operator.campaign.status",
  operatorCampaignCancel: "operator.campaign.cancel",
  operatorTicketList: "operator.ticket.list",
  operatorTicketShow: "operator.ticket.show",
  operatorCandidateShow: "operator.candidate.show",
  operatorAuditShow: "operator.audit.show",
  sessionVerifyPacket: "session.verify_packet",
  sessionSealArtifact: "session.seal_artifact",
  sessionSubmitTerminal: "session.submit_terminal",
  forumListTopics: "forum.list_topics",
  forumListThreads: "forum.list_threads",
  forumSearch: "forum.search",
  forumReadThread: "forum.read_thread",
  forumCreateTopic: "forum.create_topic",
  forumCreateThread: "forum.create_thread",
  forumPost: "forum.post",
} as const;

export type OperationName = (typeof OPERATION)[keyof typeof OPERATION];

export function isKnownOperation(value: string): value is OperationName {
  return (Object.values(OPERATION) as readonly string[]).includes(value);
}

export function assertKnownOperation(value: string): asserts value is OperationName {
  if (!isKnownOperation(value)) {
    throw new FrameProtocolError(
      "wrong_operation",
      `unknown frame operation ${JSON.stringify(value)}`,
    );
  }
}

export class FrameProtocolError extends Error {
  readonly code:
    | "missing_length"
    | "truncated"
    | "trailing_bytes"
    | "oversized"
    | "invalid_utf8"
    | "invalid_json"
    | "wrong_operation"
    | "unsupported_protocol";

  constructor(code: FrameProtocolError["code"], message: string) {
    super(message);
    this.name = "FrameProtocolError";
    this.code = code;
  }
}

export function encodeFrame(payload: Uint8Array, maximum = REQUEST_FRAME_MAX_BYTES): Uint8Array {
  if (payload.byteLength > maximum) {
    throw new FrameProtocolError(
      "oversized",
      `frame payload is ${payload.byteLength} bytes, exceeding the ${maximum}-byte limit`,
    );
  }
  if (payload.byteLength > 0xffff_ffff) {
    throw new FrameProtocolError("oversized", "frame payload exceeds the u32 length prefix");
  }
  const frame = new Uint8Array(FRAME_PREFIX_BYTES + payload.byteLength);
  new DataView(frame.buffer).setUint32(0, payload.byteLength, false);
  frame.set(payload, FRAME_PREFIX_BYTES);
  return frame;
}

export function decodeFrame(frame: Uint8Array, maximum = RESPONSE_FRAME_MAX_BYTES): Uint8Array {
  if (frame.byteLength < FRAME_PREFIX_BYTES) {
    throw new FrameProtocolError("missing_length", "frame length prefix cannot be read");
  }
  const payloadLength = new DataView(
    frame.buffer,
    frame.byteOffset,
    frame.byteLength,
  ).getUint32(0, false);
  if (payloadLength > maximum) {
    throw new FrameProtocolError(
      "oversized",
      `frame payload is ${payloadLength} bytes, exceeding the ${maximum}-byte limit`,
    );
  }
  const expected = FRAME_PREFIX_BYTES + payloadLength;
  if (frame.byteLength < expected) {
    throw new FrameProtocolError(
      "truncated",
      `frame is truncated: expected ${expected} bytes, received ${frame.byteLength}`,
    );
  }
  if (frame.byteLength > expected) {
    throw new FrameProtocolError(
      "trailing_bytes",
      `frame has trailing bytes: expected ${expected} bytes, received ${frame.byteLength}`,
    );
  }
  return frame.slice(FRAME_PREFIX_BYTES);
}

/** Recursively orders object keys while retaining array order. */
export function canonicalize<T>(value: T): T {
  if (Array.isArray(value)) return value.map((entry) => canonicalize(entry)) as T;
  if (value !== null && typeof value === "object") {
    const object = value as Record<string, unknown>;
    const sorted: Record<string, unknown> = {};
    for (const key of Object.keys(object).sort()) sorted[key] = canonicalize(object[key]);
    return sorted as T;
  }
  return value;
}

export function canonicalJson(value: unknown): string {
  return JSON.stringify(canonicalize(value));
}

export function encodeJsonFrame(
  value: unknown,
  maximum = REQUEST_FRAME_MAX_BYTES,
): Uint8Array {
  // Operation structs have a declared field order shared with Rust's
  // miniserde derives. Canonical key sorting belongs to application-bundle
  // compilation, not to the wire fixture representation.
  return encodeFrame(new TextEncoder().encode(JSON.stringify(value)), maximum);
}

export function decodeJsonFrame<T>(
  frame: Uint8Array,
  operation: string,
  maximum = RESPONSE_FRAME_MAX_BYTES,
): T {
  let payload: string;
  try {
    payload = new TextDecoder("utf-8", { fatal: true }).decode(decodeFrame(frame, maximum));
  } catch (error) {
    if (error instanceof FrameProtocolError) throw error;
    throw new FrameProtocolError(
      "invalid_utf8",
      `frame payload is not valid UTF-8 for ${operation}`,
    );
  }
  try {
    return JSON.parse(payload) as T;
  } catch (error) {
    throw new FrameProtocolError(
      "invalid_json",
      `frame JSON is invalid for ${operation}: ${
        error instanceof Error ? error.message : String(error)
      }`,
    );
  }
}

export type ProtocolResponseShape =
  | "receipt"
  | "artifact"
  | "artifact_read"
  | "workspace_read"
  | "packet_verification"
  | "regression_checkpoint"
  | "candidate"
  | "quality_validation"
  | "quality_review"
  | "architect_decision"
  | "application_show"
  | "application_revision"
  | "operator_artifact"
  | "daemon_status"
  | "campaign_receipt"
  | "campaign_status"
  | "ticket_list"
  | "ticket_show"
  | "candidate_show"
  | "audit_show"
  | "page";

function responseObject(value: unknown, operation: string): Record<string, unknown> {
  if (value === null || typeof value !== "object" || Array.isArray(value)) {
    throw new FrameProtocolError("invalid_json", `response for ${operation} must be an object`);
  }
  return value as Record<string, unknown>;
}

function responseInteger(
  response: Record<string, unknown>,
  field: string,
  operation: string,
): void {
  const value = response[field];
  if (typeof value !== "number" || !Number.isSafeInteger(value) || value < 0) {
    throw new FrameProtocolError(
      "invalid_json",
      `response for ${operation} requires nonnegative integer ${field}`,
    );
  }
}

function responseString(
  response: Record<string, unknown>,
  field: string,
  operation: string,
): void {
  if (typeof response[field] !== "string") {
    throw new FrameProtocolError(
      "invalid_json",
      `response for ${operation} requires string ${field}`,
    );
  }
}

function requiredResponseFields(
  response: Record<string, unknown>,
  fields: readonly string[],
  operation: string,
): void {
  for (const field of fields) {
    if (!(field in response)) {
      throw new FrameProtocolError(
        "invalid_json",
        `response for ${operation} is missing required field ${field}`,
      );
    }
  }
}

/** Validates the closed response envelope and its operation-specific success shape. */
export function validateProtocolResponse(
  value: unknown,
  expectedOperation: string,
  expectedRequestId: string,
  shape: ProtocolResponseShape = "receipt",
): asserts value is Record<string, unknown> {
  const response = responseObject(value, expectedOperation);
  if (response.protocol_version !== PROTOCOL_VERSION_V1) {
    throw new FrameProtocolError(
      "unsupported_protocol",
      `response protocol version is ${
        String(response.protocol_version)
      }, expected ${PROTOCOL_VERSION_V1}`,
    );
  }
  responseString(response, "request_id", expectedOperation);
  if (response.request_id !== expectedRequestId) {
    throw new FrameProtocolError(
      "wrong_operation",
      `response request_id is ${response.request_id}, expected ${expectedRequestId}`,
    );
  }
  responseString(response, "operation", expectedOperation);
  const responseOperation = response.operation as string;
  if (!isKnownOperation(responseOperation)) {
    throw new FrameProtocolError(
      "wrong_operation",
      `unknown response operation ${responseOperation}`,
    );
  }
  if (responseOperation !== expectedOperation) {
    throw new FrameProtocolError(
      "wrong_operation",
      `response operation is ${responseOperation}, expected ${expectedOperation}`,
    );
  }

  if ("error_code" in response) {
    responseString(response, "error_code", expectedOperation);
    responseString(response, "message", expectedOperation);
    if (
      response.error_code === "revision_conflict" ||
      response.error_code === "idempotency_conflict"
    ) {
      requiredResponseFields(response, ["current_revision"], expectedOperation);
      responseInteger(response, "current_revision", expectedOperation);
    }
    return;
  }

  switch (shape) {
    case "workspace_read":
      requiredResponseFields(
        response,
        ["canonical_path", "blake3", "byte_length", "content_base64"],
        expectedOperation,
      );
      responseString(response, "canonical_path", expectedOperation);
      responseString(response, "blake3", expectedOperation);
      responseInteger(response, "byte_length", expectedOperation);
      responseString(response, "content_base64", expectedOperation);
      break;
    case "artifact":
      requiredResponseFields(
        response,
        ["artifact_id", "digest", "byte_length", "aggregate_revision"],
        expectedOperation,
      );
      responseInteger(response, "artifact_id", expectedOperation);
      responseString(response, "digest", expectedOperation);
      responseInteger(response, "byte_length", expectedOperation);
      responseInteger(response, "aggregate_revision", expectedOperation);
      break;
    case "artifact_read":
      requiredResponseFields(
        response,
        ["artifact_id", "digest", "byte_length", "content_base64"],
        expectedOperation,
      );
      responseInteger(response, "artifact_id", expectedOperation);
      responseString(response, "digest", expectedOperation);
      responseInteger(response, "byte_length", expectedOperation);
      responseString(response, "content_base64", expectedOperation);
      break;
    case "packet_verification":
      requiredResponseFields(response, ["packet_digest", "verified"], expectedOperation);
      responseString(response, "packet_digest", expectedOperation);
      if (typeof response.verified !== "boolean") {
        throw new FrameProtocolError(
          "invalid_json",
          `response for ${expectedOperation} requires boolean verified`,
        );
      }
      break;
    case "page":
      requiredResponseFields(response, ["items", "next_cursor"], expectedOperation);
      if (!Array.isArray(response.items)) {
        throw new FrameProtocolError(
          "invalid_json",
          `response for ${expectedOperation} requires array items`,
        );
      }
      responseString(response, "next_cursor", expectedOperation);
      break;
    case "receipt":
      requiredResponseFields(response, ["audit_id", "aggregate_revision"], expectedOperation);
      responseInteger(response, "audit_id", expectedOperation);
      responseInteger(response, "aggregate_revision", expectedOperation);
      break;
    case "regression_checkpoint":
      requiredResponseFields(
        response,
        [
          "regression_tree",
          "regression_patch_artifact_id",
          "regression_command_set_artifact_id",
          "regression_log_artifact_id",
        ],
        expectedOperation,
      );
      responseString(response, "regression_tree", expectedOperation);
      responseInteger(response, "regression_patch_artifact_id", expectedOperation);
      responseInteger(response, "regression_command_set_artifact_id", expectedOperation);
      responseInteger(response, "regression_log_artifact_id", expectedOperation);
      break;
    case "candidate":
      requiredResponseFields(
        response,
        [
          "audit_id",
          "aggregate_revision",
          "candidate_id",
          "validation_id",
          "candidate_tree",
        ],
        expectedOperation,
      );
      responseInteger(response, "audit_id", expectedOperation);
      responseInteger(response, "aggregate_revision", expectedOperation);
      responseInteger(response, "candidate_id", expectedOperation);
      responseInteger(response, "validation_id", expectedOperation);
      responseString(response, "candidate_tree", expectedOperation);
      break;
    case "quality_validation":
      requiredResponseFields(
        response,
        ["audit_id", "aggregate_revision", "validation_id", "candidate_id", "candidate_tree"],
        expectedOperation,
      );
      responseInteger(response, "audit_id", expectedOperation);
      responseInteger(response, "aggregate_revision", expectedOperation);
      responseInteger(response, "validation_id", expectedOperation);
      responseInteger(response, "candidate_id", expectedOperation);
      responseString(response, "candidate_tree", expectedOperation);
      break;
    case "quality_review":
      requiredResponseFields(
        response,
        ["audit_id", "aggregate_revision", "review_id", "candidate_id", "verdict"],
        expectedOperation,
      );
      responseInteger(response, "audit_id", expectedOperation);
      responseInteger(response, "aggregate_revision", expectedOperation);
      responseInteger(response, "review_id", expectedOperation);
      responseInteger(response, "candidate_id", expectedOperation);
      if (response.verdict !== "accept" && response.verdict !== "reject") {
        throw new FrameProtocolError(
          "invalid_json",
          `response for ${expectedOperation} has an invalid Quality verdict`,
        );
      }
      break;
    case "architect_decision":
      requiredResponseFields(
        response,
        ["audit_id", "aggregate_revision", "architect_decision_id", "decision_kind"],
        expectedOperation,
      );
      responseInteger(response, "audit_id", expectedOperation);
      responseInteger(response, "aggregate_revision", expectedOperation);
      responseInteger(response, "architect_decision_id", expectedOperation);
      if (
        !["sponsor", "release", "deliver", "rework", "reject"].includes(
          response.decision_kind as string,
        )
      ) {
        throw new FrameProtocolError(
          "invalid_json",
          `response for ${expectedOperation} has an invalid Architect decision kind`,
        );
      }
      break;
    case "application_show":
      requiredResponseFields(
        response,
        [
          "application_key",
          "application_revision_id",
          "aggregate_revision",
          "bundle_artifact_id",
          "is_active",
        ],
        expectedOperation,
      );
      responseString(response, "application_key", expectedOperation);
      responseInteger(response, "application_revision_id", expectedOperation);
      responseInteger(response, "aggregate_revision", expectedOperation);
      responseInteger(response, "bundle_artifact_id", expectedOperation);
      if (typeof response.is_active !== "boolean") {
        throw new FrameProtocolError(
          "invalid_json",
          `response for ${expectedOperation} requires boolean is_active`,
        );
      }
      break;
    case "application_revision":
      requiredResponseFields(
        response,
        [
          "audit_id",
          "aggregate_revision",
          "application_revision_id",
          "is_active",
          "was_idempotent_retry",
        ],
        expectedOperation,
      );
      responseInteger(response, "audit_id", expectedOperation);
      responseInteger(response, "aggregate_revision", expectedOperation);
      responseInteger(response, "application_revision_id", expectedOperation);
      if (
        typeof response.is_active !== "boolean" ||
        typeof response.was_idempotent_retry !== "boolean"
      ) {
        throw new FrameProtocolError(
          "invalid_json",
          `response for ${expectedOperation} requires boolean application receipt flags`,
        );
      }
      break;
    case "operator_artifact":
      requiredResponseFields(
        response,
        [
          "audit_id",
          "aggregate_revision",
          "artifact_id",
          "digest",
          "byte_length",
          "was_idempotent_retry",
          "was_reused",
        ],
        expectedOperation,
      );
      responseInteger(response, "audit_id", expectedOperation);
      responseInteger(response, "aggregate_revision", expectedOperation);
      responseInteger(response, "artifact_id", expectedOperation);
      responseString(response, "digest", expectedOperation);
      responseInteger(response, "byte_length", expectedOperation);
      if (
        typeof response.was_idempotent_retry !== "boolean" ||
        typeof response.was_reused !== "boolean"
      ) {
        throw new FrameProtocolError(
          "invalid_json",
          `response for ${expectedOperation} requires boolean operator artifact receipt flags`,
        );
      }
      break;
    case "daemon_status":
      requiredResponseFields(
        response,
        ["state", "current_kernel_build_id", "aggregate_revision"],
        expectedOperation,
      );
      responseString(response, "state", expectedOperation);
      responseInteger(response, "aggregate_revision", expectedOperation);
      if (
        response.current_kernel_build_id !== null &&
        typeof response.current_kernel_build_id !== "string"
      ) {
        throw new FrameProtocolError(
          "invalid_json",
          `response for ${expectedOperation} has invalid current kernel build ID`,
        );
      }
      break;
    case "campaign_receipt":
      requiredResponseFields(
        response,
        [
          "audit_id",
          "aggregate_revision",
          "campaign_id",
          "kernel_build_id",
          "application_revision_id",
          "repository_id",
          "was_idempotent_retry",
        ],
        expectedOperation,
      );
      responseInteger(response, "audit_id", expectedOperation);
      responseInteger(response, "aggregate_revision", expectedOperation);
      responseInteger(response, "campaign_id", expectedOperation);
      responseString(response, "kernel_build_id", expectedOperation);
      responseInteger(response, "application_revision_id", expectedOperation);
      responseInteger(response, "repository_id", expectedOperation);
      if (typeof response.was_idempotent_retry !== "boolean") {
        throw new FrameProtocolError(
          "invalid_json",
          `response for ${expectedOperation} requires campaign receipt replay flag`,
        );
      }
      break;
    case "campaign_status":
      requiredResponseFields(
        response,
        [
          "campaign_id",
          "state",
          "aggregate_revision",
          "kernel_build_id",
          "application_revision_id",
          "repository_id",
          "aggregate_budget_micro_usd",
          "measured_cost_state",
          "deadline_unix_millis",
          "delivery_target",
          "base_commit",
          "candidate_tree",
          "candidate_commit",
          "delivered_commit",
          "delivered_factory_cost_micro_usd",
          "delivered_attempt_count",
          "ready_ticket_count",
          "proposed_ticket_count",
          "in_flight_ticket_count",
          "downstream_ticket_attempt_count",
          "downstream_evidence",
          "ready_low_water",
          "ready_target",
          "ready_maximum",
          "proposal_maximum",
          "scheduler_next_action",
          "scheduler_constraint",
          "session_costs",
          "session_cost_aggregates",
        ],
        expectedOperation,
      );
      for (
        const field of [
          "campaign_id",
          "aggregate_revision",
          "application_revision_id",
          "repository_id",
          "aggregate_budget_micro_usd",
          "deadline_unix_millis",
          "delivery_target",
          "delivered_attempt_count",
          "ready_ticket_count",
          "proposed_ticket_count",
          "in_flight_ticket_count",
          "downstream_ticket_attempt_count",
          "ready_low_water",
          "ready_target",
          "ready_maximum",
          "proposal_maximum",
        ]
      ) responseInteger(response, field, expectedOperation);
      for (
        const field of [
          "state",
          "kernel_build_id",
          "measured_cost_state",
          "scheduler_next_action",
        ]
      ) responseString(response, field, expectedOperation);
      if (!Array.isArray(response.session_costs)) {
        throw new FrameProtocolError(
          "invalid_json",
          `response for ${expectedOperation} requires bounded session_costs`,
        );
      }
      if (response.session_costs.length > 20) {
        throw new FrameProtocolError(
          "invalid_json",
          `response for ${expectedOperation} exceeds the session cost bound`,
        );
      }
      if (!Array.isArray(response.session_cost_aggregates)) {
        throw new FrameProtocolError(
          "invalid_json",
          `response for ${expectedOperation} requires session cost aggregates`,
        );
      }
      if (response.session_cost_aggregates.length > 18) {
        throw new FrameProtocolError(
          "invalid_json",
          `response for ${expectedOperation} exceeds the session cost aggregate bound`,
        );
      }
      for (
        const field of [
          "base_commit",
          "candidate_tree",
          "candidate_commit",
          "delivered_commit",
        ]
      ) {
        if (response[field] !== null && typeof response[field] !== "string") {
          throw new FrameProtocolError(
            "invalid_json",
            `response for ${expectedOperation} has invalid ${field}`,
          );
        }
      }
      if (
        response.delivered_factory_cost_micro_usd !== null &&
        !Number.isSafeInteger(response.delivered_factory_cost_micro_usd)
      ) {
        throw new FrameProtocolError(
          "invalid_json",
          `response for ${expectedOperation} has invalid delivered Factory-Cost`,
        );
      }
      if (
        response.downstream_evidence !== null &&
        (typeof response.downstream_evidence !== "object" ||
          Array.isArray(response.downstream_evidence))
      ) {
        throw new FrameProtocolError(
          "invalid_json",
          `response for ${expectedOperation} has invalid downstream evidence`,
        );
      }
      if (response.downstream_evidence !== null) {
        const evidence = response.downstream_evidence as Record<string, unknown>;
        requiredResponseFields(
          evidence,
          ["candidate_commit", "latest_validation", "review", "architect_decision"],
          expectedOperation,
        );
        if (evidence.candidate_commit !== null && typeof evidence.candidate_commit !== "string") {
          throw new FrameProtocolError(
            "invalid_json",
            `response for ${expectedOperation} has invalid candidate commit`,
          );
        }
        validateNullableDownstreamEvidence(
          evidence.latest_validation,
          ["validation_id", "state", "log_artifact_id"],
          ["validation_id", "log_artifact_id"],
          ["state"],
          expectedOperation,
        );
        validateNullableDownstreamEvidence(
          evidence.review,
          ["review_id", "review_revision", "verdict", "rationale_artifact_id"],
          ["review_id", "review_revision", "rationale_artifact_id"],
          ["verdict"],
          expectedOperation,
        );
        validateNullableDownstreamEvidence(
          evidence.architect_decision,
          ["architect_decision_id", "decision_kind", "rationale_artifact_id"],
          ["architect_decision_id", "rationale_artifact_id"],
          ["decision_kind"],
          expectedOperation,
        );
      }
      for (const session of response.session_costs) {
        if (typeof session !== "object" || session === null || Array.isArray(session)) {
          throw new FrameProtocolError(
            "invalid_json",
            `response for ${expectedOperation} has invalid session cost row`,
          );
        }
        const row = session as Record<string, unknown>;
        for (const field of ["session_id", "assignment_id"]) {
          responseInteger(row, field, expectedOperation);
        }
        for (const field of ["assignment_role", "model_provider", "model_id", "outcome", "cost_state"]) {
          responseString(row, field, expectedOperation);
        }
        if (
          !["product_research", "engineering", "quality"].includes(row.assignment_role as string) ||
          !["prepared", "running", "succeeded", "failed", "cancelled", "interrupted"].includes(
            row.outcome as string,
          ) ||
          !["pending", "known", "unknown", "exceeded"].includes(row.cost_state as string)
        ) {
          throw new FrameProtocolError(
            "invalid_json",
            `response for ${expectedOperation} has an unknown session cost state`,
          );
        }
        for (const field of ["cost_micro_usd", "elapsed_millis"]) {
          if (row[field] !== null && !Number.isSafeInteger(row[field])) {
            throw new FrameProtocolError(
              "invalid_json",
              `response for ${expectedOperation} has invalid ${field}`,
            );
          }
        }
      }
      for (const aggregate of response.session_cost_aggregates) {
        if (typeof aggregate !== "object" || aggregate === null || Array.isArray(aggregate)) {
          throw new FrameProtocolError(
            "invalid_json",
            `response for ${expectedOperation} has invalid session cost aggregate`,
          );
        }
        const row = aggregate as Record<string, unknown>;
        for (const field of ["assignment_role", "model_provider", "model_id", "outcome"]) {
          responseString(row, field, expectedOperation);
        }
        for (
          const field of [
            "session_count",
            "accounted_cost_micro_usd",
            "pending_cost_session_count",
            "unknown_cost_session_count",
            "exceeded_cost_session_count",
          ]
        ) {
          responseInteger(row, field, expectedOperation);
        }
        if (
          !["product_research", "engineering", "quality"].includes(row.assignment_role as string) ||
          !["prepared", "running", "succeeded", "failed", "cancelled", "interrupted"].includes(
            row.outcome as string,
          )
        ) {
          throw new FrameProtocolError(
            "invalid_json",
            `response for ${expectedOperation} has unknown session cost aggregate identity`,
          );
        }
      }
      break;
    case "ticket_list":
      requiredResponseFields(response, ["items"], expectedOperation);
      if (!Array.isArray(response.items)) {
        throw new FrameProtocolError(
          "invalid_json",
          `response for ${expectedOperation} requires ticket array items`,
        );
      }
      break;
    case "ticket_show":
      requiredResponseFields(
        response,
        ["ticket_id", "ticket_revision_id", "ticket_revision", "state", "evidence", "attempts"],
        expectedOperation,
      );
      responseInteger(response, "ticket_id", expectedOperation);
      responseInteger(response, "ticket_revision_id", expectedOperation);
      responseInteger(response, "ticket_revision", expectedOperation);
      responseString(response, "state", expectedOperation);
      break;
    case "candidate_show":
      requiredResponseFields(
        response,
        [
          "candidate_id",
          "candidate_revision",
          "state",
          "ticket_attempt_id",
          "evidence",
          "delivery",
        ],
        expectedOperation,
      );
      responseInteger(response, "candidate_id", expectedOperation);
      responseInteger(response, "candidate_revision", expectedOperation);
      responseInteger(response, "ticket_attempt_id", expectedOperation);
      responseString(response, "state", expectedOperation);
      if (
        response.delivery !== null &&
        (typeof response.delivery !== "object" || Array.isArray(response.delivery))
      ) {
        throw new FrameProtocolError(
          "invalid_json",
          `response for ${expectedOperation} has invalid delivery`,
        );
      }
      if (response.delivery !== null) {
        const delivery = response.delivery as Record<string, unknown>;
        requiredResponseFields(
          delivery,
          ["delivery_id", "resulting_commit", "factory_cost_micro_usd"],
          expectedOperation,
        );
        responseInteger(delivery, "delivery_id", expectedOperation);
        responseString(delivery, "resulting_commit", expectedOperation);
        responseInteger(delivery, "factory_cost_micro_usd", expectedOperation);
      }
      break;
    case "audit_show":
      requiredResponseFields(response, ["selector", "items"], expectedOperation);
      responseString(response, "selector", expectedOperation);
      if (!Array.isArray(response.items)) {
        throw new FrameProtocolError(
          "invalid_json",
          `response for ${expectedOperation} requires audit array items`,
        );
      }
      break;
  }
}

function validateNullableDownstreamEvidence(
  value: unknown,
  required: readonly string[],
  integerFields: readonly string[],
  stringFields: readonly string[],
  operation: string,
): void {
  if (value === null) return;
  if (typeof value !== "object" || Array.isArray(value)) {
    throw new FrameProtocolError("invalid_json", `response for ${operation} has invalid evidence`);
  }
  const row = value as Record<string, unknown>;
  requiredResponseFields(row, required, operation);
  for (const field of integerFields) responseInteger(row, field, operation);
  for (const field of stringFields) responseString(row, field, operation);
}

function validateRequestId(value: string): void {
  if (
    typeof value !== "string" || value.length === 0 || value.length > 160 || value.includes("\0")
  ) {
    throw new FrameProtocolError("invalid_json", "request_id must be a nonempty bounded string");
  }
}

export interface RoutingEnvelope {
  readonly protocol_version: typeof PROTOCOL_VERSION_V1;
  readonly request_id: string;
  readonly operation: OperationName;
}

export interface FrameTransport {
  exchange(frame: Uint8Array): Promise<Uint8Array>;
}

export interface MutatingCall {
  readonly client_command_id: string;
  readonly expected_revision: number;
}

export interface WorkspaceReadCall {
  readonly repository_relative_path: string;
}

export interface WorkspaceReadResponse {
  readonly protocol_version: number;
  readonly request_id: string;
  readonly operation: typeof OPERATION.workspaceRead;
  readonly canonical_path: string;
  readonly blake3: string;
  readonly byte_length: number;
  readonly content_base64: string;
}

export interface ArtifactSealWorkspaceFileCall extends MutatingCall {
  readonly workspace_relative_path: string;
  readonly byte_limit: number;
}
export interface ArtifactReadCall {
  readonly artifact_id: number;
  readonly expected_digest: string;
}

/** A bounded reference to a daemon-adopted artifact, never inline bytes. */
export interface SealedArtifactReferenceV1 {
  readonly artifact_id: number;
  readonly digest: string;
  readonly byte_length: number;
}

export interface CommandObservationV1 {
  readonly exit_status: number;
  readonly stdout: SealedArtifactReferenceV1;
  readonly stderr: SealedArtifactReferenceV1;
}

/** The closed exact-observation comparison rule implemented by V1. */
export const EXACT_OBSERVATION_COMPARISON_V1 = 1 as const;

export interface TwoRunReproducerV1 {
  readonly comparison_rule_version: typeof EXACT_OBSERVATION_COMPARISON_V1;
  readonly command: SealedArtifactReferenceV1;
  /** Exact optional stdin; null means the child receives immediate EOF. */
  readonly stdin: SealedArtifactReferenceV1 | null;
  readonly expected_observation: CommandObservationV1;
  readonly first_observation: CommandObservationV1;
  readonly second_observation: CommandObservationV1;
}

export interface TicketContractReadV1 {
  readonly path: string;
  readonly reason: string;
}

/** Input the kernel uses to search the live ticket buffer before admission. */
export interface DuplicateSearchInputV1 {
  readonly query: string;
  readonly limit: number;
}

/**
 * The whole Product proposal is closed, generic data. Large narrative,
 * evidence, process-command, and observation bodies are sealed artifacts.
 * There is no sponsorship field: only the external Architect sponsors a
 * proposed ticket after this repeatable actor operation succeeds.
 */
export interface ProductTicketProposalV1 {
  readonly title: string;
  readonly mission_value: string;
  readonly scope: string;
  readonly contract_owner: string;
  readonly risk: string;
  readonly narrative: SealedArtifactReferenceV1;
  readonly evidence: SealedArtifactReferenceV1;
  readonly acceptance_criteria: readonly string[];
  readonly contract_reads: readonly TicketContractReadV1[];
  readonly duplicate_search: DuplicateSearchInputV1;
  readonly reproducer_profile: string;
  readonly reproducer: TwoRunReproducerV1;
}

export interface ProductSubmitTicketCall extends MutatingCall, ProductTicketProposalV1 {
}
export interface CandidateCheckpointRegressionCall extends MutatingCall {
  readonly regression_command: string;
  readonly expected_failure: string;
}
export interface CandidateSubmitCall extends MutatingCall {
  readonly commit_subject: string;
  readonly commit_body: string;
  readonly regression_test_identity: string;
}
export interface QualityRunFullSuiteCall extends MutatingCall {
  readonly validation_profile: string;
}
export interface QualitySubmitReviewCall extends MutatingCall {
  readonly full_suite_validation_id: number;
  readonly verdict: "accept" | "reject";
  readonly rationale: SealedArtifactReferenceV1;
  readonly risks: SealedArtifactReferenceV1;
  readonly additional_probes: SealedArtifactReferenceV1;
}
export interface WorkCompleteCall extends MutatingCall {
  readonly result_artifact_id: number;
}
/** These calls are valid only on an external operator transport, never an actor host. */
export interface ArchitectSponsorTicketRevisionCall extends MutatingCall {
  readonly ticket_revision_id: number;
  readonly rationale: SealedArtifactReferenceV1;
  readonly principal: string;
}
export interface ArchitectReleaseTicketAttemptCall extends MutatingCall {
  readonly ticket_attempt_id: number;
  readonly rationale: SealedArtifactReferenceV1;
  readonly principal: string;
}
export interface ArchitectDecideCandidateCall extends MutatingCall {
  readonly candidate_id: number;
  readonly review_id: number;
  readonly decision: "deliver" | "rework" | "reject";
  readonly rationale: SealedArtifactReferenceV1;
  /** Explicit rejected-review relation; never a Boolean hard-gate override. */
  readonly quality_rejection_override_review_id: number | null;
  readonly principal: string;
}
/** Read-only generic application projection on the operator connection. */
export interface OperatorApplicationShowCall {
  readonly application_key: string;
  readonly application_revision_id: number | null;
}
/** Paths are admitted only by daemon-owned Rust/CAS; callers send no bytes. */
export interface OperatorApplicationRegisterCall extends MutatingCall {
  readonly expected_kernel_build_revision: number;
  readonly kernel_build_id: string;
  readonly source_root: string;
  readonly bundle_relative_path: string;
  readonly principal: string;
}
/** Grand Architect activation of one exact inert application revision. */
export interface OperatorApplicationActivateCall extends MutatingCall {
  readonly application_key: string;
  readonly application_revision_id: number;
  readonly rationale: SealedArtifactReferenceV1;
  readonly principal: string;
}
/** Operator-only bounded CAS adoption. The local daemon reads the named file;
 * no evidence bytes cross this protocol. */
export interface OperatorArtifactSealCall {
  readonly client_command_id: string;
  readonly expected_kernel_build_revision: number;
  readonly source_root: string;
  readonly source_relative_path: string;
  readonly principal: string;
}
/** Transport-owned liveness probe; it has no durable side effect. */
/** The status probe intentionally accepts no caller-selected fields. */
export type FactorydStatusCall = Record<string, never>;
/** Start pins the selected active application revision, its repository, and
 * the current installed build inside PostgreSQL. Callers never choose those
 * derived identities. */
export interface OperatorCampaignStartCall {
  readonly client_command_id: string;
  readonly expected_application_revision: number;
  readonly application_revision_id: number;
  readonly aggregate_budget_micro_usd: number;
  readonly deadline_unix_millis: number;
  readonly delivery_target: number;
  readonly principal: string;
}
export interface OperatorCampaignStatusCall {
  readonly campaign_id: number;
}
export interface OperatorCampaignCancelCall extends MutatingCall {
  readonly campaign_id: number;
  readonly principal: string;
}
export type TicketLifecycle =
  | "proposed"
  | "sponsored"
  | "in_flight"
  | "delivered"
  | "blocked"
  | "resolved"
  | "superseded"
  | "rejected";
export interface OperatorTicketListCall {
  readonly state: TicketLifecycle | null;
}
export interface OperatorTicketShowCall {
  readonly ticket_id: number;
}
export interface OperatorCandidateShowCall {
  readonly candidate_id: number;
}
/** One closed audit subject selector, never a SQL or free-text query. */
export interface OperatorAuditShowCall {
  readonly selector: string;
}
export interface SessionVerifyPacketCall {
  readonly packet_digest: string;
  readonly packet_bytes_b64: string;
}
export interface SessionSealArtifactCall extends MutatingCall {
  readonly staging_relative_path: string;
  readonly role: "pi_transcript_gzip" | "required_read_manifest";
  readonly byte_limit: number;
}
export interface SessionSubmitTerminalCall extends MutatingCall {
  readonly terminal_operation: string | null;
  readonly terminal_payload_b64: string;
  readonly transcript_artifact_id: number;
  readonly input_tokens: number;
  readonly output_tokens: number;
  readonly cache_read_tokens: number;
  readonly cache_write_tokens: number;
  readonly reasoning_tokens: number | null;
  readonly reported_cost_micro_usd: number | null;
  readonly stop_reason: string;
}
export interface ArtifactReceiptResponse {
  readonly protocol_version: number;
  readonly request_id: string;
  readonly operation: string;
  readonly artifact_id: number;
  readonly digest: string;
  readonly byte_length: number;
  readonly aggregate_revision: number;
}

export interface OperationReceiptResponse {
  readonly protocol_version: number;
  readonly request_id: string;
  readonly operation: string;
  readonly audit_id: number;
  readonly aggregate_revision: number;
}

/** Kernel-owned candidate capture returned after Engineering submission.
 * The candidate commit is attached only after terminal transcript custody. */
export interface CandidateReceiptResponse extends OperationReceiptResponse {
  readonly operation: typeof OPERATION.candidateSubmit;
  readonly candidate_id: number;
  readonly validation_id: number;
  readonly candidate_tree: string;
}

/** Evidence navigation returned after the daemon accepts the one retained
 * Engineering regression checkpoint. It is deliberately not an audit receipt
 * and cannot recreate the kernel's opaque checkpoint capability. */
export interface RegressionCheckpointReceiptResponse {
  readonly protocol_version: number;
  readonly request_id: string;
  readonly operation: typeof OPERATION.candidateCheckpointRegression;
  readonly regression_tree: string;
  readonly regression_patch_artifact_id: number;
  readonly regression_command_set_artifact_id: number;
  readonly regression_log_artifact_id: number;
}

/** The Quality-owned full-suite receipt required by a later review. */
export interface QualityValidationReceiptResponse extends OperationReceiptResponse {
  readonly operation: typeof OPERATION.qualityRunFullSuite;
  readonly validation_id: number;
  readonly candidate_id: number;
  readonly candidate_tree: string;
}

/** Immutable Quality review receipt for the external Architect relation. */
export interface QualityReviewReceiptResponse extends OperationReceiptResponse {
  readonly operation: typeof OPERATION.qualitySubmitReview;
  readonly review_id: number;
  readonly candidate_id: number;
  readonly verdict: "accept" | "reject";
}

export interface ArchitectDecisionReceiptResponse extends OperationReceiptResponse {
  readonly operation:
    | typeof OPERATION.architectSponsorTicketRevision
    | typeof OPERATION.architectReleaseTicketAttempt
    | typeof OPERATION.architectDecideCandidate;
  readonly architect_decision_id: number;
  readonly decision_kind: "sponsor" | "release" | "deliver" | "rework" | "reject";
}

export interface ApplicationShowResponse {
  readonly protocol_version: number;
  readonly request_id: string;
  readonly operation: typeof OPERATION.operatorApplicationShow;
  readonly application_key: string;
  readonly application_revision_id: number;
  readonly aggregate_revision: number;
  readonly bundle_artifact_id: number;
  readonly is_active: boolean;
}

export interface ApplicationRevisionReceiptResponse extends OperationReceiptResponse {
  readonly operation:
    | typeof OPERATION.operatorApplicationRegister
    | typeof OPERATION.operatorApplicationActivate;
  readonly application_revision_id: number;
  readonly is_active: boolean;
  readonly was_idempotent_retry: boolean;
}

export interface OperatorArtifactSealReceiptResponse extends OperationReceiptResponse {
  readonly operation: typeof OPERATION.operatorArtifactSeal;
  readonly artifact_id: number;
  readonly digest: string;
  readonly byte_length: number;
  readonly was_idempotent_retry: boolean;
  readonly was_reused: boolean;
}

/** Read-only daemon liveness and current kernel-build command guard. */
export interface FactorydStatusResponse {
  readonly protocol_version: number;
  readonly request_id: string;
  readonly operation: typeof OPERATION.factorydStatus;
  readonly state: "ready";
  readonly current_kernel_build_id: string | null;
  readonly aggregate_revision: number;
}

/** Durable receipt for campaign admission or cancellation, including every
 * trusted identity PostgreSQL pinned or retained. */
export interface CampaignReceiptResponse extends OperationReceiptResponse {
  readonly operation:
    | typeof OPERATION.operatorCampaignStart
    | typeof OPERATION.operatorCampaignCancel;
  readonly campaign_id: number;
  readonly kernel_build_id: string;
  readonly application_revision_id: number;
  readonly repository_id: number;
  readonly was_idempotent_retry: boolean;
}

export type CampaignMeasuredCostState = "known" | "unknown" | "exceeded";
export type DownstreamActionStage =
  | "hard_validation"
  | "quality"
  | "candidate_commit_attach_required"
  | "quality_review_required"
  | "awaiting_architect"
  | "deliver_accepted"
  | "rework_engineering"
  | "rework_validation"
  | "rework_quality";

/** One bounded zero-write campaign/buffer/scheduler status projection. */
export interface CampaignStatusResponse {
  readonly protocol_version: number;
  readonly request_id: string;
  readonly operation: typeof OPERATION.operatorCampaignStatus;
  readonly campaign_id: number;
  readonly state: "running" | "completed" | "failed" | "cancelled";
  readonly aggregate_revision: number;
  readonly kernel_build_id: string;
  readonly application_revision_id: number;
  readonly repository_id: number;
  readonly aggregate_budget_micro_usd: number;
  readonly measured_cost_state: CampaignMeasuredCostState;
  readonly measured_cost_micro_usd: number | null;
  readonly remaining_budget_micro_usd: number | null;
  readonly deadline_unix_millis: number;
  readonly delivery_target: number;
  readonly base_commit: string | null;
  readonly candidate_tree: string | null;
  readonly candidate_commit: string | null;
  readonly delivered_commit: string | null;
  readonly delivered_factory_cost_micro_usd: number | null;
  readonly delivered_attempt_count: number;
  readonly ready_ticket_count: number;
  readonly proposed_ticket_count: number;
  readonly in_flight_ticket_count: number;
  readonly downstream_ticket_attempt_count: number;
  readonly downstream_action_stage: DownstreamActionStage | null;
  readonly downstream_ticket_attempt_id: number | null;
  readonly downstream_ticket_attempt_revision: number | null;
  readonly downstream_candidate_id: number | null;
  readonly downstream_candidate_revision: number | null;
  readonly downstream_evidence: DownstreamEvidenceResponse | null;
  readonly ready_low_water: number;
  readonly ready_target: number;
  readonly ready_maximum: number;
  readonly proposal_maximum: number;
  readonly oldest_sponsored_ticket_revision_id: number | null;
  readonly oldest_sponsored_ticket_revision: number | null;
  readonly scheduler_next_action: string;
  readonly scheduler_constraint: string | null;
  /** At most twenty exact session cost/outcome rows, ordered by session ID. */
  readonly session_costs: readonly CampaignSessionCostResponse[];
  /** Complete all-session grouping; at most three offices by six outcomes. */
  readonly session_cost_aggregates: readonly CampaignSessionCostAggregateResponse[];
}

export interface CampaignSessionCostResponse {
  readonly session_id: number;
  readonly assignment_id: number;
  readonly assignment_role: "product_research" | "engineering" | "quality";
  readonly model_provider: string;
  readonly model_id: string;
  readonly outcome:
    | "prepared"
    | "running"
    | "succeeded"
    | "failed"
    | "cancelled"
    | "interrupted";
  readonly cost_state: "pending" | "known" | "unknown" | "exceeded";
  readonly cost_micro_usd: number | null;
  /** Present only for the current running session. */
  readonly elapsed_millis: number | null;
}

export interface CampaignSessionCostAggregateResponse {
  readonly assignment_role: "product_research" | "engineering" | "quality";
  readonly model_provider: string;
  readonly model_id: string;
  readonly outcome:
    | "prepared"
    | "running"
    | "succeeded"
    | "failed"
    | "cancelled"
    | "interrupted";
  readonly session_count: number;
  /** Sum of every persisted numeric cost, including exceeded amounts. */
  readonly accounted_cost_micro_usd: number;
  readonly pending_cost_session_count: number;
  readonly unknown_cost_session_count: number;
  readonly exceeded_cost_session_count: number;
}

/** Immutable evidence already attached to the exact downstream candidate. */
export interface DownstreamEvidenceResponse {
  readonly candidate_commit: string | null;
  readonly latest_validation: DownstreamValidationEvidenceResponse | null;
  readonly review: DownstreamReviewEvidenceResponse | null;
  readonly architect_decision: DownstreamArchitectDecisionEvidenceResponse | null;
}

export interface DownstreamValidationEvidenceResponse {
  readonly validation_id: number;
  readonly state: "passed" | "failed" | "interrupted";
  readonly log_artifact_id: number;
}

export interface DownstreamReviewEvidenceResponse {
  readonly review_id: number;
  readonly review_revision: number;
  readonly verdict: "accept" | "reject";
  readonly rationale_artifact_id: number;
}

export interface DownstreamArchitectDecisionEvidenceResponse {
  readonly architect_decision_id: number;
  readonly decision_kind: "deliver" | "rework" | "reject";
  readonly rationale_artifact_id: number;
}

export interface EvidenceArtifactResponse {
  readonly role: string;
  readonly artifact_id: number;
  readonly digest: string;
  readonly byte_length: number;
}

export interface TicketListItemResponse {
  readonly ticket_id: number;
  readonly ticket_revision_id: number;
  readonly ticket_revision: number;
  readonly application_revision_id: number;
  readonly state: TicketLifecycle;
  readonly proposal_artifact_id: number;
  readonly created_at_micros: number;
}

export interface TicketListResponse {
  readonly protocol_version: number;
  readonly request_id: string;
  readonly operation: typeof OPERATION.operatorTicketList;
  readonly items: readonly TicketListItemResponse[];
}

export interface TicketAttemptNavigationResponse {
  readonly ticket_attempt_id: number;
  readonly attempt_revision: number;
  readonly campaign_id: number;
  readonly stage: string;
  readonly candidate_id: number | null;
}

export interface TicketShowResponse {
  readonly protocol_version: number;
  readonly request_id: string;
  readonly operation: typeof OPERATION.operatorTicketShow;
  readonly ticket_id: number;
  readonly ticket_revision_id: number;
  readonly ticket_revision: number;
  readonly application_revision_id: number;
  readonly state: TicketLifecycle;
  readonly sponsorship_reason: string | null;
  readonly blocked_reason: string | null;
  readonly evidence: readonly EvidenceArtifactResponse[];
  readonly attempts: readonly TicketAttemptNavigationResponse[];
}

export interface CandidateValidationNavigationResponse {
  readonly validation_id: number;
  readonly scope: string;
  readonly state: string;
  readonly log: EvidenceArtifactResponse;
}

export interface CandidateReviewNavigationResponse {
  readonly review_id: number;
  readonly review_revision: number;
  readonly verdict: "accept" | "reject";
  readonly rationale: EvidenceArtifactResponse;
  readonly risks: EvidenceArtifactResponse;
}

export interface CandidateDecisionNavigationResponse {
  readonly architect_decision_id: number;
  readonly decision_kind: "deliver" | "rework" | "reject";
  readonly rationale: EvidenceArtifactResponse;
}

export interface DeliveryNavigationResponse {
  readonly delivery_id: number;
  readonly resulting_commit: string;
  readonly factory_cost_micro_usd: number;
}

export interface CandidateShowResponse {
  readonly protocol_version: number;
  readonly request_id: string;
  readonly operation: typeof OPERATION.operatorCandidateShow;
  readonly candidate_id: number;
  readonly candidate_revision: number;
  readonly state: string;
  readonly ticket_attempt_id: number;
  readonly ticket_revision_id: number;
  readonly ticket_revision: number;
  readonly base_commit: string;
  readonly candidate_tree: string;
  readonly candidate_commit: string | null;
  readonly evidence: readonly EvidenceArtifactResponse[];
  readonly validations: readonly CandidateValidationNavigationResponse[];
  readonly review: CandidateReviewNavigationResponse | null;
  readonly latest_architect_decision: CandidateDecisionNavigationResponse | null;
  readonly delivery_receipt: EvidenceArtifactResponse | null;
  readonly delivery: DeliveryNavigationResponse | null;
}

export interface AuditEntryResponse {
  readonly audit_id: number;
  readonly principal: string;
  readonly operation: string;
  readonly subject_kind: number;
  readonly subject_id: number;
  readonly aggregate_revision: number;
}

export interface AuditShowResponse {
  readonly protocol_version: number;
  readonly request_id: string;
  readonly operation: typeof OPERATION.operatorAuditShow;
  readonly selector: string;
  readonly items: readonly AuditEntryResponse[];
}

export interface SessionPacketVerificationResponse {
  readonly protocol_version: number;
  readonly request_id: string;
  readonly operation: typeof OPERATION.sessionVerifyPacket;
  readonly packet_digest: string;
  readonly verified: boolean;
}

/** Responses which contain an artifact identity, never artifact bytes. */
export interface ArtifactReadResponse {
  readonly protocol_version: number;
  readonly request_id: string;
  readonly operation: typeof OPERATION.artifactRead;
  readonly artifact_id: number;
  readonly digest: string;
  readonly byte_length: number;
  readonly content_base64: string;
}

export interface ConflictResponse {
  readonly protocol_version: number;
  readonly request_id: string;
  readonly operation: string;
  readonly error_code: string;
  readonly current_revision: number;
  readonly message: string;
}

export interface ErrorResponse {
  readonly protocol_version: number;
  readonly request_id: string;
  readonly operation: string;
  readonly error_code: string;
  readonly message: string;
}

export interface ProtocolClientOptions {
  readonly requestId?: () => string;
}

/**
 * Typed SDK facade for the local protocol. `request_id` is connection-local;
 * mutating retry identity belongs to each call's `client_command_id`.
 */
export class LocalProtocolClient {
  readonly #transport: FrameTransport;
  readonly #requestId: () => string;

  constructor(transport: FrameTransport, options: ProtocolClientOptions = {}) {
    this.#transport = transport;
    let nextRequestId = 0;
    this.#requestId = options.requestId ?? (() => `request-${++nextRequestId}`);
  }

  async workspaceRead(call: WorkspaceReadCall): Promise<WorkspaceReadResponse> {
    return await this.#read<WorkspaceReadResponse>(OPERATION.workspaceRead, call);
  }

  async artifactSealWorkspaceFile(
    call: ArtifactSealWorkspaceFileCall,
  ): Promise<ArtifactReceiptResponse> {
    return await this.#mutate<ArtifactReceiptResponse>(OPERATION.artifactSealWorkspaceFile, call);
  }

  async artifactRead(call: ArtifactReadCall): Promise<ArtifactReadResponse> {
    return await this.#read<ArtifactReadResponse>(OPERATION.artifactRead, call);
  }

  async productSubmitTicket(call: ProductSubmitTicketCall): Promise<OperationReceiptResponse> {
    return await this.#mutate(OPERATION.productSubmitTicket, call);
  }

  async candidateCheckpointRegression(
    call: CandidateCheckpointRegressionCall,
  ): Promise<RegressionCheckpointReceiptResponse> {
    return await this.#mutate(OPERATION.candidateCheckpointRegression, call);
  }

  async candidateSubmit(call: CandidateSubmitCall): Promise<CandidateReceiptResponse> {
    return await this.#mutate(OPERATION.candidateSubmit, call);
  }

  async qualityRunFullSuite(
    call: QualityRunFullSuiteCall,
  ): Promise<QualityValidationReceiptResponse> {
    return await this.#mutate(OPERATION.qualityRunFullSuite, call);
  }

  async qualitySubmitReview(call: QualitySubmitReviewCall): Promise<QualityReviewReceiptResponse> {
    return await this.#mutate(OPERATION.qualitySubmitReview, call);
  }

  async workComplete(call: WorkCompleteCall): Promise<OperationReceiptResponse> {
    return await this.#mutate(OPERATION.workComplete, call);
  }

  async architectSponsorTicketRevision(
    call: ArchitectSponsorTicketRevisionCall,
  ): Promise<ArchitectDecisionReceiptResponse> {
    return await this.#mutate(OPERATION.architectSponsorTicketRevision, call);
  }

  async architectReleaseTicketAttempt(
    call: ArchitectReleaseTicketAttemptCall,
  ): Promise<ArchitectDecisionReceiptResponse> {
    return await this.#mutate(OPERATION.architectReleaseTicketAttempt, call);
  }

  async architectDecideCandidate(
    call: ArchitectDecideCandidateCall,
  ): Promise<ArchitectDecisionReceiptResponse> {
    return await this.#mutate(OPERATION.architectDecideCandidate, call);
  }

  async operatorApplicationShow(
    call: OperatorApplicationShowCall,
  ): Promise<ApplicationShowResponse> {
    return await this.#read(OPERATION.operatorApplicationShow, call);
  }

  async operatorApplicationRegister(
    call: OperatorApplicationRegisterCall,
  ): Promise<ApplicationRevisionReceiptResponse> {
    return await this.#mutate(OPERATION.operatorApplicationRegister, call);
  }

  async operatorApplicationActivate(
    call: OperatorApplicationActivateCall,
  ): Promise<ApplicationRevisionReceiptResponse> {
    return await this.#mutate(OPERATION.operatorApplicationActivate, call);
  }

  async operatorArtifactSeal(
    call: OperatorArtifactSealCall,
  ): Promise<OperatorArtifactSealReceiptResponse> {
    return await this.#mutate(OPERATION.operatorArtifactSeal, call);
  }

  async factorydStatus(call: FactorydStatusCall = {}): Promise<FactorydStatusResponse> {
    return await this.#read(OPERATION.factorydStatus, call);
  }

  async operatorCampaignStart(
    call: OperatorCampaignStartCall,
  ): Promise<CampaignReceiptResponse> {
    return await this.#mutate(OPERATION.operatorCampaignStart, call);
  }

  async operatorCampaignStatus(
    call: OperatorCampaignStatusCall,
  ): Promise<CampaignStatusResponse> {
    return await this.#read(OPERATION.operatorCampaignStatus, call);
  }

  async operatorCampaignCancel(
    call: OperatorCampaignCancelCall,
  ): Promise<CampaignReceiptResponse> {
    return await this.#mutate(OPERATION.operatorCampaignCancel, call);
  }

  async operatorTicketList(call: OperatorTicketListCall): Promise<TicketListResponse> {
    return await this.#read(OPERATION.operatorTicketList, call);
  }

  async operatorTicketShow(call: OperatorTicketShowCall): Promise<TicketShowResponse> {
    return await this.#read(OPERATION.operatorTicketShow, call);
  }

  async operatorCandidateShow(call: OperatorCandidateShowCall): Promise<CandidateShowResponse> {
    return await this.#read(OPERATION.operatorCandidateShow, call);
  }

  async operatorAuditShow(call: OperatorAuditShowCall): Promise<AuditShowResponse> {
    return await this.#read(OPERATION.operatorAuditShow, call);
  }

  async sessionVerifyPacket(
    call: SessionVerifyPacketCall,
  ): Promise<SessionPacketVerificationResponse> {
    return await this.#read<SessionPacketVerificationResponse>(OPERATION.sessionVerifyPacket, call);
  }

  async sessionSealArtifact(call: SessionSealArtifactCall): Promise<ArtifactReceiptResponse> {
    return await this.#mutate<ArtifactReceiptResponse>(OPERATION.sessionSealArtifact, call);
  }

  async sessionSubmitTerminal(call: SessionSubmitTerminalCall): Promise<OperationReceiptResponse> {
    return await this.#mutate<OperationReceiptResponse>(OPERATION.sessionSubmitTerminal, call);
  }

  async #mutate<R, T = unknown>(operation: OperationName, payload: T): Promise<R> {
    return await this.#exchange<R, T>(operation, payload);
  }

  async #read<R, T = unknown>(operation: OperationName, payload: T): Promise<R> {
    return await this.#exchange<R, T>(operation, payload);
  }

  async #exchange<R, T>(operation: OperationName, payload: T): Promise<R> {
    const requestId = this.#requestId();
    validateRequestId(requestId);
    const request: RoutingEnvelope & Record<string, unknown> = {
      protocol_version: PROTOCOL_VERSION_V1,
      request_id: requestId,
      operation,
      ...payload as Record<string, unknown>,
    };
    const responseFrame = await this.#transport.exchange(
      encodeJsonFrame(request, REQUEST_FRAME_MAX_BYTES),
    );
    const response = decodeJsonFrame<
      | ArtifactReceiptResponse
      | OperationReceiptResponse
      | RegressionCheckpointReceiptResponse
      | CandidateReceiptResponse
      | QualityValidationReceiptResponse
      | QualityReviewReceiptResponse
      | ArchitectDecisionReceiptResponse
      | ApplicationShowResponse
      | ApplicationRevisionReceiptResponse
      | OperatorArtifactSealReceiptResponse
      | FactorydStatusResponse
      | CampaignReceiptResponse
      | CampaignStatusResponse
      | TicketListResponse
      | TicketShowResponse
      | CandidateShowResponse
      | AuditShowResponse
      | ConflictResponse
      | ErrorResponse
    >(responseFrame, operation, RESPONSE_FRAME_MAX_BYTES);
    validateProtocolResponse(
      response,
      operation,
      requestId,
      operation === OPERATION.artifactSealWorkspaceFile
        ? "artifact"
        : operation === OPERATION.workspaceRead
        ? "workspace_read"
        : operation === OPERATION.artifactRead
        ? "artifact_read"
        : operation === OPERATION.sessionVerifyPacket
        ? "packet_verification"
        : operation === OPERATION.candidateCheckpointRegression
        ? "regression_checkpoint"
        : operation === OPERATION.candidateSubmit
        ? "candidate"
        : operation === OPERATION.qualityRunFullSuite
        ? "quality_validation"
        : operation === OPERATION.qualitySubmitReview
        ? "quality_review"
        : operation === OPERATION.architectSponsorTicketRevision ||
            operation === OPERATION.architectReleaseTicketAttempt ||
            operation === OPERATION.architectDecideCandidate
        ? "architect_decision"
        : operation === OPERATION.operatorApplicationShow
        ? "application_show"
        : operation === OPERATION.operatorApplicationRegister ||
            operation === OPERATION.operatorApplicationActivate
        ? "application_revision"
        : operation === OPERATION.operatorArtifactSeal
        ? "operator_artifact"
        : operation === OPERATION.factorydStatus
        ? "daemon_status"
        : operation === OPERATION.operatorCampaignStart ||
            operation === OPERATION.operatorCampaignCancel
        ? "campaign_receipt"
        : operation === OPERATION.operatorCampaignStatus
        ? "campaign_status"
        : operation === OPERATION.operatorTicketList
        ? "ticket_list"
        : operation === OPERATION.operatorTicketShow
        ? "ticket_show"
        : operation === OPERATION.operatorCandidateShow
        ? "candidate_show"
        : operation === OPERATION.operatorAuditShow
        ? "audit_show"
        : "receipt",
    );
    if ("error_code" in response) {
      if (
        response.error_code === "revision_conflict" ||
        response.error_code === "idempotency_conflict"
      ) {
        throw new ProtocolCommandError(response as ConflictResponse);
      }
      throw new ProtocolCommandError(response as ErrorResponse);
    }
    return response as R;
  }
}

export class ProtocolCommandError extends Error {
  readonly response: ConflictResponse | ErrorResponse;

  constructor(response: ConflictResponse | ErrorResponse) {
    super(`${response.error_code}: ${response.message}`);
    this.name = "ProtocolCommandError";
    this.response = response;
  }
}
