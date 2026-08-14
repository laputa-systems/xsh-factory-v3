import { assert, assertEquals, assertRejects } from "@std/assert";
import { decodeJsonFrame, encodeJsonFrame } from "../factory-sdk/protocol.ts";
import {
  createFramedToolAdapters,
  FramedActorClient,
  InheritedFrameTransport,
  modelVisibleToolResult,
} from "./framed-actor.ts";

class ShortWriteDuplex {
  readonly written: Uint8Array[] = [];
  #response: Uint8Array;
  #offset = 0;

  constructor(response: Uint8Array) {
    this.#response = response;
  }

  write(bytes: Uint8Array): number {
    const count = Math.min(2, bytes.byteLength);
    this.written.push(bytes.subarray(0, count).slice());
    return count;
  }

  read(target: Uint8Array): number | null {
    if (this.#offset === this.#response.byteLength) return null;
    const count = Math.min(3, target.byteLength, this.#response.byteLength - this.#offset);
    target.set(this.#response.subarray(this.#offset, this.#offset + count));
    this.#offset += count;
    return count;
  }

  close(): void {}
}

class SerializedResourceDuplex {
  readonly written: Uint8Array[] = [];
  readonly #response: Uint8Array;
  #offset = 0;
  #writeStarted = false;
  readBeforeWrite = false;

  constructor(response: Uint8Array) {
    this.#response = response;
  }

  write(bytes: Uint8Array): number {
    this.#writeStarted = true;
    this.written.push(bytes.slice());
    return bytes.byteLength;
  }

  read(target: Uint8Array): number | null {
    if (!this.#writeStarted) {
      this.readBeforeWrite = true;
      throw new Error("serialized resource read started before request write");
    }
    if (this.#offset === this.#response.byteLength) return null;
    const count = Math.min(target.byteLength, this.#response.byteLength - this.#offset);
    target.set(this.#response.subarray(this.#offset, this.#offset + count));
    this.#offset += count;
    return count;
  }

  close(): void {}
}

class ScriptedSplitDuplex {
  readonly written: Uint8Array[] = [];
  readonly reader: Pick<Deno.FsFile, "read" | "close">;
  readonly writer: Pick<Deno.FsFile, "write" | "close">;
  readonly #responses: Uint8Array[];
  #available: Uint8Array | undefined;
  #offset = 0;
  #wake: (() => void) | undefined;

  constructor(responses: Uint8Array[]) {
    this.#responses = responses.slice();
    this.reader = {
      read: async (target: Uint8Array): Promise<number | null> => {
        while (this.#available === undefined) {
          await new Promise<void>((resolve) => this.#wake = resolve);
        }
        const count = Math.min(target.byteLength, this.#available.byteLength - this.#offset);
        target.set(this.#available.subarray(this.#offset, this.#offset + count));
        this.#offset += count;
        if (this.#offset === this.#available.byteLength) {
          this.#available = undefined;
          this.#offset = 0;
        }
        return count;
      },
      close: () => {},
    };
    this.writer = {
      write: (bytes: Uint8Array): Promise<number> => {
        this.written.push(bytes.slice());
        const response = this.#responses.shift();
        if (response === undefined || this.#available !== undefined) {
          throw new Error("unexpected scripted transport write");
        }
        this.#available = response;
        this.#wake?.();
        this.#wake = undefined;
        return Promise.resolve(bytes.byteLength);
      },
      close: () => {},
    };
  }
}

Deno.test("inherited actor transport handles short writes and validates response identity", async () => {
  const response = encodeJsonFrame({
    protocol_version: 1,
    request_id: "host-request-1",
    operation: "workspace.read",
    canonical_path: "AGENTS.md",
    blake3: "a".repeat(64),
    byte_length: 0,
    content_base64: "",
  });
  const file = new ShortWriteDuplex(response);
  const client = new FramedActorClient(new InheritedFrameTransport(file as unknown as Deno.FsFile));
  assertEquals(await client.call("workspace.read", { canonical_path: "AGENTS.md" }), {
    protocol_version: 1,
    request_id: "host-request-1",
    operation: "workspace.read",
    canonical_path: "AGENTS.md",
    blake3: "a".repeat(64),
    byte_length: 0,
    content_base64: "",
  });
  const request = decodeJsonFrame<Record<string, unknown>>(
    merge(file.written),
    "workspace.read",
  );
  assertEquals(request.operation, "workspace.read");
  assertEquals(request.canonical_path, "AGENTS.md");
});

Deno.test("actor artifact sealing frames canonical retry identity before semantic fields", async () => {
  let requestBytes: Uint8Array | undefined;
  const transport = {
    exchange: (frame: Uint8Array) => {
      requestBytes = frame;
      const request = decodeJsonFrame<Record<string, unknown>>(
        frame,
        "artifact.seal_workspace_file",
      );
      return Promise.resolve(encodeJsonFrame({
        protocol_version: 1,
        request_id: request.request_id,
        operation: "artifact.seal_workspace_file",
        artifact_id: 7,
        digest: "a".repeat(64),
        byte_length: 12,
        aggregate_revision: 4,
      }));
    },
  };
  const [tool] = createFramedToolAdapters(
    new FramedActorClient(transport),
    ["artifact_seal"],
    { session_revision: 4, next_command_id: () => 9 },
  );

  await tool.sdk_definition.invoke({
    workspace_relative_path: ".product-evidence/narrative",
    byte_limit: 4096,
  });

  const bytes = requestBytes;
  assert(bytes !== undefined);
  assertEquals(
    new TextDecoder().decode(bytes.slice(4)),
    '{"protocol_version":1,"request_id":"host-request-1","operation":"artifact.seal_workspace_file","client_command_id":"actor-artifact_seal-9","expected_revision":4,"workspace_relative_path":".product-evidence/narrative","byte_limit":4096}',
  );
});

Deno.test("inherited full-duplex transport writes before reading one serialized FsFile", async () => {
  const response = encodeJsonFrame({ operation: "test.response", accepted: true });
  const file = new SerializedResourceDuplex(response);
  const transport = new InheritedFrameTransport(file as unknown as Deno.FsFile);
  assertEquals(
    decodeJsonFrame(
      await transport.exchange(encodeJsonFrame({ operation: "test.request", request: true })),
      "test.response",
    ),
    { operation: "test.response", accepted: true },
  );
  assertEquals(file.readBeforeWrite, false);
  assertEquals(decodeJsonFrame(merge(file.written), "test.request"), {
    operation: "test.request",
    request: true,
  });
});

Deno.test("split inherited resources support repeated framed exchanges", async () => {
  const duplex = new ScriptedSplitDuplex([
    encodeJsonFrame({ operation: "test.response", ordinal: 1 }),
    encodeJsonFrame({ operation: "test.response", ordinal: 2 }),
  ]);
  const transport = new InheritedFrameTransport(
    duplex.reader as Deno.FsFile,
    duplex.writer as Deno.FsFile,
  );
  for (const ordinal of [1, 2]) {
    assertEquals(
      decodeJsonFrame(
        await transport.exchange(encodeJsonFrame({ operation: "test.request", ordinal })),
        "test.response",
      ),
      { operation: "test.response", ordinal },
    );
  }
  assertEquals(duplex.written.length, 2);
});

Deno.test("inherited actor transport stops on a truncated response", async () => {
  const prefix = new Uint8Array([0, 0, 0, 4, 1]);
  const file = new ShortWriteDuplex(prefix);
  const transport = new InheritedFrameTransport(file as unknown as Deno.FsFile);
  await assertRejects(
    () => transport.exchange(encodeJsonFrame({ ok: true })),
    Error,
    "daemon closed",
  );
});

Deno.test("actor transport EOF drives the shared authority-loss signal", async () => {
  const response = encodeJsonFrame({
    protocol_version: 1,
    request_id: "host-request-1",
    operation: "session.submit_terminal",
    audit_id: 9,
    aggregate_revision: 1,
  });
  const transport = new InheritedFrameTransport(
    new ShortWriteDuplex(response) as unknown as Deno.FsFile,
  );
  let lost = false;
  transport.onLoss(() => lost = true);
  const client = new FramedActorClient(transport);
  await client.call("session.submit_terminal", {});
  for (let attempt = 0; attempt < 20 && !lost; attempt += 1) await Promise.resolve();
  assertEquals(lost, true);
  assertEquals(transport.isAlive(), false);
});

Deno.test("Product submission custom tool exposes the closed proposal schema", () => {
  const client = new FramedActorClient({
    exchange: () => Promise.reject(new Error("not invoked while inspecting schema")),
  });
  const [tool] = createFramedToolAdapters(client, ["product_submit_ticket"]);
  assertEquals(tool.name, "product_submit_ticket");
  const schema = tool.sdk_definition.input_schema as {
    readonly additionalProperties?: unknown;
    readonly required?: readonly string[];
    readonly properties?: Record<string, unknown>;
  };
  assertEquals(schema.additionalProperties, false);
  assert(schema.required?.includes("reproducer"));
  assert(schema.required?.includes("duplicate_search"));
  assert(!schema.required?.includes("sponsor"));
  assert("narrative" in (schema.properties ?? {}));
});

Deno.test("Product submission names a safe correction for a rejected reproducer profile", async () => {
  const client = new FramedActorClient({
    exchange: () =>
      Promise.reject(
        new Error(
          "invalid_json: the sealed reproducer command differs from its named admitted profile",
        ),
      ),
  });
  const [tool] = createFramedToolAdapters(client, ["product_submit_ticket"]);

  await assertRejects(
    () => tool.sdk_definition.invoke({}),
    Error,
    "Use the exact canonical JSON profile supplied in the assignment as the command artifact",
  );
});

Deno.test("Product submission names the admitted profile after a profile-name rejection", async () => {
  const client = new FramedActorClient({
    exchange: () =>
      Promise.reject(
        new Error(
          "invalid_json: Product named a reproducer profile that is not in the admitted application revision",
        ),
      ),
  });
  const [tool] = createFramedToolAdapters(client, ["product_submit_ticket"]);

  await assertRejects(
    () => tool.sdk_definition.invoke({}),
    Error,
    "Keep `reproducer.command` as the exact canonical JSON profile supplied in the assignment",
  );
});

Deno.test("Product submission names the canonical command-profile artifact after its parser rejects", async () => {
  const client = new FramedActorClient({
    exchange: () =>
      Promise.reject(
        new Error(
          "invalid_json: Product proposal contract is invalid: invalid_json: command bytes are not canonical V1 JSON or contain unknown fields",
        ),
      ),
  });
  const [tool] = createFramedToolAdapters(client, ["product_submit_ticket"]);

  await assertRejects(
    () => tool.sdk_definition.invoke({}),
    Error,
    "The sealed command artifact must be the canonical JSON profile",
  );
});

Deno.test("Product submission preserves its bounded validation outcome", async () => {
  const client = new FramedActorClient({
    exchange: () =>
      Promise.reject(
        new Error(
          "invalid_json: Product proposal contract is invalid: invalid XSH Product proposal: contract_owner must name one supplied contract read",
        ),
      ),
  });
  const [tool] = createFramedToolAdapters(client, ["product_submit_ticket"]);

  await assertRejects(
    () => tool.sdk_definition.invoke({}),
    Error,
    "contract_owner must name one supplied contract read",
  );
});

Deno.test("Engineering checkpoint names the exact-read recovery before mutation", async () => {
  const client = new FramedActorClient({
    exchange: () =>
      Promise.reject(
        new Error(
          "invalid_rpc: all assigned exact reads are required before mutation",
        ),
      ),
  });
  const [tool] = createFramedToolAdapters(client, ["candidate_checkpoint_regression"]);

  await assertRejects(
    () =>
      tool.sdk_definition.invoke({
        regression_command: "reproducer",
        expected_failure: "ticket-attempt-1-reproducer",
      }),
    Error,
    "use `workspace_read` (not shell commands) on every path listed in the assignment",
  );
});

Deno.test("Engineering checkpoint preserves a bounded task diagnostic before edits", async () => {
  const client = new FramedActorClient({
    exchange: () =>
      Promise.reject(
        new Error("invalid_json: the regression checkpoint unexpectedly passed"),
      ),
  });
  const [tool] = createFramedToolAdapters(client, ["candidate_checkpoint_regression"]);

  await assertRejects(
    () =>
      tool.sdk_definition.invoke({
        regression_command: "reproducer",
        expected_failure: "ticket-attempt-1-reproducer",
      }),
    Error,
    "The regression checkpoint was rejected: the regression checkpoint unexpectedly passed",
  );
});

Deno.test("Engineering candidate submission preserves its bounded validation outcome", async () => {
  const client = new FramedActorClient({
    exchange: () =>
      Promise.reject(
        new Error("invalid_json: hard candidate validation is Failed (candidate 4, validation 9)"),
      ),
  });
  const [tool] = createFramedToolAdapters(client, ["candidate_submit"]);

  await assertRejects(
    () => tool.sdk_definition.invoke({}),
    Error,
    "Candidate submission did not pass: hard candidate validation is Failed",
  );
});

Deno.test("Quality full-suite execution preserves a bounded task diagnostic", async () => {
  const client = new FramedActorClient({
    exchange: () =>
      Promise.reject(
        new Error("invalid_json: Quality assignment target is not an exact validated candidate"),
      ),
  });
  const [tool] = createFramedToolAdapters(client, ["quality_run_full_suite"]);

  await assertRejects(
    () => tool.sdk_definition.invoke({ validation_profile: "full" }),
    Error,
    "Quality full-suite execution did not pass: Quality assignment target is not an exact validated candidate",
  );
});

Deno.test("Product submission corrects duplicate ticket contract-read paths", async () => {
  const client = new FramedActorClient({
    exchange: () =>
      Promise.reject(
        new Error(
          "Product proposal contract is invalid: ticket contract reads paths must be unique",
        ),
      ),
  });
  const [tool] = createFramedToolAdapters(client, ["product_submit_ticket"]);

  await assertRejects(
    () => tool.sdk_definition.invoke({}),
    Error,
    "one `contract_reads` entry per repository path",
  );
});

Deno.test("Product submission corrects an oversized ticket contract-read reason", async () => {
  const client = new FramedActorClient({
    exchange: () =>
      Promise.reject(
        new Error(
          "Product proposal contract is invalid: ticket contract read reason: text is empty, oversized, or contains NUL",
        ),
      ),
  });
  const [tool] = createFramedToolAdapters(client, ["product_submit_ticket"]);

  await assertRejects(
    () => tool.sdk_definition.invoke({}),
    Error,
    "fit within 240 UTF-8 bytes",
  );
});

Deno.test("Engineering and Quality adapters expose their exact closed tool schemas", () => {
  const client = new FramedActorClient({
    exchange: () => Promise.reject(new Error("not invoked while inspecting schema")),
  });
  const tools = createFramedToolAdapters(client, [
    "candidate_submit",
    "quality_run_full_suite",
    "quality_submit_review",
  ]);
  const schema = (name: string) =>
    tools.find((tool) => tool.name === name)?.sdk_definition
      .input_schema as {
        readonly additionalProperties?: unknown;
        readonly required?: readonly string[];
      };
  assertEquals(schema("candidate_submit").additionalProperties, false);
  assert(!schema("candidate_submit").required?.includes("engineering_report"));
  assert(!schema("candidate_submit").required?.includes("candidate_tree_artifact_id"));
  assert(!schema("candidate_submit").required?.includes("client_command_id"));
  assert(!schema("candidate_submit").required?.includes("expected_revision"));
  assertEquals(schema("quality_run_full_suite").additionalProperties, false);
  assert(!schema("quality_run_full_suite").required?.includes("client_command_id"));
  assertEquals(schema("quality_submit_review").additionalProperties, false);
  assert(schema("quality_submit_review").required?.includes("full_suite_validation_id"));
  assert(!schema("quality_submit_review").required?.includes("reasons"));
});

Deno.test("Forum custom tools expose bounded actor schemas without author-office filters", () => {
  const client = new FramedActorClient({
    exchange: () => Promise.reject(new Error("not invoked while inspecting schema")),
  });
  const tools = createFramedToolAdapters(client, [
    "forum_search",
    "forum_list_topics",
    "forum_list_threads",
    "forum_read_thread",
  ]);
  assertEquals(tools.length, 4);
  const search = tools.find((tool) => tool.name === "forum_search")!.sdk_definition
    .input_schema as { properties: Record<string, unknown>; additionalProperties: boolean };
  assertEquals(search.additionalProperties, false);
  assertEquals("author_office" in search.properties, false);
  assertEquals("query" in search.properties, true);
  assertEquals(tools.map((tool) => tool.name), [
    "forum_search",
    "forum_list_topics",
    "forum_list_threads",
    "forum_read_thread",
  ]);
});

Deno.test("custom-tool results omit transport and organizational metadata", () => {
  assertEquals(
    modelVisibleToolResult("quality_run_full_suite", {
      protocol_version: 1,
      request_id: "host-request-1",
      operation: "quality.run_full_suite",
      audit_id: 9,
      aggregate_revision: 4,
      campaign_id: 3,
      kernel_build_id: "hidden",
      validation_id: 10,
      candidate_id: 11,
      candidate_tree: "a".repeat(40),
    }),
    {
      validation_id: 10,
      candidate_id: 11,
      candidate_tree: "a".repeat(40),
    },
  );
  assertEquals(
    modelVisibleToolResult("forum_read_thread", {
      protocol_version: 1,
      request_id: "host-request-2",
      operation: "forum.read_thread",
      items: [{
        id: 1,
        kind: 2,
        author_kind: 0,
        author_office: 1,
        body: "Peer-authored text remains exact evidence.",
      }],
      next_cursor: "1",
    }),
    {
      items: [{
        id: 1,
        kind: "Finding",
        body: "Peer-authored text remains exact evidence.",
      }],
      next_cursor: "1",
    },
  );
  assertEquals(
    modelVisibleToolResult("product_submit_ticket", {
      protocol_version: 1,
      request_id: "host-request-3",
      operation: "product.submit_ticket",
      audit_id: 12,
      aggregate_revision: 8,
    }),
    { accepted: true },
  );
});

function merge(chunks: readonly Uint8Array[]): Uint8Array {
  const length = chunks.reduce((total, chunk) => total + chunk.byteLength, 0);
  const merged = new Uint8Array(length);
  let offset = 0;
  for (const chunk of chunks) {
    merged.set(chunk, offset);
    offset += chunk.byteLength;
  }
  return merged;
}
