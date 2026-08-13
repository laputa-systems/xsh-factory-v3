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
  assert(schema("candidate_submit").required?.includes("engineering_report"));
  assert(!schema("candidate_submit").required?.includes("candidate_tree_artifact_id"));
  assertEquals(schema("quality_run_full_suite").additionalProperties, false);
  assert(schema("quality_run_full_suite").required?.includes("client_command_id"));
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
    "forum_create_topic",
    "forum_create_thread",
    "forum_post",
  ]);
  assertEquals(tools.length, 7);
  const search = tools.find((tool) => tool.name === "forum_search")!.sdk_definition
    .input_schema as { properties: Record<string, unknown>; additionalProperties: boolean };
  assertEquals(search.additionalProperties, false);
  assertEquals("author_office" in search.properties, false);
  assertEquals("query" in search.properties, true);
  const post = tools.find((tool) => tool.name === "forum_post")!.sdk_definition
    .input_schema as { required: readonly string[]; additionalProperties: boolean };
  assertEquals(post.additionalProperties, false);
  assert(post.required.includes("body"));
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
