/** The only production Pi construction path: full-control, resource-empty SDK setup. */
import {
  type AgentSession,
  createAgentSession,
  createExtensionRuntime,
  createSyntheticSourceInfo,
  type Extension,
  ModelRuntime,
  type ResourceLoader,
  SessionManager,
  SettingsManager,
  type ToolDefinition,
} from "@factory/pi-headless";
import { builtinPiToolNames } from "./host.ts";
import type {
  PiAssignmentPacket,
  PiSessionFactory,
  PiSessionFactoryContext,
  PiToolAdapter,
} from "./types.ts";

export function createSdkPiSessionFactory(): PiSessionFactory {
  return {
    create: (packet, context) => createSdkSession(packet, context),
  };
}

/** Converts our provider-neutral adapter shape into Pi 0.84.1's live tool ABI. */
export function toPiToolDefinition(adapter: PiToolAdapter): ToolDefinition {
  const descriptor = adapter.sdk_definition;
  assertModelVisibleToolDescriptor(adapter);
  return {
    name: adapter.name,
    label: adapter.name,
    description: descriptor.description,
    parameters: descriptor.input_schema,
    execute: async (_toolCallId, params) => {
      const details = await descriptor.invoke(params);
      assertModelVisibleResultStructure(details);
      const text = JSON.stringify(details);
      return {
        content: [{ type: "text", text: text === undefined ? "null" : text }],
        details,
      };
    },
  };
}

const MODEL_HIDDEN_VOCABULARY =
  /\b(?:architect|campaign|compan(?:y|ies)|control\s+plane|cto|daemon|department|director|employee|factory|institution(?:s|al|ally)?|kernel|manager|office|organization(?:s|al|ally)?|sponsor(?:ed|ship)?)\b/iu;

/** Tool metadata is sent in every provider request. Reject a descriptor that
 * turns internal organization or runtime structure into a worker metaphor. */
export function assertModelVisibleToolDescriptor(adapter: PiToolAdapter): void {
  const visible = JSON.stringify({
    name: adapter.name,
    description: adapter.sdk_definition.description,
    input_schema: adapter.sdk_definition.input_schema,
  }).replaceAll("_", " ").replaceAll("-", " ");
  if (MODEL_HIDDEN_VOCABULARY.test(visible)) {
    throw new Error("custom tool metadata contains unavailable internal vocabulary");
  }
}

/** Result payload text is evidence and must remain untouched. Structural keys,
 * however, are host-authored model context and cannot expose internal roles or
 * lifecycle identities. */
function assertModelVisibleResultStructure(value: unknown): void {
  if (Array.isArray(value)) {
    for (const item of value) assertModelVisibleResultStructure(item);
    return;
  }
  if (value === null || typeof value !== "object") return;
  for (const [key, item] of Object.entries(value as Record<string, unknown>)) {
    if (MODEL_HIDDEN_VOCABULARY.test(key.replaceAll("_", " ").replaceAll("-", " "))) {
      throw new Error("tool result contains unavailable internal metadata");
    }
    assertModelVisibleResultStructure(item);
  }
}

export async function createSdkSession(
  packet: PiAssignmentPacket,
  context: PiSessionFactoryContext,
): Promise<AgentSession> {
  const modelRuntime = await createProductionModelRuntime(packet);
  return await createSdkSessionWithRuntimeForTest(packet, context, modelRuntime);
}

/** Production-only runtime construction. Test code may construct a native
 * faux provider runtime and call the narrow helper below; packet/provider
 * selection remains unchanged in the production factory. */
async function createProductionModelRuntime(packet: PiAssignmentPacket): Promise<ModelRuntime> {
  const credential = packet.runtime.credential_source;
  const modelRuntime = await ModelRuntime.create({
    // Supplying an explicit store is essential for named environment
    // credentials: leaving credentials undefined makes Pi discover its
    // default auth.json even when authPath is undefined.
    credentials: credential.kind === "environment" ? createEphemeralCredentialStore() : undefined,
    authPath: credential.kind === "pi_auth_store" ? credential.path : undefined,
    modelsPath: null,
    allowModelNetwork: false,
    refreshOnCreate: false,
  });
  if (credential.kind === "environment") {
    const apiKey = Deno.env.get(credential.name);
    if (apiKey === undefined || apiKey.length === 0) {
      throw new Error("selected provider environment credential is unavailable");
    }
    try {
      await modelRuntime.setRuntimeApiKey(packet.model.provider, apiKey);
    } finally {
      // Pi now owns the process-local credential. Removing the environment
      // source prevents later actor shell tools from inheriting the secret.
      Deno.env.delete(credential.name);
    }
  }
  return modelRuntime;
}

