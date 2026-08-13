import { assert, assertEquals, assertRejects, assertThrows } from "@std/assert";
import { join } from "@std/path";
import { type Context, fauxAssistantMessage, fauxProvider, fauxText, fauxToolCall } from "@pi/ai";
import { ModelRuntime } from "@pi/coding-agent";
import {
  type ArtifactSealer,
  type ArtifactSealReceipt,
  ASSIGNMENT_EVIDENCE_ROLES_V1,
  builtinPiToolNames,
  createCommonToolAdapters,
  createEphemeralCredentialStore,
  createForumTools,
  createFramedToolAdapters,
  createSdkSession,
  createSdkSessionWithRuntimeForTest,
  decodeAssignmentPacketV1,
  emptyResourceLoader,
  FramedActorClient,
  gzipFile,
  type PiAssignmentPacket,
  type PiHostDependencies,
  type PiSessionFactory,
  type PiSessionLike,
  type PiToolAdapter,
  readSessionAdmissionFrame,
  runAssignment,
  runPiHostEntrypoint,
  toPiToolDefinition,
  validateToolAllowlist,
  verifyModelDescriptor,
} from "./mod.ts";
import { canonicalJson } from "../factory-sdk/protocol.ts";
import { ForumAdapter } from "../factory-sdk/forum.ts";

