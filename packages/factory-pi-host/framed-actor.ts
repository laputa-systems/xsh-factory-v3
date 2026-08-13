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
  readonly #file: Deno.FsFile;
  #tail = Promise.resolve();
  #reader: Promise<void> | undefined;
  #waiting: Array<{
    resolve: (frame: Uint8Array) => void;
    reject: (error: unknown) => void;
  }> = [];
  #lossListeners = new Set<() => void>();
  #loss: unknown | undefined;

  constructor(file: Deno.FsFile) {
    this.#file = file;
  }

  exchange(frame: Uint8Array): Promise<Uint8Array> {
    const exchange = this.#tail.then(async () => {
      if (this.#loss !== undefined) throw this.#loss;
      const response = new Promise<Uint8Array>((resolve, reject) => {
        this.#waiting.push({ resolve, reject });
      });
      this.#reader ??= this.#readResponses();
      try {
        await writeAll(this.#file, frame);
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
        const frame = await readFrame(this.#file);
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
      "candidate_commit",
    ]);
    requireInteger("audit_id");
    requireInteger("aggregate_revision");
    requireInteger("candidate_id");
    requireInteger("validation_id");
    requireString("candidate_tree");
    requireString("candidate_commit");
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
  forum_read: "forum.read",
  forum_write: "forum.write",
  forum_search: "forum.search",
  forum_list_topics: "forum.list_topics",
  forum_list_threads: "forum.list_threads",
  forum_read_thread: "forum.read_thread",
  forum_create_topic: "forum.create_topic",
  forum_create_thread: "forum.create_thread",
  forum_post: "forum.post",
};

/** Converts admitted tool names into daemon-bound, operation-specific wrappers. */
export function createFramedToolAdapters(
  client: FramedActorClient,
  names: readonly HostToolName[],
): readonly PiToolAdapter[] {
  return names.flatMap((name) => {
    const operation = TOOL_OPERATIONS[name];
    // These five common tools are the pinned Pi SDK's host-local primitives;
    // they deliberately do not pretend a daemon operation exists.
    if (
      operation === undefined &&
      ["workspace_write", "workspace_edit", "workspace_search", "workspace_list", "shell"]
        .includes(name)
    ) return [];
    if (operation === undefined) throw new Error(`no framed operation for ${name}`);
    return [{
      name,
      sdk_definition: {
        description: `Daemon-bound ${name} operation.`,
        input_schema: name === "workspace_read"
          ? {
            type: "object",
            additionalProperties: false,
            required: ["repository_relative_path"],
            properties: {
              repository_relative_path: { type: "string", minLength: 1 },
            },
          }
          : name === "artifact_seal"
          ? {
            type: "object",
            additionalProperties: false,
            required: [
              "client_command_id",
              "expected_revision",
              "staging_relative_path",
              "byte_limit",
            ],
            properties: {
              client_command_id: { type: "string", minLength: 1, maxLength: 160 },
              expected_revision: { type: "integer", minimum: 0 },
              staging_relative_path: { type: "string", minLength: 1 },
              byte_limit: { type: "integer", minimum: 1 },
            },
          }
          : name === "artifact_read"
          ? {
            type: "object",
            additionalProperties: false,
            required: ["artifact_id", "expected_digest"],
            properties: {
              artifact_id: { type: "integer", minimum: 1 },
              expected_digest: { type: "string", pattern: "^[a-f0-9]{64}$" },
            },
          }
          : name === "product_submit_ticket"
          ? PRODUCT_SUBMIT_TICKET_INPUT_SCHEMA_V1
          : name === "candidate_checkpoint_regression"
          ? CANDIDATE_CHECKPOINT_REGRESSION_INPUT_SCHEMA_V1
          : name === "candidate_submit"
          ? CANDIDATE_SUBMIT_INPUT_SCHEMA_V1
          : name === "quality_run_full_suite"
          ? QUALITY_RUN_FULL_SUITE_INPUT_SCHEMA_V1
          : name === "quality_submit_review"
          ? QUALITY_SUBMIT_REVIEW_INPUT_SCHEMA_V1
          : { type: "object", additionalProperties: false },
        invoke: (input: unknown) => client.call(operation, input),
      },
    }];
  });
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