/**
 * Test-only construction seam for Pi's native provider runtime. It exists so
 * provider-free tests exercise the real `createAgentSession` path, including
 * custom-tool dispatch and terminal SDK events, without widening production
 * provider selection or credential authority.
 */
export async function createSdkSessionWithRuntimeForTest(
  packet: PiAssignmentPacket,
  context: PiSessionFactoryContext,
  modelRuntime: ModelRuntime,
): Promise<AgentSession> {
  const model = modelRuntime.getModel(packet.model.provider, packet.model.model_id);
  if (model === undefined) throw new Error("pinned model is not present in the offline Pi catalog");
  verifyModelDescriptor(model, packet);

  const systemPrompt = new TextDecoder("utf-8", { fatal: true }).decode(packet.system_prompt_bytes);
  const resourceLoader = sealedAssignmentResourceLoader(systemPrompt, packet.workspace_root);
  const settingsManager = SettingsManager.inMemory({
    compaction: { enabled: false },
    // A provider call that produces no bytes must not occupy the factory's
    // single paid-session slot until the much broader assignment wall limit.
    // Pi applies this setting to each model request, including the initial
    // response and every tool-follow-up. The host keeps retries disabled so a
    // timeout is a truthful terminal failure, not hidden additional spend.
    retry: { enabled: false, maxRetries: 0, provider: { timeoutMs: 90_000, maxRetries: 0 } },
    extensions: [],
    skills: [],
    prompts: [],
    themes: [],
    packages: [],
    enableInstallTelemetry: false,
    enableAnalytics: false,
  });
  const customTools = context.custom_tools.map(toPiToolDefinition);
  const { session } = await createAgentSession({
    cwd: packet.workspace_root,
    agentDir: `${packet.staging_root}/pi-agent`,
    model,
    thinkingLevel: packet.model.thinking_level === "none" ? "off" : packet.model.thinking_level,
    scopedModels: [],
    tools: [...builtinPiToolNames(packet), ...context.custom_tools.map((adapter) => adapter.name)],
    customTools,
    resourceLoader,
    sessionManager: SessionManager.inMemory(packet.workspace_root),
    settingsManager,
    modelRuntime,
  });
  return session;
}

/**
 * Minimal process-local CredentialStore for an environment-selected key.
 * Runtime API keys are installed by ModelRuntime and never written to disk.
 * This deliberately does not resolve, persist, or enumerate ambient secrets.
 */
type EphemeralCredential =
  | { type: "api_key"; key?: string; env?: Record<string, string> }
  | { type: "oauth"; refresh: string; access: string; expires: number; [key: string]: unknown };

interface EphemeralCredentialStore {
  read(providerId: string): Promise<EphemeralCredential | undefined>;
  list(): Promise<readonly { providerId: string; type: "api_key" | "oauth" }[]>;
  modify(
    providerId: string,
    update: (current: EphemeralCredential | undefined) => Promise<EphemeralCredential | undefined>,
  ): Promise<EphemeralCredential | undefined>;
  delete(providerId: string): Promise<void>;
}

export function createEphemeralCredentialStore(): EphemeralCredentialStore {
  const credentials = new Map<string, EphemeralCredential>();
  const chains = new Map<string, Promise<void>>();
  return {
    read: (providerId: string) => Promise.resolve(credentials.get(providerId)),
    list: () => Promise.resolve([] as readonly { providerId: string; type: "api_key" | "oauth" }[]),
    modify: async (
      providerId: string,
      update: (
        current: EphemeralCredential | undefined,
      ) => Promise<EphemeralCredential | undefined>,
    ) => {
      const prior = chains.get(providerId) ?? Promise.resolve();
      let resolveChain!: () => void;
      const currentChain = new Promise<void>((resolve) => resolveChain = resolve);
      chains.set(providerId, currentChain);
      await prior;
      try {
        const next = await update(credentials.get(providerId));
        if (next !== undefined) credentials.set(providerId, next);
        return next;
      } finally {
        resolveChain();
        if (chains.get(providerId) === currentChain) chains.delete(providerId);
      }
    },
    delete: (providerId: string) => {
      credentials.delete(providerId);
      return Promise.resolve();
    },
  };
}

