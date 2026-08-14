import {
  decodeJsonFrame,
  encodeJsonFrame,
  FrameProtocolError,
  type FrameTransport,
  REQUEST_FRAME_MAX_BYTES,
  RESPONSE_FRAME_MAX_BYTES,
} from "../factory-sdk/protocol.ts";
import { PRODUCT_SUBMIT_TICKET_INPUT_SCHEMA_V1 } from "../factory-sdk/product.ts";
import {
  CANDIDATE_CHECKPOINT_REGRESSION_INPUT_SCHEMA_V1,
  CANDIDATE_SUBMIT_INPUT_SCHEMA_V1,
} from "../factory-sdk/candidate.ts";
import {
  QUALITY_RUN_FULL_SUITE_INPUT_SCHEMA_V1,
  QUALITY_SUBMIT_REVIEW_INPUT_SCHEMA_V1,
} from "../factory-sdk/quality.ts";
import type {
  HostToolName,
  PiToolAdapter,
  RequiredReadObservation,
  RequiredReadVerifier,
} from "./types.ts";

/** Reads and writes the daemon's already-connected big-endian length frames. */
export class InheritedFrameTransport implements FrameTransport {
  readonly #readFile: Deno.FsFile;
  readonly #writeFile: Deno.FsFile;
  #tail = Promise.resolve();
  #reader: Promise<void> | undefined;
  #waiting: Array<{
    resolve: (frame: Uint8Array) => void;
    reject: (error: unknown) => void;
  }> = [];
  #lossListeners = new Set<() => void>();
  #loss: unknown | undefined;

  constructor(readFile: Deno.FsFile, writeFile: Deno.FsFile = readFile) {
    this.#readFile = readFile;
    this.#writeFile = writeFile;
  }

  exchange(frame: Uint8Array): Promise<Uint8Array> {
    const exchange = this.#tail.then(async () => {
      if (this.#loss !== undefined) throw this.#loss;
      const response = new Promise<Uint8Array>((resolve, reject) => {
        this.#waiting.push({ resolve, reject });
      });
      try {
        await writeAll(this.#writeFile, frame);
        // Deno serializes operations on one FsFile resource. Starting the
        // blocking response read first prevents the request write on an
        // inherited full-duplex socket and deadlocks both peers. A socket
        // buffers the response, so enqueue the waiter, write the request, and
        // only then start the sole response reader.
        this.#reader ??= this.#readResponses();
      } catch (error) {
        // `#markLost` rejects every queued response. Observe this response's
        // rejection here because the write error itself is the exchange's
        // primary failure.
        void response.catch(() => undefined);
        this.#markLost(error);
        throw error;
      }
      return await response;
    });
    this.#tail = exchange.then(() => undefined, () => undefined);
    return exchange;
  }

  /** True until the full-duplex reader observes EOF or an I/O failure. */
  isAlive(): boolean {
    return this.#loss === undefined;
  }

  /** Registers the Pi-abort signal backed by actual stream EOF/write failure. */
  onLoss(listener: () => void): () => void {
    if (this.#loss !== undefined) queueMicrotask(listener);
    else this.#lossListeners.add(listener);
    return () => this.#lossListeners.delete(listener);
  }

  async #readResponses(): Promise<void> {
    try {
      while (true) {
        const frame = await readFrame(this.#readFile);
        const waiter = this.#waiting.shift();
        if (waiter === undefined) {
          throw new FrameProtocolError(
            "wrong_operation",
            "daemon sent an unsolicited actor response",
          );
        }
        waiter.resolve(frame);
      }
    } catch (error) {
      this.#markLost(error);
    }
  }

  #markLost(error: unknown): void {
    if (this.#loss !== undefined) return;
    this.#loss = error instanceof Error ? error : new Error(String(error));
    for (const waiter of this.#waiting.splice(0)) waiter.reject(this.#loss);
    for (const listener of this.#lossListeners) listener();
    this.#lossListeners.clear();
  }
}

/** Generic operation caller used by daemon-bound common and application tools. */
export class FramedActorClient {
  readonly #transport: FrameTransport;
  #nextRequestId = 0;

  constructor(transport: FrameTransport) {
    this.#transport = transport;
  }

  async call<T = unknown>(operation: string, payload: unknown): Promise<T> {
    const requestId = `host-request-${++this.#nextRequestId}`;
    const frame = encodeJsonFrame({
      protocol_version: 1,
      request_id: requestId,
      operation,
      ...(payload as Record<string, unknown>),
    }, REQUEST_FRAME_MAX_BYTES);
    const response = decodeJsonFrame<Record<string, unknown>>(
      await this.#transport.exchange(frame),
      operation,
      RESPONSE_FRAME_MAX_BYTES,
    );
    if (
      response.protocol_version !== 1 || response.request_id !== requestId ||
      response.operation !== operation
    ) {
      throw new FrameProtocolError(
        "wrong_operation",
        `response identity does not match ${operation}/${requestId}`,
      );
    }
    if (typeof response.error_code === "string") {
      throw new Error(`${response.error_code}: ${String(response.message ?? "daemon error")}`);
    }
    validateFramedSuccess(response, operation);
    return response as T;
  }
}

function validateFramedSuccess(response: Record<string, unknown>, operation: string): void {
  const requireString = (field: string): void => {
    if (typeof response[field] !== "string") {
      throw new FrameProtocolError(
        "invalid_json",
        `${operation} response requires string ${field}`,
      );
    }
  };
  const requireInteger = (field: string): void => {
    if (!Number.isSafeInteger(response[field]) || (response[field] as number) < 0) {
      throw new FrameProtocolError(
        "invalid_json",
        `${operation} response requires integer ${field}`,
      );
    }
  };
  const requireFields = (fields: readonly string[]): void => {
    for (const field of fields) {
      if (!(field in response)) {
        throw new FrameProtocolError("invalid_json", `${operation} response is missing ${field}`);
      }
    }
  };
  if (operation === "workspace.read") {
    requireFields(["canonical_path", "blake3", "byte_length", "content_base64"]);
    requireString("canonical_path");
    requireString("blake3");
    requireInteger("byte_length");
    requireString("content_base64");
  } else if (operation === "session.verify_packet") {
    requireFields(["packet_digest", "verified"]);
    requireString("packet_digest");
    if (response.verified !== true) {
      throw new FrameProtocolError(
        "invalid_json",
        "session.verify_packet response was not verified",
      );
    }
  } else if (
    operation === "session.seal_artifact" || operation === "artifact.seal_workspace_file"
  ) {
    requireFields(["artifact_id", "digest", "byte_length", "aggregate_revision"]);
    requireInteger("artifact_id");
    requireString("digest");
    requireInteger("byte_length");
    requireInteger("aggregate_revision");
  } else if (operation === "session.submit_terminal") {
    requireFields(["audit_id", "aggregate_revision"]);
    requireInteger("audit_id");
    requireInteger("aggregate_revision");
  } else if (operation === "candidate.checkpoint_regression") {
    requireFields([
      "regression_tree",
      "regression_patch_artifact_id",
      "regression_command_set_artifact_id",
      "regression_log_artifact_id",
    ]);
    requireString("regression_tree");
    requireInteger("regression_patch_artifact_id");
    requireInteger("regression_command_set_artifact_id");
    requireInteger("regression_log_artifact_id");
  } else if (operation === "candidate.submit") {
    requireFields([
      "audit_id",
      "aggregate_revision",
      "candidate_id",
      "validation_id",
      "candidate_tree",
    ]);
    requireInteger("audit_id");
    requireInteger("aggregate_revision");
    requireInteger("candidate_id");
    requireInteger("validation_id");
    requireString("candidate_tree");
  } else if (operation === "quality.run_full_suite") {
    requireFields([
      "audit_id",
      "aggregate_revision",
      "validation_id",
      "candidate_id",
      "candidate_tree",
    ]);
    requireInteger("audit_id");
    requireInteger("aggregate_revision");
    requireInteger("validation_id");
    requireInteger("candidate_id");
    requireString("candidate_tree");
  } else if (operation === "quality.submit_review") {
    requireFields(["audit_id", "aggregate_revision", "review_id", "candidate_id", "verdict"]);
    requireInteger("audit_id");
    requireInteger("aggregate_revision");
    requireInteger("review_id");
    requireInteger("candidate_id");
    if (response.verdict !== "accept" && response.verdict !== "reject") {
      throw new FrameProtocolError(
        "invalid_json",
        "quality.submit_review response has invalid verdict",
      );
    }
  } else if (operation === "publication.create") {
    requireFields(["audit_id", "aggregate_revision", "publication_id", "was_idempotent_retry"]);
    requireInteger("audit_id");
    requireInteger("aggregate_revision");
    requireInteger("publication_id");
    if (typeof response.was_idempotent_retry !== "boolean") {
      throw new FrameProtocolError(
        "invalid_json",
        "publication.create response requires an idempotency replay flag",
      );
    }
  }
}

const TOOL_OPERATIONS: Readonly<Partial<Record<HostToolName, string>>> = {
  workspace_read: "workspace.read",
  artifact_seal: "artifact.seal_workspace_file",
  artifact_read: "artifact.read",
  product_submit_ticket: "product.submit_ticket",
  candidate_checkpoint_regression: "candidate.checkpoint_regression",
  candidate_submit: "candidate.submit",
  quality_run_full_suite: "quality.run_full_suite",
  quality_submit_review: "quality.submit_review",
  work_complete: "work.complete",
  forum_search: "forum.search",
  forum_list_topics: "forum.list_topics",
  forum_list_threads: "forum.list_threads",
  forum_read_thread: "forum.read_thread",
  publication_create: "publication.create",
};

const TOOL_DESCRIPTIONS: Readonly<Partial<Record<HostToolName, string>>> = {
  workspace_read: "Read exact bytes from one path in the assigned workspace.",
  artifact_seal: "Seal one workspace file as immutable assignment evidence.",
  artifact_read: "Read one sealed upstream evidence item named in this assignment.",
  product_submit_ticket: "Submit one complete reproducible XSH defect proposal.",
  candidate_checkpoint_regression: "Capture and run the regression-only checkpoint.",
  candidate_submit: "Submit the completed XSH change for exact validation.",
  quality_run_full_suite: "Run the assigned full validation suite on the exact candidate.",
  quality_submit_review: "Submit the independent review of the exact candidate.",
  work_complete: "Complete this assignment without another proposal.",
  forum_search: "Search discussion history with bounded filters and continuation.",
  forum_list_topics: "List discussion topics by recent activity.",
  forum_list_threads: "List threads in one discussion topic by recent activity.",
  forum_read_thread: "Read a bounded chronological page of discussion posts.",
  publication_create:
    "Publish one immutable, anchored finding, question, challenge, correction, decision link, or note.",
};

const FORUM_POST_KINDS = [
  "Note",
  "Question",
  "Finding",
  "Proposal",
  "Challenge",
  "Correction",
  "DecisionLink",
] as const;

const EMPTY_INPUT_SCHEMA = { type: "object", additionalProperties: false } as const;

/**
 * Per-session transport facts minted by the daemon admission frame. These are
 * host bookkeeping, never model inputs: an actor should describe the work,
 * not guess an optimistic revision or manufacture an idempotency key.
 */
export interface FramedToolCommandContext {
  readonly session_revision: number;
  readonly next_command_id: () => number;
}

const defaultCommandContext: FramedToolCommandContext = {
  session_revision: 0,
  next_command_id: (() => {
    let next = 0;
    return () => ++next;
  })(),
};

const SESSION_MUTATING_TOOLS = new Set<HostToolName>([
  "artifact_seal",
  "product_submit_ticket",
  "candidate_checkpoint_regression",
  "candidate_submit",
  "quality_run_full_suite",
  "quality_submit_review",
  "publication_create",
]);

/** Converts admitted tool names into assigned operation-specific wrappers. */
export function createFramedToolAdapters(
  client: FramedActorClient,
  names: readonly HostToolName[],
  commandContext: FramedToolCommandContext = defaultCommandContext,
): readonly PiToolAdapter[] {
  return names.flatMap((name) => {
    const operation = TOOL_OPERATIONS[name];
    // These five common tools are the local Pi-headless runtime's host-local primitives;
    // they deliberately do not pretend a daemon operation exists.
    if (
      operation === undefined &&
      ["workspace_write", "workspace_edit", "workspace_search", "workspace_list", "shell"]
        .includes(name)
    ) return [];
    if (operation === undefined) throw new Error(`no framed operation for ${name}`);
    const description = TOOL_DESCRIPTIONS[name];
    if (description === undefined) throw new Error(`no model-visible description for ${name}`);
    return [{
      name,
      sdk_definition: {
        description,
        input_schema: modelToolInputSchema(name),
        invoke: async (input: unknown) => {
          try {
            const result = await client.call(
              operation,
              modelToolWireInput(name, input, commandContext),
            );
            return modelVisibleToolResult(name, result);
          } catch (error) {
            // Wire and authority diagnostics remain in host/kernel evidence.
            // A model sees only a task-level failure, never internal service
            // names, lifecycle identities, or transport wording.
            throw taskLevelToolFailure(name, error);
          }
        },
      },
    }];
  });
}

/**
 * Maps a closed, task-level correction that the actor can safely act on while
 * keeping all transport and authority details in the durable host evidence.
 */
function taskLevelToolFailure(name: HostToolName, error: unknown): Error {
  const detail = error instanceof Error ? error.message : "";
  if (
    name === "candidate_checkpoint_regression" &&
    detail.includes("all assigned exact reads are required before mutation")
  ) {
    return new Error(
      "Before retrying the checkpoint, use `workspace_read` (not shell commands) on every path " +
        "listed in the assignment's `Exact workspace reads required before any mutating tool` " +
        "section. Then retry the same checkpoint before editing the implementation.",
    );
  }
  if (name === "candidate_checkpoint_regression") {
    // A checkpoint is deliberately nonterminal, so its failure is a correction
    // opportunity rather than a reason to keep the engineer guessing. The
    // daemon has already bound this operation to one assigned repository and
    // one ticket reproducer; preserve a bounded task diagnostic in the Pi
    // transcript so the actor can correct the assigned work before editing.
    const taskDetail = detail
      .replace(/^[a-z_]+:\s*/u, "")
      .replace(/[\r\n\t]+/gu, " ")
      .slice(0, 480);
    if (taskDetail.length > 0) {
      return new Error(
        `The regression checkpoint was rejected: ${taskDetail}. ` +
          "Do not edit until the checkpoint succeeds; correct the stated pre-fix condition and retry it.",
      );
    }
  }
  if (name === "candidate_submit") {
    // Submission is terminal only when validation succeeds. A rejected
    // candidate remains a durable, inspectable fact, so expose its bounded
    // task outcome instead of leaving the engineer with an opaque failure.
    const taskDetail = detail
      .replace(/^[a-z_]+:\s*/u, "")
      .replace(/[\r\n\t]+/gu, " ")
      .slice(0, 480);
    if (taskDetail.length > 0) {
      return new Error(`Candidate submission did not pass: ${taskDetail}.`);
    }
  }
  if (name === "quality_run_full_suite") {
    // Quality cannot submit a review without the kernel-owned receipt, so an
    // opaque failure merely consumes the review assignment. Expose one
    // bounded task-level diagnostic, as the Engineering checkpoint and
    // candidate submission already do; raw transport and authority evidence
    // remains sealed outside the model-visible result.
    const taskDetail = detail
      .replace(/^[a-z_]+:\s*/u, "")
      .replace(/[\r\n\t]+/gu, " ")
      .slice(0, 480);
    if (taskDetail.length > 0) {
      return new Error(`Quality full-suite execution did not pass: ${taskDetail}.`);
    }
  }
  if (
    name === "product_submit_ticket" &&
    detail.includes(
      "Product named a reproducer profile that is not in the admitted application revision",
    )
  ) {
    return new Error(
      "The admitted reproducer profile name is `reproducer`. Set `reproducer_profile` to " +
        "`reproducer`; it names the profile, not the command bytes. Keep `reproducer.command` " +
        "as the exact canonical JSON profile supplied in the assignment, then submit again.",
    );
  }
  if (
    name === "product_submit_ticket" &&
    detail.includes("command bytes are not canonical V1 JSON")
  ) {
    return new Error(
      "The sealed command artifact must be the canonical JSON profile, not a shell command " +
        "string. Use the exact profile JSON supplied in the assignment, keep `reproducer_profile` " +
        "as `reproducer`, and put the behavioral XSH program only in the sealed stdin artifact.",
    );
  }
  if (
    name === "product_submit_ticket" &&
    detail.includes("sealed reproducer command differs from its named admitted profile")
  ) {
    return new Error(
      "The sealed reproducer command must exactly match the assigned `reproducer` profile. " +
        "Use the exact canonical JSON profile supplied in the assignment as the command artifact; " +
        "keep `target/debug/xsh` only inside the sealed stdin program, then submit again.",
    );
  }
  if (
    name === "product_submit_ticket" &&
    detail.includes("ticket contract reads paths must be unique")
  ) {
    return new Error(
      "Use one `contract_reads` entry per repository path. Combine the relevant constraints from " +
        "the same file in that entry's reason, then submit again.",
    );
  }
  if (
    name === "product_submit_ticket" &&
    detail.includes("ticket contract read reason")
  ) {
    return new Error(
      "Each `contract_reads` reason must be nonempty, contain no NUL, and fit within 240 UTF-8 " +
        "bytes so downstream implementation can read the same exact contract. Tighten the reason " +
        "without changing its path, then submit again.",
    );
  }
  if (name === "product_submit_ticket") {
    // A Product proposal is nonterminal until the kernel accepts its exact
    // evidence. Preserve the bounded validator outcome so Product can repair
    // a field such as `contract_owner` in the same paid assignment instead of
    // completing successfully and forcing another discovery session.
    const taskDetail = detail
      .replace(/^[a-z_]+:\s*/u, "")
      .replace(/[\r\n\t]+/gu, " ")
      .slice(0, 480);
    if (taskDetail.length > 0) {
      return new Error(
        `Ticket submission did not pass: ${taskDetail}. Correct the stated field and submit again.`,
      );
    }
  }
  return new Error(`The assigned ${name} operation failed.`);
}

function modelToolInputSchema(name: HostToolName): Readonly<Record<string, unknown>> {
  if (name === "workspace_read") {
    return {
      type: "object",
      additionalProperties: false,
      required: ["repository_relative_path"],
      properties: { repository_relative_path: { type: "string", minLength: 1 } },
    };
  }
  if (name === "artifact_seal") {
    return {
      type: "object",
      additionalProperties: false,
      required: ["workspace_relative_path", "byte_limit"],
      properties: {
        workspace_relative_path: { type: "string", minLength: 1 },
        byte_limit: { type: "integer", minimum: 1 },
      },
    };
  }
  if (name === "artifact_read") {
    return {
      type: "object",
      additionalProperties: false,
      required: ["artifact_id", "expected_digest"],
      properties: {
        artifact_id: { type: "integer", minimum: 1 },
        expected_digest: { type: "string", pattern: "^[a-f0-9]{64}$" },
      },
    };
  }
  if (name === "product_submit_ticket") {
    return productSubmitToolInputSchema();
  }
  if (name === "candidate_checkpoint_regression") {
    return withoutCommandEnvelope(CANDIDATE_CHECKPOINT_REGRESSION_INPUT_SCHEMA_V1);
  }
  if (name === "candidate_submit") return withoutCommandEnvelope(CANDIDATE_SUBMIT_INPUT_SCHEMA_V1);
  if (name === "quality_run_full_suite") {
    return withoutCommandEnvelope(QUALITY_RUN_FULL_SUITE_INPUT_SCHEMA_V1);
  }
  if (name === "quality_submit_review") {
    return withoutCommandEnvelope(QUALITY_SUBMIT_REVIEW_INPUT_SCHEMA_V1);
  }
  if (name === "publication_create") {
    return {
      type: "object",
      additionalProperties: false,
      required: ["anchor", "kind", "summary", "body_artifact_id"],
      properties: {
        anchor: {
          type: "object",
          additionalProperties: false,
          required: ["kind", "id"],
          properties: {
            kind: {
              enum: [
                "project",
                "rfc",
                "rfc_revision",
                "ticket",
                "ticket_revision",
                "experiment",
                "claim",
                "decision",
              ],
            },
            id: { type: "integer", minimum: 1 },
          },
        },
        kind: {
          enum: ["finding", "question", "challenge", "correction", "decision_link", "note"],
        },
        summary: { type: "string", minLength: 1, maxLength: 4096 },
        body_artifact_id: { type: "integer", minimum: 1 },
        attachments: {
          type: "array",
          maxItems: 8,
          items: {
            type: "object",
            additionalProperties: false,
            required: ["artifact_id", "label"],
            properties: {
              artifact_id: { type: "integer", minimum: 1 },
              label: { type: "string", minLength: 1, maxLength: 160 },
            },
          },
        },
        reply_to: { type: ["integer", "null"], minimum: 1 },
        supersedes: { type: ["integer", "null"], minimum: 1 },
      },
    };
  }
  if (name === "forum_list_topics") return forumListSchema();
  if (name === "forum_list_threads") {
    return {
      type: "object",
      additionalProperties: false,
      required: ["topic_id", "limit"],
      properties: {
        topic_id: { type: "integer", minimum: 1 },
        cursor: { type: ["string", "null"], maxLength: 512 },
        limit: { type: "integer", minimum: 1, maximum: 20 },
      },
    };
  }
  if (name === "forum_search") {
    return {
      type: "object",
      additionalProperties: false,
      required: ["query", "limit"],
      properties: {
        query: { type: "string", minLength: 1, maxLength: 4096 },
        topic_id: { type: ["integer", "null"], minimum: 1 },
        thread_id: { type: ["integer", "null"], minimum: 1 },
        post_kind: { enum: [...FORUM_POST_KINDS, null] },
        created_after_micros: { type: ["integer", "null"], minimum: 0 },
        created_before_micros: { type: ["integer", "null"], minimum: 0 },
        cursor: { type: ["string", "null"], maxLength: 512 },
        limit: { type: "integer", minimum: 1, maximum: 20 },
      },
    };
  }
  if (name === "forum_read_thread") {
    return {
      type: "object",
      additionalProperties: false,
      required: ["thread_id", "limit"],
      properties: {
        thread_id: { type: "integer", minimum: 1 },
        after_post_id: { type: ["integer", "null"], minimum: 1 },
        limit: { type: "integer", minimum: 1, maximum: 20 },
      },
    };
  }
  return EMPTY_INPUT_SCHEMA;
}

/**
 * The kernel's Product DTO retains both observations so a sealed proposal is
 * self-contained. At the worker boundary, though, the second observation is
 * a required duplicate of the first and `modelToolWireInput` derives it
 * before framing the strict DTO. Leave only that redundant object open in
 * Pi's pre-invocation schema: otherwise Pi rejects a hand-copied digest
 * before the host can replace it, while widening any authoritative field
 * would weaken the model-to-kernel contract.
 */
function productSubmitToolInputSchema(): Readonly<Record<string, unknown>> {
  const schema = withoutCommandEnvelope(PRODUCT_SUBMIT_TICKET_INPUT_SCHEMA_V1);
  const properties = schema.properties as Readonly<Record<string, unknown>>;
  const reproducer = properties.reproducer as Readonly<Record<string, unknown>>;
  const reproducerProperties = reproducer.properties as Readonly<Record<string, unknown>>;
  return {
    ...schema,
    properties: {
      ...properties,
      reproducer: {
        ...reproducer,
        properties: {
          ...reproducerProperties,
          second_observation: { type: "object" },
        },
      },
    },
  };
}

function forumListSchema(): Readonly<Record<string, unknown>> {
  return {
    type: "object",
    additionalProperties: false,
    required: ["limit"],
    properties: {
      cursor: { type: ["string", "null"], maxLength: 512 },
      limit: { type: "integer", minimum: 1, maximum: 20 },
    },
  };
}

function withoutCommandEnvelope(
  schema: Readonly<Record<string, unknown>>,
): Readonly<Record<string, unknown>> {
  const required = (schema.required as readonly string[]).filter((field) =>
    field !== "client_command_id" && field !== "expected_revision"
  );
  const properties = { ...(schema.properties as Readonly<Record<string, unknown>>) };
  delete properties.client_command_id;
  delete properties.expected_revision;
  return { ...schema, required, properties };
}

function modelToolWireInput(
  name: HostToolName,
  input: unknown,
  commandContext: FramedToolCommandContext,
): unknown {
  const value = object(input) ?? {};
  let semantic: unknown;
  if (name === "forum_list_topics") {
    semantic = { cursor: value.cursor ?? "", limit: value.limit };
  } else if (name === "forum_list_threads") {
    semantic = { topic_id: value.topic_id, cursor: value.cursor ?? "", limit: value.limit };
  } else if (name === "forum_search") {
    const kind = value.post_kind;
    semantic = {
      query: value.query,
      topic_id: value.topic_id ?? null,
      thread_id: value.thread_id ?? null,
      author_office: null,
      post_kind: kind == null ? null : FORUM_POST_KINDS.indexOf(kind as never),
      created_after_micros: value.created_after_micros ?? null,
      created_before_micros: value.created_before_micros ?? null,
      cursor: value.cursor ?? "",
      limit: value.limit,
    };
  } else if (name === "forum_read_thread") {
    semantic = {
      thread_id: value.thread_id,
      after_post_id: value.after_post_id ?? 0,
      limit: value.limit,
    };
  } else {
    semantic = input;
  }
  if (name === "product_submit_ticket") {
    const proposal = object(semantic);
    const reproducer = proposal === undefined ? undefined : object(proposal.reproducer);
    if (reproducer === undefined) return semantic;
    // The full tool schema retains the kernel's complete two-run DTO. Its
    // second observation is a required duplicate of the first, however, so
    // derive it here instead of accepting a hand-copied artifact identity.
    // This prevents one model transcription typo from creating an invalid
    // state while preserving the exact typed wire shape the kernel verifies.
    semantic = {
      ...proposal,
      reproducer: {
        ...reproducer,
        second_observation: reproducer.first_observation,
      },
    };
  }
  if (!SESSION_MUTATING_TOOLS.has(name)) return semantic;
  const fields = object(semantic);
  if (fields === undefined) return semantic;
  // Rust's closed mutating DTOs deliberately place the retry identity before
  // the operation-specific fields.  Build that ordering here rather than
  // relying on the JSON property order returned by a model tool call: the
  // kernel's canonical-frame check must accept the host's own valid command
  // while still rejecting reordered or expanded bytes at its boundary.
  const {
    client_command_id: _ignoredCommandId,
    expected_revision: _ignoredRevision,
    ...semanticFields
  } = fields;
  const client_command_id = `actor-${name}-${commandContext.next_command_id()}`;
  if (name === "publication_create") {
    return { client_command_id, ...semanticFields };
  }
  return {
    client_command_id,
    expected_revision: commandContext.session_revision,
    ...semanticFields,
  };
}

const HIDDEN_MODEL_RESULT_FIELDS = new Set([
  "protocol_version",
  "request_id",
  "operation",
  "audit_id",
  "aggregate_revision",
  "author_kind",
]);
const HIDDEN_MODEL_RESULT_FIELD_VOCABULARY =
  /(?:^|_)(?:architect|campaign|company|control_plane|daemon|director|factory|kernel|office|sponsor)(?:_|$)/u;

/** Removes transport receipts and organizational attribution while retaining
 * the task evidence a worker actually needs. Text bodies and sealed file
 * contents remain byte-for-byte evidence and are never rewritten here. */
export function modelVisibleToolResult(name: HostToolName, value: unknown): unknown {
  const visible = stripHiddenModelResultFields(value);
  if (["product_submit_ticket", "work_complete"].includes(name) && isEmptyRecord(visible)) {
    return { accepted: true };
  }
  if (
    (name === "forum_search" || name === "forum_read_thread") &&
    visible !== null && typeof visible === "object" && !Array.isArray(visible)
  ) {
    const page = visible as Record<string, unknown>;
    const items = Array.isArray(page.items)
      ? page.items.map((item) => {
        if (item === null || typeof item !== "object" || Array.isArray(item)) return item;
        const record = item as Record<string, unknown>;
        const kind = record.kind;
        return {
          ...record,
          ...(typeof kind === "number" && FORUM_POST_KINDS[kind] !== undefined
            ? { kind: FORUM_POST_KINDS[kind] }
            : {}),
        };
      })
      : page.items;
    return { ...page, items };
  }
  return visible;
}

function stripHiddenModelResultFields(value: unknown): unknown {
  if (Array.isArray(value)) return value.map(stripHiddenModelResultFields);
  if (value === null || typeof value !== "object") return value;
  return Object.fromEntries(
    Object.entries(value as Record<string, unknown>)
      .filter(([key]) =>
        !HIDDEN_MODEL_RESULT_FIELDS.has(key) &&
        !HIDDEN_MODEL_RESULT_FIELD_VOCABULARY.test(key)
      )
      .map(([key, item]) => [key, stripHiddenModelResultFields(item)]),
  );
}

function isEmptyRecord(value: unknown): boolean {
  return value !== null && typeof value === "object" && !Array.isArray(value) &&
    Object.keys(value).length === 0;
}

/**
 * Host-side convenience gate for the response emitted by `workspace.read`.
 * The durable proof is the Rust connection-bound ledger; this parser only
 * lets the host fail early when Pi did not receive a successful typed result.
 */
export class DaemonRequiredReadVerifier implements RequiredReadVerifier {
  verify(result: unknown): Promise<RequiredReadObservation | undefined> {
    const outer = object(result);
    const value = object(outer?.details) ?? object(outer?.result) ?? outer;
    if (value === undefined) return Promise.resolve(undefined);
    const canonicalPath = value.canonical_path;
    const digest = value.blake3;
    const length = value.byte_length;
    const content = value.content_base64;
    if (
      typeof canonicalPath !== "string" || canonicalPath.length === 0 ||
      typeof digest !== "string" || !/^[0-9a-f]{64}$/.test(digest) ||
      typeof length !== "number" || !Number.isSafeInteger(length) || length < 0 ||
      typeof content !== "string"
    ) return Promise.resolve(undefined);
    try {
      const decodedLength = Uint8Array.from(atob(content), (character) => character.charCodeAt(0))
        .byteLength;
      if (decodedLength !== length) return Promise.resolve(undefined);
    } catch {
      return Promise.resolve(undefined);
    }
    return Promise.resolve({
      canonical_path: canonicalPath,
      blake3: digest,
      success: true,
    });
  }
}

function object(value: unknown): Record<string, unknown> | undefined {
  return value !== null && typeof value === "object" && !Array.isArray(value)
    ? value as Record<string, unknown>
    : undefined;
}

async function readFrame(file: Deno.FsFile): Promise<Uint8Array> {
  const prefix = await readExactly(file, 4);
  const length = new DataView(prefix.buffer, prefix.byteOffset, prefix.byteLength).getUint32(0);
  if (length > RESPONSE_FRAME_MAX_BYTES) {
    throw new FrameProtocolError("oversized", "daemon response exceeds the 4 MiB limit");
  }
  return await readExactly(file, length).then((payload) => {
    const frame = new Uint8Array(4 + payload.length);
    frame.set(prefix);
    frame.set(payload, 4);
    return frame;
  });
}

async function readExactly(file: Deno.FsFile, length: number): Promise<Uint8Array> {
  const bytes = new Uint8Array(length);
  let offset = 0;
  while (offset < length) {
    const count = await file.read(bytes.subarray(offset));
    if (count === null) throw new Error("daemon closed the actor transport");
    if (count === 0) continue;
    offset += count;
  }
  return bytes;
}

async function writeAll(file: Deno.FsFile, bytes: Uint8Array): Promise<void> {
  let offset = 0;
  while (offset < bytes.byteLength) {
    const count = await file.write(bytes.subarray(offset));
    if (count <= 0) throw new Error("actor transport write made no progress");
    offset += count;
  }
}