class FakeSession implements PiSessionLike {
  #listeners = new Set<(event: unknown) => void>();
  disposed = false;
  aborted = false;
  lastPrompt: string | undefined;
  constructor(
    private readonly events: readonly unknown[],
    private readonly beforeEvents?: () => Promise<void>,
  ) {}
  subscribe(listener: (event: unknown) => void): () => void {
    this.#listeners.add(listener);
    return () => this.#listeners.delete(listener);
  }
  async prompt(text: string): Promise<void> {
    this.lastPrompt = text;
    await this.beforeEvents?.();
    for (const event of this.events) for (const listener of this.#listeners) listener(event);
  }
  dispose(): void {
    this.disposed = true;
  }
  abort(): Promise<void> {
    this.aborted = true;
    return Promise.resolve();
  }
}

class FakeFactory implements PiSessionFactory {
  created = 0;
  lastPacket: PiAssignmentPacket | undefined;
  lastSession: FakeSession | undefined;
  constructor(
    private readonly events: readonly unknown[],
    private readonly beforeEvents?: (tools: readonly PiToolAdapter[]) => Promise<void>,
  ) {}
  create(
    packet: PiAssignmentPacket,
    context: { custom_tools: readonly PiToolAdapter[] },
  ): Promise<PiSessionLike> {
    this.created += 1;
    this.lastPacket = packet;
    this.lastSession = new FakeSession(
      this.events,
      async () => {
        await this.beforeEvents?.(context.custom_tools);
      },
    );
    return Promise.resolve(this.lastSession);
  }
}

class FakeSealer implements ArtifactSealer {
  readonly roles: string[] = [];
  async seal(
    path: string,
    role: "pi_transcript_gzip" | "required_read_manifest",
  ): Promise<ArtifactSealReceipt> {
    this.roles.push(role);
    const bytes = await Deno.readFile(path);
    return {
      artifact_id: this.roles.length,
      digest: `digest-${this.roles.length}`,
      byte_length: bytes.length,
    };
  }
}

function noTransportClient(): FramedActorClient {
  return new FramedActorClient({
    exchange: () => Promise.reject(new Error("tool descriptor inspection must not send a frame")),
  });
}

Deno.test("model-visible tool descriptors hide control-plane vocabulary", () => {
  const noTransport = {
    exchange: () => Promise.reject(new Error("tool descriptor inspection must not send a frame")),
  };
  const framed = createFramedToolAdapters(noTransportClient(), [
    "workspace_read",
    "artifact_seal",
    "artifact_read",
    "product_submit_ticket",
    "candidate_checkpoint_regression",
    "candidate_submit",
    "quality_run_full_suite",
    "quality_submit_review",
    "work_complete",
    "forum_search",
    "forum_list_topics",
    "forum_list_threads",
    "forum_read_thread",
    "forum_create_topic",
    "forum_create_thread",
    "forum_post",
  ]);
  const forum = createForumTools(new ForumAdapter(noTransport));
  const common = createCommonToolAdapters({
    artifact_seal: () => Promise.resolve({ accepted: true }),
    artifact_read: () => Promise.resolve({ content_base64: "" }),
  });
  const forbidden =
    /\b(?:architect|campaign|company|control[- ]plane|cto|daemon|department|director|employee|factory|kernel|manager|office|organization|sponsor)\b/iu;
  for (const tool of [...framed, ...forum, ...common]) {
    const descriptor = "sdk_definition" in tool ? tool.sdk_definition : tool;
    const visible = JSON.stringify({
      name: tool.name,
      description: descriptor.description,
      input_schema: descriptor.input_schema,
    });
    const match = forbidden.exec(visible.replaceAll("_", " ").replaceAll("-", " "));
    if (match !== null) {
      throw new Error(`${tool.name} exposes control-plane vocabulary ${JSON.stringify(match[0])}`);
    }
  }
});

Deno.test("model-visible evidence labels use only task vocabulary", () => {
  const forbidden =
    /\b(?:architect|campaign|company|control[- ]plane|daemon|department|director|factory|kernel|office|sponsor)\b/iu;
  for (const role of ASSIGNMENT_EVIDENCE_ROLES_V1) {
    const match = forbidden.exec(role.replaceAll("_", " "));
    if (match !== null) {
      throw new Error(`assignment evidence role exposes ${JSON.stringify(match[0])}`);
    }
  }
});

function packet(
  staging_root: string,
  required_reads = true,
  terminal_required = required_reads,
): PiAssignmentPacket {
  return {
    format_version: 1,
    assignment_id: "assignment-1",
    office: "engineering",
    campaign_id: "campaign-1",
    application_revision_id: "3",
    kernel_build_id: "e".repeat(64),
    repository_base_identity: "f".repeat(64),
    factory_base_identity: "4".repeat(64),
    target: "generic-assignment",
    ticket_attempt_id: "17",
    candidate_id: null,
    packet_digest: "a".repeat(64),
    system_prompt_artifact_id: 1,
    assignment_prompt_artifact_id: 2,
    required_read_manifest_artifact_id: 3,
    system_prompt_digest: "b".repeat(64),
    assignment_prompt_digest: "c".repeat(64),
    aggregate_revision: "4",
    aggregate_cost_remaining_micro_usd: 100,
    legal_terminal_operations: ["work_complete"],
    workspace_root: staging_root,
    staging_root,
    system_prompt_bytes: new TextEncoder().encode("sealed system"),
    assignment_prompt_bytes: new TextEncoder().encode("sealed assignment"),
    model: {
      provider: "fake-provider",
      model_id: "fake-model",
      thinking_level: "high",
      context_token_limit: 100,
      output_token_limit: 50,
      price_input_micro_usd_per_million_tokens: 1,
      price_output_micro_usd_per_million_tokens: 2,
      price_cache_read_micro_usd_per_million_tokens: 3,
      price_cache_write_micro_usd_per_million_tokens: 4,
      capability_flags: [],
    },
    limits: { turn_limit: 4, wall_limit_millis: 10_000, output_byte_limit: 50_000 },
    tools: ["workspace_read", "shell", "work_complete"],
    required_reads: required_reads
      ? [{ canonical_path: "AGENTS.md", blake3: "d".repeat(64), reason: "contract" }]
      : [],
    assignment_evidence: [
      {
        role: "ticket_proposal",
        artifact_id: 4,
        digest: "6".repeat(64),
        byte_length: 1,
      },
    ],
    runtime: {
      deno_executable: "/opt/deno",
      deno_version: "2.9.4",
      source_graph_digest: "1".repeat(64),
      resolved_dependency_graph_digest: "5".repeat(64),
      deno_json_digest: "2".repeat(64),
      deno_lock_digest: "3".repeat(64),
      pi_version: "0.84.1",
      credential_source: { kind: "environment", name: "FAKE_PROVIDER_KEY" },
    },
    terminal_submission_required: terminal_required,
  };
}

function terminal(
  cost_micro_usd: number | undefined,
  stop_reason = "completed",
): Record<string, unknown> {
  return {
    type: "terminal",
    stop_reason,
    cost_micro_usd,
    turns: 2,
    usage: { input: 11, output: 7, cache_read: 3, cache_write: 2, reasoning: 5 },
  };
}

function deps(factory: PiSessionFactory, sealer = new FakeSealer()): PiHostDependencies {
  return {
    session_factory: factory,
    artifact_sealer: sealer,
    packet_integrity_verifier: () => Promise.resolve(true),
    authority: {
      file: undefined as unknown as Deno.FsFile,
      await_admission: () =>
        Promise.resolve({
          type: "session.admitted" as const,
          protocol_version: 1 as const,
          assignment_id: "assignment-1",
          session_id: 1,
          session_revision: 0,
          packet_digest: "a".repeat(64),
          packet_b64: "eyJwYWNrZXQiOnRydWV9",
        }),
      is_alive: () => true,
    },
    terminal_submission: {
      submit: async (_operation, _payload, _manifest, _summary) => {},
    },
    custom_tools: [
      {
        name: "workspace_read" as const,
        sdk_definition: {
          description: "read",
          input_schema: { type: "object", additionalProperties: false },
          invoke: (input: unknown) => Promise.resolve(input),
        },
      },
      {
        name: "shell" as const,
        sdk_definition: {
          description: "shell",
          input_schema: { type: "object", additionalProperties: false },
          invoke: (input: unknown) => Promise.resolve(input),
        },
      },
      {
        name: "work_complete" as const,
        sdk_definition: {
          description: "complete",
          input_schema: { type: "object", additionalProperties: false },
          invoke: (input: unknown) => Promise.resolve(input),
        },
      },
    ],
    required_read_verifier: {
      verify: (result: unknown) => {
        const value = result as { bytes?: string };
        return Promise.resolve(
          value.bytes === "contract bytes"
            ? { canonical_path: "AGENTS.md", blake3: "d".repeat(64), success: true }
            : undefined,
        );
      },
    },
  };
}

Deno.test("fake Pi success preserves exact identity, retries, usage, and required-read proof", async () => {
  const root = await Deno.makeTempDir({ prefix: "pi-host-" });
  let underlyingTerminalCalls = 0;
  const factory = new FakeFactory([
    { type: "tool_execution_start", tool_name: "workspace_read" },
    {
      type: "tool_execution_end",
      tool_name: "workspace_read",
      result: { bytes: "contract bytes" },
      exit_code: 0,
    },
    { type: "auto_retry_start", attempt: 1 },
    { type: "tool_execution_start", tool_name: "shell" },
    { type: "tool_execution_end", tool_name: "shell", exit_code: 1 },
    {
      type: "agent_end",
      messages: [{
        role: "assistant",
        stopReason: "completed",
        usage: {
          input: 11,
          output: 7,
          cacheRead: 3,
          cacheWrite: 2,
          reasoning: 5,
          cost: { input: 0, output: 0, cacheRead: 0, cacheWrite: 0, total: 0.000003 },
        },
      }],
    },
  ], async (tools) => {
    const terminalTool = tools.find((tool) => tool.name === "work_complete");
    await terminalTool?.sdk_definition.invoke({ operation: "typed-terminal-payload" });
  });
  const sealer = new FakeSealer();
  const baseDeps = deps(factory, sealer);
  const result = await runAssignment(packet(root), {
    ...baseDeps,
    custom_tools: baseDeps.custom_tools?.map((tool) =>
      tool.name === "work_complete"
        ? {
          ...tool,
          sdk_definition: {
            ...tool.sdk_definition,
            invoke: (input) => {
              underlyingTerminalCalls += 1;
              return tool.sdk_definition.invoke(input);
            },
          },
        }
        : tool
    ),
    terminal_submission: {
      submit: (operation, payload) => {
        assertEquals(operation, "work_complete");
        assertEquals(payload, { operation: "typed-terminal-payload" });
        return Promise.resolve();
      },
    },
    runtime: { deno_version: "2.9.4", pi_sdk_version: "0.84.1" },
  });
  assertEquals(result.status, "succeeded");
  assertEquals(result.summary.cost_micro_usd, 3);
  assertEquals(result.summary.tokens.reasoning, 5);
  assertEquals(result.summary.retries, 1);
  assertEquals(result.summary.tool_calls, 2);
  assertEquals(result.summary.nonzero_tool_results, 1);
  assertEquals(result.required_read_manifest.satisfied.length, 1);
  assertEquals(sealer.roles, ["pi_transcript_gzip"]);
  assertEquals(factory.created, 1);
  assertEquals(
    factory.lastPacket?.assignment_prompt_bytes,
    new TextEncoder().encode("sealed assignment"),
  );
  assertEquals(factory.lastSession?.lastPrompt, "sealed assignment");
  assertEquals(factory.lastSession?.disposed, true);
  const transcript = JSON.parse(
    await Deno.readTextFile(result.summary.transcript_path).then((text) =>
      `[${text.trim().split("\n").join(",")}]`
    ),
  ) as unknown[];
  assertEquals(transcript.length, 6);
  assertEquals(underlyingTerminalCalls, 0);
});

Deno.test("1,000 raw events are streamed and gzip is complete", async () => {
  const root = await Deno.makeTempDir({ prefix: "pi-host-events-" });
  const events: unknown[] = Array.from(
    { length: 1_000 },
    (_, index) => ({ type: "message_update", index }),
  );
  events.push({ type: "terminal", cost_micro_usd: 1, stop_reason: "completed" });
  const result = await runAssignment(packet(root, false), deps(new FakeFactory(events)));
  assertEquals(result.status, "succeeded");
  const compressed = await Deno.readFile(result.summary.transcript_gzip_path);
  const decompressed = await new Response(
    new Blob([compressed]).stream().pipeThrough(new DecompressionStream("gzip")),
  ).text();
  assertEquals(decompressed.trim().split("\n").length, 1_001);
});

Deno.test("all provider stop reasons normalize without inventing cost", async () => {
  for (const stop_reason of ["completed", "length", "tool_error", "aborted"]) {
    const root = await Deno.makeTempDir({ prefix: "pi-host-stop-" });
    const result = await runAssignment(
      packet(root, false),
      deps(new FakeFactory([terminal(1, stop_reason)])),
    );
    assertEquals(result.status, "succeeded");
    assertEquals(result.summary.stop_reason, stop_reason);
  }
  const root = await Deno.makeTempDir({ prefix: "pi-host-cost-" });
  const result = await runAssignment(
    packet(root, false),
    {
      ...deps(new FakeFactory([terminal(undefined)])),
      terminal_submission: {
        submit: (operation, _payload, _manifest, summary) => {
          assertEquals(operation, null);
          assertEquals(summary.stop_reason, "unknown_cost");
          return Promise.resolve();
        },
      },
    },
  );
  assertEquals(result.status, "cost_unknown");
  assertEquals(result.summary.cost_micro_usd, null);
});

Deno.test("missing exact required read sends failure reconciliation without authority", async () => {
  const root = await Deno.makeTempDir({ prefix: "pi-host-read-" });
  let submission: { operation: unknown; stop_reason: string } | undefined;
  const result = await runAssignment(packet(root), {
    ...deps(new FakeFactory([terminal(1)])),
    terminal_submission: {
      submit: (operation, _payload, _manifest, summary) => {
        submission = { operation, stop_reason: summary.stop_reason };
        return Promise.resolve();
      },
    },
  });
  assertEquals(result.status, "required_reads_missing");
  assertEquals(result.required_read_manifest.missing.length, 1);
  assertEquals(submission, { operation: null, stop_reason: "protocol_error" });
});

Deno.test("daemon disconnect stops prompt admission and host has no resume path", async () => {
  const root = await Deno.makeTempDir({ prefix: "pi-host-disconnect-" });
  let prompted = false;
  const factory: PiSessionFactory = {
    create: () =>
      Promise.resolve({
        subscribe: () => () => {},
        prompt: () => {
          prompted = true;
          return Promise.resolve();
        },
        dispose: () => {},
      }),
  };
  const result = await runAssignment(packet(root, false, true), {
    ...deps(factory),
    authority: {
      file: undefined as unknown as Deno.FsFile,
      await_admission: () =>
        Promise.resolve({
          type: "session.admitted" as const,
          protocol_version: 1 as const,
          assignment_id: "assignment-1",
          session_id: 1,
          session_revision: 0,
          packet_digest: "a".repeat(64),
          packet_b64: "eyJwYWNrZXQiOnRydWV9",
        }),
      is_alive: () => false,
    },
  });
  assertEquals(result.status, "disconnected");
  assertEquals(prompted, false);
});

Deno.test("empty resource loader and credentials expose no ambient resources or secret bytes", () => {
  const resources = emptyResourceLoader("sealed");
  assertEquals(resources.getExtensions().extensions.length, 0);
  assertEquals(resources.getSkills().skills.length, 0);
  assertEquals(resources.getPrompts().prompts.length, 0);
  assertEquals(resources.getThemes().themes.length, 0);
  assertEquals(resources.getAgentsFiles().agentsFiles.length, 0);
  assertEquals(resources.getSystemPrompt(), "sealed");
  const secret = "should-not-be-persisted";
  const descriptor = { kind: "environment", name: "FAKE_PROVIDER_KEY" };
  assert(!JSON.stringify({ descriptor }).includes(secret));
  const common = createCommonToolAdapters({
    workspace_read: () => Promise.resolve({ ok: true }),
    shell: () => Promise.resolve({ ok: true }),
  });
  assertEquals(common.map((tool) => tool.name), ["workspace_read", "shell"]);
});

Deno.test("environment credential store is process-local and starts empty", async () => {
  const store = createEphemeralCredentialStore();
  assertEquals(await store.list(), []);
  assertEquals(await store.read("fake-provider"), undefined);
  await store.modify(
    "fake-provider",
    () => Promise.resolve({ type: "api_key" as const, key: "secret" }),
  );
  assertEquals((await store.read("fake-provider"))?.type, "api_key");
  await store.delete("fake-provider");
  assertEquals(await store.read("fake-provider"), undefined);
});

Deno.test("inherited actor admission frame is closed and exact", async () => {
  const path = await Deno.makeTempFile({ prefix: "pi-host-admission-" });
  await Deno.writeTextFile(
    path,
    '{"type":"session.admitted","protocol_version":1,"assignment_id":"assignment-1",' +
      '"session_id":7,"session_revision":0,"packet_digest":"' + "a".repeat(64) + '",' +
      '"packet_b64":"eyJwYWNrZXQiOnRydWV9"}\n',
  );
  const file = await Deno.open(path, { read: true });
  try {
    assertEquals((await readSessionAdmissionFrame(file)).session_id, 7);
  } finally {
    file.close();
  }
  await assertRejects(
    async () => {
      const invalid = await Deno.makeTempFile({ prefix: "pi-host-admission-invalid-" });
      await Deno.writeTextFile(invalid, '{"type":"session.admitted","extra":true}\n');
      const invalidFile = await Deno.open(invalid, { read: true });
      try {
        await readSessionAdmissionFrame(invalidFile);
      } finally {
        invalidFile.close();
      }
    },
    Error,
    "unknown or missing",
  );
});

Deno.test("host entrypoint consumes one attested packet before Pi construction", async () => {
  const root = await Deno.makeTempDir({ prefix: "pi-host-entrypoint-" });
  const source = packet(root, false, false);
  const bytes = new TextEncoder().encode(canonicalJson({
    format_version: 1,
    assignment_id: 22,
    office: source.office,
    campaign_id: 11,
    application_revision_id: Number(source.application_revision_id),
    kernel_build_id: source.kernel_build_id,
    repository_base_identity: source.repository_base_identity,
    factory_base_identity: source.factory_base_identity,
    target: source.target,
    ticket_attempt_id: Number(source.ticket_attempt_id),
    candidate_id: source.candidate_id === null ? null : Number(source.candidate_id),
    packet_digest: source.packet_digest,
    system_prompt_artifact_id: source.system_prompt_artifact_id,
    assignment_prompt_artifact_id: source.assignment_prompt_artifact_id,
    required_read_manifest_artifact_id: source.required_read_manifest_artifact_id,
    system_prompt_digest: source.system_prompt_digest,
    assignment_prompt_digest: source.assignment_prompt_digest,
    remaining_campaign_allowance_micro_usd: source.aggregate_cost_remaining_micro_usd,
    aggregate_revision: Number(source.aggregate_revision),
    terminal_operations: source.legal_terminal_operations,
    workspace_root: source.workspace_root,
    staging_root: source.staging_root,
    system_prompt_bytes_b64: encodeBase64(source.system_prompt_bytes),
    assignment_prompt_bytes_b64: encodeBase64(source.assignment_prompt_bytes),
    model: { ...source.model, capability_flags: [] },
    limits: source.limits,
    tools: source.tools,
    required_reads: source.required_reads.map((read) => ({
      path: read.canonical_path,
      digest: read.blake3,
      reason: read.reason,
    })),
    assignment_evidence: source.assignment_evidence,
    runtime: {
      ...source.runtime,
      credential_source: {
        kind: source.runtime.credential_source.kind,
        name: source.runtime.credential_source.kind === "environment"
          ? source.runtime.credential_source.name
          : null,
        path: source.runtime.credential_source.kind === "pi_auth_store"
          ? source.runtime.credential_source.path
          : null,
      },
    },
  }));
  const frame = {
    type: "session.admitted" as const,
    protocol_version: 1 as const,
    assignment_id: "22",
    session_id: 9,
    session_revision: 0,
    packet_digest: source.packet_digest,
    packet_b64: encodeBase64(bytes),
  };
  let verifiedBytes: Uint8Array | undefined;
  let created = false;
  const base = deps({
    create: async (admitted, context) => {
      created = true;
      assertEquals(admitted.assignment_id, "22");
      return await new FakeFactory([terminal(1)], async (tools) => {
        await tools.find((tool) => tool.name === "work_complete")?.sdk_definition.invoke({});
      }).create(admitted, context);
    },
  });
  const result = await runPiHostEntrypoint({
    ...base,
    authority: {
      file: undefined as unknown as Deno.FsFile,
      await_admission: () => Promise.resolve(frame),
      is_alive: () => true,
    },
    packet_integrity_verifier: (_packet, attestedBytes, expectedDigest) => {
      verifiedBytes = attestedBytes?.slice();
      assertEquals(expectedDigest, source.packet_digest);
      return Promise.resolve(attestedBytes?.byteLength === bytes.byteLength);
    },
  });
  assertEquals(decodeAssignmentPacketV1(bytes).assignment_id, "22");
  assertEquals(verifiedBytes, bytes);
  assertEquals(created, true);
  assertEquals(result.status, "succeeded");
});

Deno.test("host adapters are converted to the live Pi tool ABI", async () => {
  let seen: unknown;
  const definition = toPiToolDefinition({
    name: "artifact_seal",
    sdk_definition: {
      description: "seal",
      input_schema: { type: "object", additionalProperties: false },
      invoke: (input) => {
        seen = input;
        return Promise.resolve({ artifact_id: 7 });
      },
    },
  });
  const result = await definition.execute(
    "call-1",
    { path: "staging/file" },
    undefined,
    undefined,
    {} as never,
  );
  assertEquals(seen, { path: "staging/file" });
  assertEquals(result.content, [{ type: "text", text: '{"artifact_id":7}' }]);
  assertEquals(result.details, { artifact_id: 7 });

  assertThrows(
    () =>
      toPiToolDefinition({
        name: "artifact_seal",
        sdk_definition: {
          description: "Send bytes to the daemon.",
          input_schema: { type: "object", additionalProperties: false },
          invoke: () => Promise.resolve({ accepted: true }),
        },
      }),
    Error,
    "internal vocabulary",
  );

  const hiddenResult = toPiToolDefinition({
    name: "artifact_seal",
    sdk_definition: {
      description: "Seal assignment evidence.",
      input_schema: { type: "object", additionalProperties: false },
      invoke: () => Promise.resolve({ campaign_id: 7 }),
    },
  });
  await assertRejects(
    () => hiddenResult.execute("call-2", {}, undefined, undefined, {} as never),
    Error,
    "internal metadata",
  );
});

Deno.test("artifact reading requires sealed upstream assignment evidence", async () => {
  const root = await Deno.makeTempDir({ prefix: "pi-host-product-artifact-read-" });
  const source = packet(root, false, false);
  await assertRejects(
    () =>
      runAssignment(
        {
          ...source,
          office: "product_research",
          ticket_attempt_id: null,
          candidate_id: null,
          tools: ["artifact_read", "work_complete"],
          legal_terminal_operations: ["work_complete"],
          assignment_evidence: [],
        },
        deps(new FakeFactory([])),
      ),
    Error,
    "requires sealed upstream assignment evidence",
  );
});

function encodeBase64(bytes: Uint8Array): string {
  let binary = "";
  for (const byte of bytes) binary += String.fromCharCode(byte);
  return btoa(binary);
}

Deno.test("full-control SDK session constructs offline with the pinned catalog", async () => {
  const root = await Deno.makeTempDir({ prefix: "pi-host-sdk-" });
  Deno.env.set("FAKE_PROVIDER_KEY", "provider-secret");
  try {
    const value = packet(root, false, false);
    const session = await createSdkSession(
      {
        ...value,
        model: {
          provider: "amazon-bedrock",
          model_id: "amazon.nova-lite-v1:0",
          thinking_level: "none",
          context_token_limit: 300_000,
          output_token_limit: 8_192,
          price_input_micro_usd_per_million_tokens: 60_000,
          price_output_micro_usd_per_million_tokens: 240_000,
          price_cache_read_micro_usd_per_million_tokens: 15_000,
          price_cache_write_micro_usd_per_million_tokens: 0,
          capability_flags: [],
        },
      },
      {
        custom_tools: [
          "workspace_read",
          "shell",
          "work_complete",
        ].map((name) => ({
          name: name as "workspace_read" | "shell" | "work_complete",
          sdk_definition: {
            description: name,
            input_schema: { type: "object", additionalProperties: false },
            invoke: (input: unknown) => Promise.resolve(input),
          },
        })),
      },
    );
    assertEquals(typeof session.prompt, "function");
    session.dispose();
  } finally {
    Deno.env.delete("FAKE_PROVIDER_KEY");
  }
});

Deno.test("real Pi SDK faux provider prompts, invokes a bound tool, and emits terminal usage", async () => {
  const root = await Deno.makeTempDir({ prefix: "pi-host-faux-sdk-" });
  try {
    const faux = fauxProvider();
    faux.setResponses([
      fauxAssistantMessage(
        [fauxToolCall("workspace_read", { repository_relative_path: "AGENTS.md" })],
        { stopReason: "toolUse" },
      ),
      fauxAssistantMessage([fauxText("complete")]),
    ]);
    const runtime = await ModelRuntime.create({
      credentials: createEphemeralCredentialStore(),
      authPath: undefined,
      modelsPath: null,
      allowModelNetwork: false,
      refreshOnCreate: false,
    });
    runtime.registerNativeProvider(faux.provider);
    const model = faux.models[0];
    const value = packet(root, false, false);
    const invoked: unknown[] = [];
    const session = await createSdkSessionWithRuntimeForTest(
      {
        ...value,
        model: {
          ...value.model,
          provider: model.provider,
          model_id: model.id,
          thinking_level: "none",
          context_token_limit: model.contextWindow,
          output_token_limit: model.maxTokens,
          price_input_micro_usd_per_million_tokens: 0,
          price_output_micro_usd_per_million_tokens: 0,
          price_cache_read_micro_usd_per_million_tokens: 0,
          price_cache_write_micro_usd_per_million_tokens: 0,
        },
      },
      {
        custom_tools: [{
          name: "workspace_read",
          sdk_definition: {
            description: "bound workspace read",
            input_schema: {
              type: "object",
              additionalProperties: false,
              required: ["repository_relative_path"],
              properties: { repository_relative_path: { type: "string" } },
            },
            invoke: (input) => {
              invoked.push(input);
              return Promise.resolve({ blake3: "d".repeat(64) });
            },
          },
        }],
      },
      runtime,
    );
    const events: unknown[] = [];
    const unsubscribe = session.subscribe((event) => events.push(event));
    await session.prompt("perform the assigned exact read");
    unsubscribe();
    session.dispose();
    assertEquals(invoked, [{ repository_relative_path: "AGENTS.md" }]);
    assert(events.some((event) => JSON.stringify(event).includes("usage")));
  } finally {
    await Deno.remove(root, { recursive: true });
  }
});

Deno.test("real Pi SDK faux provider completes a sealed host assignment through workspace read and terminal submission", async () => {
  const root = await Deno.makeTempDir({ prefix: "pi-host-faux-assignment-" });
  try {
    const faux = fauxProvider();
    const providerContexts: Array<{
      systemPrompt: string | undefined;
      messages: readonly { role: string; content: unknown }[];
      toolNames: readonly string[];
    }> = [];
    const providerModels: Array<{ provider: string; id: string }> = [];
    const capture = (response: ReturnType<typeof fauxAssistantMessage>) =>
    (
      context: Context,
      _options: unknown,
      _state: unknown,
      selectedModel: { provider: string; id: string },
    ) => {
      providerContexts.push({
        systemPrompt: context.systemPrompt,
        messages: context.messages.map((message) => ({
          role: message.role,
          content: structuredClone(message.content),
        })),
        toolNames: context.tools?.map((tool) => tool.name) ?? [],
      });
      providerModels.push({ provider: selectedModel.provider, id: selectedModel.id });
      return response;
    };
    faux.setResponses([
      capture(fauxAssistantMessage(
        [fauxToolCall("workspace_read", { repository_relative_path: "AGENTS.md" })],
        { stopReason: "toolUse" },
      )),
      capture(fauxAssistantMessage([fauxToolCall("work_complete", {})], { stopReason: "toolUse" })),
      capture(fauxAssistantMessage([fauxText("assignment complete")])),
    ]);
    const runtime = await ModelRuntime.create({
      credentials: createEphemeralCredentialStore(),
      authPath: undefined,
      modelsPath: null,
      allowModelNetwork: false,
      refreshOnCreate: false,
    });
    runtime.registerNativeProvider(faux.provider);
    const model = faux.models[0];
    const source = packet(root, true, true);
    const assignment = {
      ...source,
      model: {
        ...source.model,
        provider: model.provider,
        model_id: model.id,
        thinking_level: "none" as const,
        context_token_limit: model.contextWindow,
        output_token_limit: model.maxTokens,
        price_input_micro_usd_per_million_tokens: 0,
        price_output_micro_usd_per_million_tokens: 0,
        price_cache_read_micro_usd_per_million_tokens: 0,
        price_cache_write_micro_usd_per_million_tokens: 0,
      },
      tools: ["workspace_read", "work_complete"] as const,
      legal_terminal_operations: ["work_complete"] as const,
    };
    let createdPacket: PiAssignmentPacket | undefined;
    let sdkSession: Awaited<ReturnType<typeof createSdkSessionWithRuntimeForTest>> | undefined;
    let disposed = false;
    const factory: PiSessionFactory = {
      create: async (admitted, context) => {
        createdPacket = admitted;
        const session = await createSdkSessionWithRuntimeForTest(admitted, context, runtime);
        sdkSession = session;
        return {
          subscribe: (listener) => session.subscribe(listener),
          prompt: (prompt) => session.prompt(prompt),
          dispose: () => {
            disposed = true;
            session.dispose();
          },
          abort: () => session.abort(),
        };
      },
    };
    const sealer = new FakeSealer();
    const workspaceReads: unknown[] = [];
    const verifiedReadResults: unknown[] = [];
    const terminals: Array<{ operation: string | null; payload: unknown }> = [];
    const result = await runAssignment(assignment, {
      ...deps(factory, sealer),
      custom_tools: [
        {
          name: "workspace_read",
          sdk_definition: {
            description: "exact workspace read",
            input_schema: {
              type: "object",
              additionalProperties: false,
              required: ["repository_relative_path"],
              properties: { repository_relative_path: { type: "string", minLength: 1 } },
            },
            invoke: (input) => {
              workspaceReads.push(input);
              return Promise.resolve({ bytes: "contract bytes", blake3: "d".repeat(64) });
            },
          },
        },
        {
          name: "work_complete",
          sdk_definition: {
            description: "host terminal capture",
            input_schema: { type: "object", additionalProperties: false },
            invoke: (input) => Promise.resolve(input),
          },
        },
      ],
      required_read_verifier: {
        verify: (result) => {
          verifiedReadResults.push(result);
          return Promise.resolve({
            canonical_path: "AGENTS.md",
            blake3: "d".repeat(64),
            success: true,
          });
        },
      },
      terminal_submission: {
        submit: (operation, payload, manifest, summary) => {
          terminals.push({ operation, payload });
          assertEquals(manifest.required.length, 1);
          assertEquals(summary.cost_status, "known");
          return Promise.resolve();
        },
      },
      runtime: { deno_version: "2.9.4", pi_sdk_version: "0.84.1" },
    });

    assertEquals(result.status, "succeeded");
    assertEquals(createdPacket?.assignment_prompt_bytes, assignment.assignment_prompt_bytes);
    assertEquals(createdPacket?.model.provider, model.provider);
    assertEquals(createdPacket?.runtime.pi_version, "0.84.1");
    assertEquals(faux.state.callCount, 3);
    assertEquals(faux.getPendingResponseCount(), 0);
    assertEquals(
      providerModels,
      Array.from({ length: 3 }, () => ({ provider: model.provider, id: model.id })),
    );
    assertEquals(sdkSession?.systemPrompt, "sealed system");
    assertEquals(providerContexts[0]?.systemPrompt, "sealed system");
    assertEquals(providerContexts[0]?.messages.length, 1);
    assertEquals(providerContexts[0]?.messages[0]?.role, "user");
    assertEquals(providerContexts[0]?.messages[0]?.content, [{
      type: "text",
      text: "sealed assignment",
    }]);
    assertEquals(providerContexts[0]?.toolNames, ["workspace_read", "work_complete"]);
    assertEquals(workspaceReads, [{ repository_relative_path: "AGENTS.md" }]);
    assertEquals(verifiedReadResults.length, 1);
    assertEquals(result.required_read_manifest.satisfied, [{
      canonical_path: "AGENTS.md",
      blake3: "d".repeat(64),
      reason: "contract",
    }]);
    assertEquals(terminals, [{ operation: "work_complete", payload: {} }]);
    assertEquals(sealer.roles, ["pi_transcript_gzip"]);
    assertEquals(result.summary.transcript_artifact?.digest, "digest-1");
    assertEquals(result.summary.cost_status, "known");
    assertEquals(result.summary.cost_micro_usd, 0);
    const transcript = await new Response(
      new Blob([await Deno.readFile(result.summary.transcript_gzip_path)]).stream().pipeThrough(
        new DecompressionStream("gzip"),
      ),
    ).text();
    assert(transcript.includes("workspace_read"));
    assert(transcript.includes("work_complete"));
    assertEquals(disposed, true);
  } finally {
    await Deno.remove(root, { recursive: true });
  }
});

Deno.test("model descriptor rejects capability and thinking-map absence or drift", () => {
  const sourcePacket = packet("/tmp/factory-model-descriptor", false);
  const packetValue = {
    ...sourcePacket,
    model: {
      ...sourcePacket.model,
      thinking_level: "none" as const,
      capability_flags: [],
    },
  };
  const model = {
    provider: packetValue.model.provider,
    id: packetValue.model.model_id,
    contextWindow: packetValue.model.context_token_limit,
    maxTokens: packetValue.model.output_token_limit,
    reasoning: false,
    cost: {
      input: packetValue.model.price_input_micro_usd_per_million_tokens / 1_000_000,
      output: packetValue.model.price_output_micro_usd_per_million_tokens / 1_000_000,
      cacheRead: packetValue.model.price_cache_read_micro_usd_per_million_tokens / 1_000_000,
      cacheWrite: packetValue.model.price_cache_write_micro_usd_per_million_tokens / 1_000_000,
    },
  };
  verifyModelDescriptor(model, packetValue);
  assertThrows(
    () => verifyModelDescriptor({ ...model, reasoning: true }, packetValue),
    Error,
    "capabilities drifted",
  );
  const reasoningPacket = {
    ...packetValue,
    model: {
      ...packetValue.model,
      thinking_level: "high" as const,
      capability_flags: ["reasoning" as const],
    },
  };
  assertThrows(
    () => verifyModelDescriptor({ ...model, reasoning: true }, reasoningPacket),
    Error,
    "thinking level",
  );
  verifyModelDescriptor(
    { ...model, reasoning: true, thinkingLevelMap: { high: "high" } },
    reasoningPacket,
  );
});

Deno.test("missing terminal submission adapter refuses work completion", async () => {
  const root = await Deno.makeTempDir({ prefix: "pi-host-submit-" });
  const result = await runAssignment(packet(root, false, true), {
    ...deps(new FakeFactory([terminal(1)])),
    terminal_submission: undefined,
  });
  assertEquals(result.status, "terminal_submission_missing");
});

Deno.test("terminal submission requires exactly one legal model tool invocation", async () => {
  const root = await Deno.makeTempDir({ prefix: "pi-host-terminal-gate-" });
  let calls = 0;
  const noInvocation = await runAssignment(packet(root, false, true), {
    ...deps(new FakeFactory([terminal(1)])),
    terminal_submission: {
      submit: (_operation, _payload, _manifest, _summary) => {
        calls += 1;
        return Promise.resolve();
      },
    },
  });
  assertEquals(noInvocation.status, "failed");
  assertEquals(calls, 1);

  const duplicate = await runAssignment(packet(root, false, true), {
    ...deps(
      new FakeFactory([terminal(1)], async (tools) => {
        const tool = tools.find((item) => item.name === "work_complete")!;
        await tool.sdk_definition.invoke({ first: true });
        await tool.sdk_definition.invoke({ second: true });
      }),
    ),
    terminal_submission: {
      submit: (_operation, _payload, _manifest, _summary) => {
        calls += 1;
        return Promise.resolve();
      },
    },
  });
  assertEquals(duplicate.status, "failed");
  assertEquals(calls, 2);

  const illegal = await runAssignment(packet(root, false, true), {
    ...deps(
      new FakeFactory([
        {
          type: "tool_execution_end",
          tool_name: "quality_submit_review",
          isError: false,
          result: {},
        },
        terminal(1),
      ]),
    ),
    terminal_submission: {
      submit: (_operation, _payload, _manifest, _summary) => {
        calls += 1;
        return Promise.resolve();
      },
    },
  });
  assertEquals(illegal.status, "failed");
  assertEquals(calls, 3);
});

Deno.test("Candidate and Quality terminal tools call their daemon adapter before terminal capture", async () => {
  for (const operation of ["candidate_submit", "quality_submit_review"] as const) {
    const root = await Deno.makeTempDir({ prefix: `pi-host-${operation}-` });
    const source = packet(root, false, true);
    const target = operation === "candidate_submit"
      ? { ...source, legal_terminal_operations: [operation], tools: [operation] }
      : {
        ...source,
        office: "quality",
        candidate_id: "23",
        legal_terminal_operations: [operation],
        tools: [operation],
      };
    const calls: string[] = [];
    let terminalOperation: string | null | undefined;
    const result = await runAssignment(target, {
      ...deps(
        new FakeFactory([terminal(1)], async (tools) => {
          await tools.find((tool) => tool.name === operation)?.sdk_definition.invoke({
            proof: operation,
          });
        }),
      ),
      custom_tools: [{
        name: operation,
        sdk_definition: {
          description: "submit terminal result",
          input_schema: { type: "object", additionalProperties: false },
          invoke: (input: unknown) => {
            calls.push(JSON.stringify(input));
            return Promise.resolve({ durable: true });
          },
        },
      }],
      terminal_submission: {
        submit: (submittedOperation) => {
          terminalOperation = submittedOperation;
          return Promise.resolve();
        },
      },
    });
    assertEquals(result.status, "succeeded");
    assertEquals(calls, [JSON.stringify({ proof: operation })]);
    assertEquals(terminalOperation, operation);
  }
});

Deno.test("failed Candidate daemon terminal is not captured for session terminal submission", async () => {
  const root = await Deno.makeTempDir({ prefix: "pi-host-candidate-failure-" });
  const source = packet(root, false, true);
  let terminalOperation: string | null | undefined;
  const result = await runAssignment(
    { ...source, legal_terminal_operations: ["candidate_submit"], tools: ["candidate_submit"] },
    {
      ...deps(
        new FakeFactory([terminal(1)], async (tools) => {
          await assertRejects(
            () => tools.find((tool) => tool.name === "candidate_submit")!.sdk_definition.invoke({}),
            Error,
            "durable rejection",
          );
        }),
      ),
      custom_tools: [{
        name: "candidate_submit",
        sdk_definition: {
          description: "submit terminal result",
          input_schema: { type: "object", additionalProperties: false },
          invoke: () => Promise.reject(new Error("durable rejection")),
        },
      }],
      terminal_submission: {
        submit: (operation) => {
          terminalOperation = operation;
          return Promise.resolve();
        },
      },
    },
  );
  assertEquals(result.status, "failed");
  assertEquals(terminalOperation, null);
});

Deno.test("output byte limit aborts the one session and remains explicit", async () => {
  const root = await Deno.makeTempDir({ prefix: "pi-host-output-limit-" });
  const factory = new FakeFactory([
    {
      type: "message_update",
      assistantMessageEvent: { type: "text_delta", delta: "four" },
    },
    terminal(1),
  ]);
  const result = await runAssignment(
    { ...packet(root, false), limits: { ...packet(root, false).limits, output_byte_limit: 3 } },
    deps(factory),
  );
  assertEquals(result.status, "failed");
  assertEquals(result.summary.stop_reason, "output_limit");
  assertEquals(result.summary.output_bytes, 4);
  assertEquals(factory.lastSession?.aborted, true);
});

Deno.test("turn, wall, and aggregate allowance limits halt before success", async () => {
  const turnRoot = await Deno.makeTempDir({ prefix: "pi-host-turn-limit-" });
  const turn = await runAssignment(
    {
      ...packet(turnRoot, false, false),
      limits: { ...packet(turnRoot, false, false).limits, turn_limit: 1 },
    },
    deps(new FakeFactory([{ type: "turn_end" }, terminal(1)])),
  );
  assertEquals(turn.status, "failed");
  assertEquals(turn.summary.stop_reason, "turn_limit");

  const wallRoot = await Deno.makeTempDir({ prefix: "pi-host-wall-limit-" });
  const wallFactory = new FakeFactory([terminal(1)], async () => {
    await new Promise((resolve) => setTimeout(resolve, 20));
  });
  const wall = await runAssignment(
    {
      ...packet(wallRoot, false, false),
      limits: { ...packet(wallRoot, false, false).limits, wall_limit_millis: 1 },
    },
    deps(wallFactory),
  );
  assertEquals(wall.status, "failed");
  assertEquals(wall.summary.stop_reason, "wall_limit");

  const costRoot = await Deno.makeTempDir({ prefix: "pi-host-cost-limit-" });
  const cost = await runAssignment(
    packet(costRoot, false, false),
    deps(new FakeFactory([{ type: "usage", cost_usd: 0.0002 }, terminal(1)])),
  );
  assertEquals(cost.status, "failed");
  assertEquals(cost.summary.stop_reason, "aggregate_cost");
});

Deno.test("artifact sealer failures remain explicit", async () => {
  const root = await Deno.makeTempDir({ prefix: "pi-host-seal-" });
  await assertRejects(
    () =>
      runAssignment(packet(root, false, true), {
        ...deps(new FakeFactory([terminal(1)])),
        artifact_sealer: { seal: () => Promise.reject(new Error("seal failed")) },
      }),
    Error,
    "seal failed",
  );
});

Deno.test("partial transcript gzip remains readable and tool allowlists are exact", async () => {
  const root = await Deno.makeTempDir({ prefix: "pi-host-partial-" });
  const source = join(root, "partial.ndjson");
  const destination = join(root, "partial.ndjson.gz");
  await Deno.writeTextFile(source, '{"sequence":0}\n');
  await gzipFile(source, destination);
  const bytes = await Deno.readFile(destination);
  assertEquals(
    await new Response(new Blob([bytes]).stream().pipeThrough(new DecompressionStream("gzip")))
      .text(),
    '{"sequence":0}\n',
  );
  const packetValue = packet(root, false);
  const adapter = {
    name: "candidate_submit" as const,
    sdk_definition: {
      description: "seal",
      input_schema: { type: "object", additionalProperties: false },
      invoke: (input: unknown) => Promise.resolve(input),
    },
  };
  const artifactPacket = {
    ...packetValue,
    tools: ["candidate_submit"] as const,
    legal_terminal_operations: ["candidate_submit"] as const,
  };
  assertEquals(validateToolAllowlist(artifactPacket, [adapter]), [adapter]);
  await assertRejects(
    () => {
      try {
        validateToolAllowlist(artifactPacket, []);
        return Promise.resolve();
      } catch (error) {
        return Promise.reject(error);
      }
    },
    Error,
    "no bound adapter",
  );
});

Deno.test("workspace mutation, search, list, and shell use only pinned Pi builtins", () => {
  const value = packet("/tmp/factory-host-builtins", false);
  const names = [
    "workspace_write",
    "workspace_edit",
    "workspace_search",
    "workspace_list",
    "shell",
  ] as const;
  assertEquals(
    builtinPiToolNames({ ...value, tools: names }),
    ["write", "edit", "grep", "ls", "bash"],
  );
});