/** Every ambient resource category is explicitly empty. */
export function emptyResourceLoader(systemPrompt: string): ResourceLoader {
  return {
    getExtensions: () => ({ extensions: [], errors: [], runtime: createExtensionRuntime() }),
    getSkills: () => ({ skills: [], diagnostics: [] }),
    getPrompts: () => ({ prompts: [], diagnostics: [] }),
    getThemes: () => ({ themes: [], diagnostics: [] }),
    getAgentsFiles: () => ({ agentsFiles: [] }),
    getSystemPrompt: () => systemPrompt,
    getSystemPromptSource: () => undefined,
    getAppendSystemPrompt: () => [],
    getAppendSystemPromptSources: () => [],
    extendResources: () => {},
    reload: async () => {},
  };
}

/**
 * This one inline extension is a host envelope, not discovered extension
 * code.  It has exactly one handler, performs no I/O, registers no tools or
 * providers, and replaces each turn's system prompt with the sealed bytes
 * plus the exact shell workspace before Pi makes a provider request.  The
 * path is packet-owned operational context, not a source-discovery hint: an
 * actor that cannot name its worktree wastes turns searching the host and may
 * inspect the wrong checkout. All ambient extension resources remain empty.
 */
function sealedAssignmentResourceLoader(
  systemPrompt: string,
  cwd: string,
): ResourceLoader {
  const prompt =
    `${systemPrompt}\n\nYour shell starts in this assigned workspace: ${cwd}\nRun assignment commands there. Do not search for or switch to another checkout.`;
  const runtime = createExtensionRuntime();
  // `createAgentSession` consumes an already-loaded extension collection.
  // Construct exactly one closed handler in memory rather than invoking Pi's
  // extension loader/discovery path. The erased handler map is Pi's internal
  // heterogeneous-event representation; its sole value is the typed,
  // no-input/no-I/O system-prompt replacement below.
  const extension = {
    path: "<sealed-assignment-envelope>",
    resolvedPath: "<sealed-assignment-envelope>",
    hidden: true,
    sourceInfo: createSyntheticSourceInfo("<sealed-assignment-envelope>", {
      source: "host",
    }),
    handlers: new Map([[
      "before_agent_start",
      [() => Promise.resolve({ systemPrompt: prompt })],
    ]]),
    tools: new Map(),
    messageRenderers: new Map(),
    entryRenderers: new Map(),
    commands: new Map(),
    flags: new Map(),
    shortcuts: new Map(),
  } as Extension;
  return {
    getExtensions: () => ({ extensions: [extension], errors: [], runtime }),
    getSkills: () => ({ skills: [], diagnostics: [] }),
    getPrompts: () => ({ prompts: [], diagnostics: [] }),
    getThemes: () => ({ themes: [], diagnostics: [] }),
    getAgentsFiles: () => ({ agentsFiles: [] }),
    getSystemPrompt: () => prompt,
    getSystemPromptSource: () => undefined,
    getAppendSystemPrompt: () => [],
    getAppendSystemPromptSources: () => [],
    extendResources: () => {},
    reload: async () => {},
  };
}

export function verifyModelDescriptor(
  model: {
    provider: string;
    id: string;
    contextWindow: number;
    maxTokens: number;
    reasoning: boolean;
    cost: { input: number; output: number; cacheRead: number; cacheWrite: number };
    thinkingLevelMap?: Record<string, string | null>;
  },
  packet: PiAssignmentPacket,
): void {
  if (model.provider !== packet.model.provider || model.id !== packet.model.model_id) {
    throw new Error("Pi selected a different model");
  }
  if (
    model.contextWindow !== packet.model.context_token_limit ||
    model.maxTokens !== packet.model.output_token_limit
  ) throw new Error("Pi model limits drifted from the assignment");
  if (
    Math.round(model.cost.input * 1_000_000) !==
      packet.model.price_input_micro_usd_per_million_tokens ||
    Math.round(model.cost.output * 1_000_000) !==
      packet.model.price_output_micro_usd_per_million_tokens ||
    Math.round(model.cost.cacheRead * 1_000_000) !==
      packet.model.price_cache_read_micro_usd_per_million_tokens ||
    Math.round(model.cost.cacheWrite * 1_000_000) !==
      packet.model.price_cache_write_micro_usd_per_million_tokens
  ) throw new Error("Pi model prices drifted from the assignment");
  const expectsReasoning = packet.model.capability_flags.includes("reasoning");
  if (model.reasoning !== expectsReasoning) {
    throw new Error("Pi model capabilities drifted from the assignment");
  }
  if (packet.model.thinking_level !== "none") {
    const thinking = model.thinkingLevelMap?.[packet.model.thinking_level];
    if (!expectsReasoning || thinking === undefined || thinking === null) {
      throw new Error("Pi model does not support the assigned thinking level");
    }
  }
}
