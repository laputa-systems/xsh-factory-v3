import { gzipFile, transcriptPaths, TranscriptWriter, writeManifest } from "./transcript.ts";
import { projectHeadlessAuditEvent } from "@factory/pi-headless";
import type {
  ArtifactSealer,
  AuthorityLiveness,
  HostToolName,
  NormalizedSessionSummary,
  PiAssignmentPacket,
  PiHostResult,
  PiSessionFactory,
  PiToolAdapter,
  RequiredReadManifest,
  RequiredReadObservation,
  RequiredReadVerifier,
  TerminalSubmission,
} from "./types.ts";

/**
 * Host-local common tools use Pi's pinned SDK implementations under the
 * packet's generic names. The Deno process is intentionally a cooperative,
 * full-host actor (not a filesystem sandbox); the spawn cwd supplies the
 * assigned workspace. `workspace_read` remains daemon-bound because only its
 * connection-owned ledger can satisfy a required-read assertion.
 */
const BUILTIN_TOOL_NAMES: Readonly<Record<string, string>> = {
  workspace_write: "write",
  workspace_edit: "edit",
  workspace_search: "grep",
  workspace_list: "ls",
  shell: "bash",
};

const KNOWN_TOOL_NAMES = new Set([
  "workspace_read",
  "workspace_write",
  "workspace_edit",
  "workspace_search",
  "workspace_list",
  "shell",
  "forum_read",
  "forum_write",
  "forum_search",
  "forum_list_topics",
  "forum_list_threads",
  "forum_read_thread",
  "forum_create_topic",
  "forum_create_thread",
  "forum_post",
  "artifact_seal",
  "artifact_read",
  "product_submit_ticket",
  "candidate_checkpoint_regression",
  "candidate_submit",
  "quality_run_full_suite",
  "quality_submit_review",
  "work_complete",
]);
const TERMINAL_TOOL_NAMES = new Set([
  "candidate_submit",
  "quality_submit_review",
  "work_complete",
]);

interface MutableMetrics {
  turns: number;
  output_bytes: number;
  running_cost_micro_usd: number;
  tool_calls: number;
  nonzero_tool_results: number;
  retries: number;
  tokens: {
    input: number;
    output: number;
    cache_read: number;
    cache_write: number;
    reasoning: number | null;
  };
}

interface TerminalUsage {
  stop_reason?: unknown;
  stopReason?: unknown;
  failure_reason?: unknown;
  failureReason?: unknown;
  cost_micro_usd?: unknown;
  costMicroUsd?: unknown;
  cost_usd?: unknown;
  costUsd?: unknown;
  turns?: unknown;
  tokens?: Record<string, unknown>;
  usage?: Record<string, unknown>;
  messages?: readonly unknown[];
}

export interface PiHostDependencies {
  readonly session_factory: PiSessionFactory;
  readonly artifact_sealer: ArtifactSealer;
  /**
   * Cross-checks the daemon-attested packet. Entrypoint callers receive the
   * immutable canonical bytes and out-of-band digest; ordinary host calls
   * pass only the already-admitted packet object.
   */
  readonly packet_integrity_verifier: (
    packet: PiAssignmentPacket,
    canonical_bytes?: Uint8Array,
    expected_digest?: string,
  ) => Promise<boolean>;
  readonly required_read_verifier?: RequiredReadVerifier;
  readonly authority: AuthorityLiveness;
  readonly terminal_submission?: TerminalSubmission;
  readonly runtime?: {
    readonly deno_version?: string;
    readonly pi_sdk_version?: string;
  };
  readonly custom_tools?: readonly PiToolAdapter[];
}

/**
 * Runs exactly one fresh assignment. The factory is injected so all ordinary
 * tests use a realistic event stream without constructing a provider client.
 */
export async function runAssignment(
  packet: PiAssignmentPacket,
  dependencies: PiHostDependencies,
): Promise<PiHostResult> {
  validatePacket(packet);
  if (!(await dependencies.packet_integrity_verifier(packet))) {
    throw new Error("assignment packet digest does not match");
  }
  const promptDecoder = new TextDecoder("utf-8", { fatal: true });
  promptDecoder.decode(packet.system_prompt_bytes);
  const assignmentPrompt = promptDecoder.decode(packet.assignment_prompt_bytes);
  if (packet.required_reads.length > 0 && dependencies.required_read_verifier === undefined) {
    throw new Error("required-read verifier is required for assignments with read assertions");
  }
  const customTools = validateToolAllowlist(packet, dependencies.custom_tools ?? []);
  const paths = transcriptPaths(packet.staging_root);
  const writer = await TranscriptWriter.open(paths.ndjson, packet.limits.output_byte_limit);
  const observed: RequiredReadObservation[] = [];
  const metrics: MutableMetrics = {
    turns: 0,
    output_bytes: 0,
    running_cost_micro_usd: 0,
    tool_calls: 0,
    nonzero_tool_results: 0,
    retries: 0,
    tokens: { input: 0, output: 0, cache_read: 0, cache_write: 0, reasoning: null },
  };
  let terminal: TerminalUsage | undefined;
  let terminalInvocation: { operation: HostToolName; payload: unknown } | undefined;
  let terminalInvocationInFlight = false;
  let duplicateTerminalInvocation = false;
  let illegalTerminalInvocation = false;
  let eventWrites = Promise.resolve();
  let readVerifications = Promise.resolve();
  let disconnected = false;
  let outputLimitExceeded = false;
  let transcriptLimitExceeded = false;
  let limitReason: "output_limit" | "turn_limit" | "wall_limit" | "aggregate_cost" | undefined;
  let wallTimer: ReturnType<typeof setTimeout> | undefined;
  let session: Awaited<ReturnType<PiSessionFactory["create"]>> | undefined;
  let unsubscribe: (() => void) | undefined;
  let unsubscribeLoss: (() => void) | undefined;

  const deferredTools = deferTerminalTools(customTools, {
    begin: () => {
      if (terminalInvocation !== undefined || terminalInvocationInFlight) {
        duplicateTerminalInvocation = true;
        return false;
      }
      terminalInvocationInFlight = true;
      return true;
    },
    accept: (operation, payload) => {
      terminalInvocation = { operation, payload: structuredClone(payload) };
      terminalInvocationInFlight = false;
    },
    reject: () => terminalInvocationInFlight = false,
  });

  const onEvent = (event: unknown): void => {
    eventWrites = eventWrites.then(async () => {
      const auditEvent = projectHeadlessAuditEvent(event);
      if (auditEvent === undefined) return;
      const append = await writer.append(auditEvent);
      if (append.truncated && !transcriptLimitExceeded) {
        transcriptLimitExceeded = true;
        outputLimitExceeded = true;
        void session?.abort?.();
      }
    });
    metrics.output_bytes += outputBytes(event);
    const eventCost = eventCostMicroUsd(event);
    if (eventCost !== undefined) {
      metrics.running_cost_micro_usd = Math.max(metrics.running_cost_micro_usd, eventCost);
      if (
        metrics.running_cost_micro_usd > packet.aggregate_cost_remaining_micro_usd &&
        limitReason === undefined
      ) {
        limitReason = "aggregate_cost";
        void session?.abort?.();
      }
    }
    if (metrics.output_bytes > packet.limits.output_byte_limit && !outputLimitExceeded) {
      outputLimitExceeded = true;
      void session?.abort?.();
    }
    observeEvent(event, metrics, (value) => {
      terminal = value;
    });
    const eventRecord = event as Record<string, unknown>;
    const eventToolName = eventRecord !== null && typeof eventRecord === "object"
      ? eventRecord.toolName ?? eventRecord.tool_name
      : undefined;
    if (
      eventRecord !== null && typeof eventRecord === "object" &&
      eventRecord.type === "tool_execution_end" && typeof eventToolName === "string"
    ) {
      const successful = eventRecord.isError === false ||
        (eventRecord.result as Record<string, unknown> | undefined)?.isError === false;
      if (
        successful && TERMINAL_TOOL_NAMES.has(eventToolName) &&
        !packet.legal_terminal_operations.includes(eventToolName as HostToolName)
      ) {
        illegalTerminalInvocation = true;
      }
    }
    if (eventRecord !== null && typeof eventRecord === "object") {
      const turnIndex = number(eventRecord.turnIndex);
      if (turnIndex !== undefined) metrics.turns = Math.max(metrics.turns, turnIndex + 1);
    }
    const turnEnded = eventRecord.type === "turn_end";
    if (
      (turnEnded && metrics.turns >= packet.limits.turn_limit) ||
      (!turnEnded && eventRecord.type === "turn_start" && metrics.turns > packet.limits.turn_limit)
    ) {
      limitReason = "turn_limit";
      void session?.abort?.();
    }
    const record = event as Record<string, unknown>;
    if (
      dependencies.required_read_verifier !== undefined &&
      record !== null && typeof record === "object" &&
      record.type === "tool_execution_end" &&
      (record.toolName === "workspace_read" || record.tool_name === "workspace_read")
    ) {
      readVerifications = readVerifications.then(async () => {
        const verified = await dependencies.required_read_verifier!.verify(record.result);
        if (verified?.success === true) observed.push(verified);
      });
    }
  };

  try {
    const admission = await dependencies.authority?.await_admission();
    if (admission !== undefined) {
      if (
        admission.type !== "session.admitted" || admission.protocol_version !== 1 ||
        admission.assignment_id !== packet.assignment_id ||
        admission.packet_digest !== packet.packet_digest ||
        !Number.isSafeInteger(admission.session_id) || admission.session_id < 1 ||
        !Number.isSafeInteger(admission.session_revision) || admission.session_revision < 0
      ) throw new Error("daemon admission frame does not match the assignment");
    }
    session = await dependencies.session_factory.create(packet, {
      authority_file: dependencies.authority?.file,
      custom_tools: deferredTools,
    });
    unsubscribe = session.subscribe(onEvent);
    wallTimer = setTimeout(() => {
      if (limitReason === undefined) limitReason = "wall_limit";
      void session?.abort?.();
    }, packet.limits.wall_limit_millis);
    unsubscribeLoss = dependencies.authority?.on_loss?.(() => {
      disconnected = true;
      void session?.abort?.();
    });
    if (dependencies.authority && !(await dependencies.authority.is_alive())) {
      disconnected = true;
      throw new Error("daemon authority disconnected before the first prompt");
    }
    await session.prompt(assignmentPrompt);
    if (dependencies.authority && !(await dependencies.authority.is_alive())) {
      disconnected = true;
    }
  } catch (error) {
    if (!disconnected) {
      terminal ??= {
        stop_reason: "host_error",
        failure_reason: error instanceof Error ? error.message : String(error),
      };
    }
  } finally {
    unsubscribeLoss?.();
    unsubscribe?.();
    try {
      session?.dispose();
    } finally {
      if (wallTimer !== undefined) clearTimeout(wallTimer);
      await eventWrites;
      await readVerifications;
      await writer.close();
    }
  }
  if (outputLimitExceeded) limitReason = "output_limit";
  if (limitReason !== undefined) {
    terminal = {
      ...terminal,
      stop_reason: limitReason,
      failure_reason: `assignment ${limitReason.replace("_", " ")} exceeded`,
    };
  }

  const summaryBase = summarize(
    packet,
    metrics,
    terminal,
    paths.ndjson,
    paths.gzip,
    paths.required_read_manifest,
    dependencies.runtime,
  );
  await gzipFile(paths.ndjson, paths.gzip);
  const transcriptArtifact = await dependencies.artifact_sealer.seal(
    paths.gzip,
    "pi_transcript_gzip",
  );
  const manifest = makeRequiredReadManifest(packet.required_reads, observed);
  await writeManifest(paths.required_read_manifest, manifest);
  let summary: NormalizedSessionSummary = {
    ...summaryBase,
    transcript_artifact: transcriptArtifact,
    // The daemon's WorkspaceReadAuthority seals its own ledger. This local
    // JSON is diagnostic evidence only and never becomes terminal authority.
    required_read_manifest_artifact: null,
  };

  let status: PiHostResult["status"] = "succeeded";
  let error: string | null = null;
  let operation: HostToolName | null = null;
  let payload: unknown = null;
  if (disconnected) {
    status = "disconnected";
    error = "daemon authority disconnected";
  } else if (summary.cost_status === "unknown") {
    status = "cost_unknown";
    error = "provider cost was absent or unknown";
  } else if (
    summary.cost_micro_usd !== null &&
    summary.cost_micro_usd > packet.aggregate_cost_remaining_micro_usd
  ) {
    status = "failed";
    error = "assignment cost exceeded aggregate campaign budget";
    summary = { ...summary, stop_reason: "aggregate_cost_exceeded" };
  } else if (manifest.missing.length > 0) {
    status = "required_reads_missing";
    error = "required reads were not proven by exact workspace_read results";
  } else if (
    packet.terminal_submission_required && dependencies.terminal_submission === undefined
  ) {
    status = "terminal_submission_missing";
    error = "terminal submission adapter is missing";
  } else if (
    summary.stop_reason === "host_error" || summary.stop_reason === "terminal_missing" ||
    summary.stop_reason === "output_limit" || summary.stop_reason === "turn_limit" ||
    summary.stop_reason === "wall_limit" || summary.stop_reason === "aggregate_cost"
  ) {
    status = "failed";
    error = summary.failure_reason;
  } else if (packet.terminal_submission_required && terminalInvocation === undefined) {
    status = "failed";
    error = "no successful legal terminal operation was invoked";
  } else if (duplicateTerminalInvocation || illegalTerminalInvocation) {
    status = "failed";
    error = duplicateTerminalInvocation
      ? "duplicate terminal operation invocation"
      : "illegal terminal operation invocation";
  } else if (packet.terminal_submission_required && terminalInvocation !== undefined) {
    operation = terminalInvocation.operation;
    payload = terminalInvocation.payload;
  }

  // The daemon receives one normalized terminal summary for every outcome
  // whose authority is still alive. The local manifest remains diagnostic;
  // the daemon's read ledger is the only source of terminal evidence.
  if (!disconnected && dependencies.terminal_submission !== undefined) {
    await dependencies.terminal_submission.submit(
      operation,
      payload,
      manifest,
      { ...summary, stop_reason: normalizeTerminalStopReason(summary, status) },
    );
  }
  return { status, summary, required_read_manifest: manifest, error };
}

function normalizeTerminalStopReason(
  summary: NormalizedSessionSummary,
  status: PiHostResult["status"],
): string {
  if (status === "succeeded") return "completed";
  if (status === "disconnected") return "daemon_disconnected";
  if (summary.cost_status === "unknown" || status === "cost_unknown") return "unknown_cost";
  if (
    summary.stop_reason === "output_limit" || summary.stop_reason === "aggregate_cost" ||
    summary.stop_reason === "aggregate_cost_exceeded"
  ) return "output_limit";
  if (summary.stop_reason === "turn_limit" || summary.stop_reason === "wall_limit") {
    return "deadline";
  }
  if (summary.stop_reason === "host_error" || summary.stop_reason === "terminal_missing") {
    return "protocol_error";
  }
  return "protocol_error";
}

export function validateToolAllowlist(
  packet: PiAssignmentPacket,
  adapters: readonly PiToolAdapter[],
): readonly PiToolAdapter[] {
  const seen = new Set<string>();
  const byName = new Map<string, PiToolAdapter>();
  for (const adapter of adapters) {
    if (byName.has(adapter.name)) throw new Error(`adapter ${adapter.name} is repeated`);
    if (!KNOWN_TOOL_NAMES.has(adapter.name)) {
      throw new Error(`adapter ${adapter.name} is unknown to the host`);
    }
    byName.set(adapter.name, adapter);
  }
  const resolved: PiToolAdapter[] = [];
  for (const name of packet.tools) {
    if (!KNOWN_TOOL_NAMES.has(name)) throw new Error(`assignment contains unknown tool ${name}`);
    if (!seen.add(name)) throw new Error(`assignment repeats tool ${name}`);
    if (!(name in BUILTIN_TOOL_NAMES)) {
      const adapter = byName.get(name);
      // Older immutable Engineering application revisions still advertise
      // report sealing. Completion evidence is now kernel-owned, so omit this
      // strictly less-powerful legacy tool rather than making a changed
      // worktree depend on an optional workspace prose file.
      if (adapter === undefined && name === "artifact_seal" && packet.assignment_role === "engineering") {
        continue;
      }
      if (adapter === undefined) throw new Error(`assignment tool ${name} has no bound adapter`);
      resolved.push(adapter);
    }
  }
  for (const adapter of adapters) {
    if (!seen.has(adapter.name)) {
      throw new Error(`adapter ${adapter.name} is not admitted by assignment`);
    }
  }
  return resolved;
}

export function builtinPiToolNames(packet: PiAssignmentPacket): readonly string[] {
  return packet.tools.filter((name) => name in BUILTIN_TOOL_NAMES).map((name) =>
    BUILTIN_TOOL_NAMES[name]
  );
}

function deferTerminalTools(
  adapters: readonly PiToolAdapter[],
  terminal: {
    readonly begin: () => boolean;
    readonly accept: (operation: HostToolName, payload: unknown) => void;
    readonly reject: () => void;
  },
): readonly PiToolAdapter[] {
  return adapters.map((adapter) => {
    if (!TERMINAL_TOOL_NAMES.has(adapter.name)) return adapter;
    return {
      ...adapter,
      sdk_definition: {
        ...adapter.sdk_definition,
        async invoke(input: unknown): Promise<unknown> {
          if (!terminal.begin()) {
            // Do not invoke a second daemon terminal transition. The host
            // records the duplicate and fails terminal admission afterward,
            // while letting Pi complete normally enough to seal diagnostics.
            return { accepted: false, duplicate: true };
          }
          // `work_complete` is host-capture-only: its normalized terminal
          // report is the one daemon operation. Candidate/Quality terminal
          // tools, by contrast, first perform their own kernel transition;
          // only a successful receipt may be captured for session terminal.
          if (adapter.name === "work_complete") {
            terminal.accept(adapter.name, input);
            return { accepted: true, deferred: true };
          }
          try {
            const result = await adapter.sdk_definition.invoke(input);
            terminal.accept(adapter.name, input);
            return result;
          } catch (error) {
            terminal.reject();
            throw error;
          }
        },
      },
    };
  });
}

function validatePacket(packet: PiAssignmentPacket): void {
  if (packet.format_version !== 1) throw new Error("assignment packet format is unsupported");
  if (
    packet.assignment_id.length === 0 || packet.campaign_id.length === 0 ||
    packet.application_revision_id.length === 0 || packet.kernel_build_id.length === 0 ||
    packet.repository_base_identity.length === 0 ||
    packet.factory_base_identity.length === 0
  ) {
    throw new Error("assignment identity is required");
  }
  for (
    const [field, value] of [
      ["packet digest", packet.packet_digest],
      ["system prompt digest", packet.system_prompt_digest],
      ["assignment prompt digest", packet.assignment_prompt_digest],
    ] as const
  ) {
    if (!/^[a-f0-9]{64}$/.test(value)) throw new Error(`${field} is invalid`);
  }
  if (
    !Number.isSafeInteger(packet.aggregate_cost_remaining_micro_usd) ||
    packet.aggregate_cost_remaining_micro_usd < 1
  ) throw new Error("aggregate cost remaining is invalid");
  if (packet.legal_terminal_operations.length === 0) {
    throw new Error("at least one legal terminal operation is required");
  }
  const legal = new Set<string>();
  for (const operation of packet.legal_terminal_operations) {
    if (!TERMINAL_TOOL_NAMES.has(operation) || !legal.add(operation)) {
      throw new Error("legal terminal operation is invalid");
    }
    if (!packet.tools.includes(operation)) {
      throw new Error("legal terminal operation is not in the tool allowlist");
    }
  }
  if (
    packet.system_prompt_bytes.byteLength === 0 || packet.assignment_prompt_bytes.byteLength === 0
  ) throw new Error("sealed prompts must not be empty");
  if (packet.tools.includes("artifact_read") && packet.assignment_evidence.length === 0) {
    throw new Error("artifact_read requires sealed upstream assignment evidence");
  }
  if (!Number.isSafeInteger(packet.limits.turn_limit) || packet.limits.turn_limit < 1) {
    throw new Error("turn limit is invalid");
  }
  if (
    !Number.isSafeInteger(packet.limits.wall_limit_millis) || packet.limits.wall_limit_millis < 1
  ) throw new Error("wall limit is invalid");
  if (
    !Number.isSafeInteger(packet.limits.output_byte_limit) || packet.limits.output_byte_limit < 1
  ) throw new Error("output limit is invalid");
  if (
    packet.runtime.credential_source.kind === "environment" &&
    !/^[A-Z_][A-Z0-9_]*$/.test(packet.runtime.credential_source.name)
  ) throw new Error("credential environment name is invalid");
  if (
    packet.runtime.credential_source.kind === "pi_auth_store" &&
    packet.runtime.credential_source.path.length === 0
  ) throw new Error("credential path is required");
  const exactTarget = packet.assignment_role === "product_research"
    ? packet.ticket_attempt_id === null && packet.candidate_id === null
    : packet.assignment_role === "engineering"
    ? packet.ticket_attempt_id !== null && packet.candidate_id === null
    : packet.assignment_role === "quality"
    ? packet.ticket_attempt_id !== null && packet.candidate_id !== null
    : false;
  if (!exactTarget) throw new Error("assignment durable target does not match its office");
}

function observeEvent(
  event: unknown,
  metrics: MutableMetrics,
  setTerminal: (terminal: TerminalUsage) => void,
): void {
  if (event === null || typeof event !== "object") return;
  const record = event as Record<string, unknown>;
  const type = typeof record.type === "string" ? record.type : "";
  if (type === "tool_call" || type === "tool_execution_start" || type === "tool_start") {
    metrics.tool_calls += 1;
  }
  if (type === "tool_result" || type === "tool_execution_end" || type === "tool_end") {
    const result = record.result as Record<string, unknown> | undefined;
    const exit = record.exit_code ?? record.exitCode ?? result?.exit_code ?? result?.exitCode;
    if (typeof exit === "number" && exit !== 0) metrics.nonzero_tool_results += 1;
  }
  if (type === "auto_retry_start" || type === "retry") metrics.retries += 1;
  if (type === "turn_end" || type === "turn_start") {
    metrics.turns = Math.max(
      metrics.turns,
      number(record.turns ?? record.turn) ?? metrics.turns + (type === "turn_end" ? 1 : 0),
    );
  }
  addUsage(metrics, (record.usage ?? record.tokens) as Record<string, unknown> | undefined);
  if (type === "terminal") setTerminal(record as TerminalUsage);
  if (type === "agent_end") {
    const messages = Array.isArray(record.messages) ? record.messages : [];
    const assistant = [...messages].reverse().find((message) =>
      message !== null && typeof message === "object" &&
      (message as Record<string, unknown>).role === "assistant"
    ) as Record<string, unknown> | undefined;
    const usage = assistant?.usage as Record<string, unknown> | undefined;
    const cost = usage?.cost as Record<string, unknown> | undefined;
    setTerminal({
      ...record,
      stop_reason: assistant?.stopReason ?? record.stop_reason ?? record.stopReason,
      failure_reason: assistant?.errorMessage ?? record.failure_reason ?? record.failureReason,
      cost_usd: cost?.total,
      usage,
      messages,
    });
  }
}

function addUsage(metrics: MutableMetrics, usage: Record<string, unknown> | undefined): void {
  if (usage === undefined) return;
  metrics.tokens.input = number(usage.input ?? usage.input_tokens ?? usage.inputTokens) ??
    metrics.tokens.input;
  metrics.tokens.output = number(usage.output ?? usage.output_tokens ?? usage.outputTokens) ??
    metrics.tokens.output;
  metrics.tokens.cache_read =
    number(usage.cache_read ?? usage.cacheRead ?? usage.cache_read_tokens) ??
      metrics.tokens.cache_read;
  metrics.tokens.cache_write =
    number(usage.cache_write ?? usage.cacheWrite ?? usage.cache_write_tokens) ??
      metrics.tokens.cache_write;
  const reasoning = number(usage.reasoning ?? usage.reasoning_tokens ?? usage.reasoningTokens);
  if (reasoning !== undefined) metrics.tokens.reasoning = reasoning;
}

/** Counts streamed model deltas once, in UTF-8 bytes, before they reach tools. */
function outputBytes(event: unknown): number {
  if (event === null || typeof event !== "object") return 0;
  const record = event as Record<string, unknown>;
  if (record.type !== "message_update") return 0;
  const update =
    (record.assistantMessageEvent ?? record.assistant_message_event ?? record) as Record<
      string,
      unknown
    >;
  return typeof update.delta === "string" ? new TextEncoder().encode(update.delta).byteLength : 0;
}

function eventCostMicroUsd(event: unknown): number | undefined {
  if (event === null || typeof event !== "object") return undefined;
  const record = event as Record<string, unknown>;
  const usage = record.usage as Record<string, unknown> | undefined;
  const usageCost = usage?.cost as Record<string, unknown> | undefined;
  const micro = number(
    record.aggregate_cost_micro_usd ?? record.cost_micro_usd ?? record.costMicroUsd,
  );
  if (micro !== undefined && micro >= 0) return Math.ceil(micro);
  const dollars = number(record.aggregate_cost_usd ?? record.cost_usd ?? record.costUsd);
  if (dollars !== undefined && dollars >= 0) return Math.ceil(dollars * 1_000_000);
  const usageDollars = number(usageCost?.total);
  if (usageDollars !== undefined && usageDollars >= 0) {
    return Math.ceil(usageDollars * 1_000_000);
  }
  return undefined;
}

function summarize(
  packet: PiAssignmentPacket,
  metrics: MutableMetrics,
  terminal: TerminalUsage | undefined,
  transcriptPath: string,
  transcriptGzipPath: string,
  manifestPath: string,
  runtime: PiHostDependencies["runtime"],
): NormalizedSessionSummary {
  const cost = terminalCost(terminal);
  const messageUsage = terminal?.messages === undefined
    ? undefined
    : aggregateAssistantMessages(terminal.messages);
  if (messageUsage !== undefined) {
    metrics.tokens = messageUsage.tokens;
    metrics.turns = Math.max(metrics.turns, messageUsage.turns);
  } else {
    const terminalTokens = terminal?.tokens ?? terminal?.usage;
    if (terminalTokens !== undefined) addUsage(metrics, terminalTokens);
  }
  const stopReason = string(terminal?.stop_reason ?? terminal?.stopReason) ?? "terminal_missing";
  return {
    assignment_id: packet.assignment_id,
    assignment_role: packet.assignment_role,
    stop_reason: stopReason,
    failure_reason: string(terminal?.failure_reason ?? terminal?.failureReason) ?? null,
    cost_status: cost === undefined ? "unknown" : "known",
    cost_micro_usd: cost ?? null,
    turns: number(terminal?.turns) ?? metrics.turns,
    tokens: { ...metrics.tokens },
    output_bytes: metrics.output_bytes,
    tool_calls: metrics.tool_calls,
    nonzero_tool_results: metrics.nonzero_tool_results,
    retries: metrics.retries,
    model: packet.model,
    runtime: {
      deno_version: runtime?.deno_version ?? Deno.version.deno,
      pi_sdk_version: runtime?.pi_sdk_version ?? "0.84.1",
    },
    transcript_path: transcriptPath,
    transcript_gzip_path: transcriptGzipPath,
    transcript_artifact: null,
    required_read_manifest_path: manifestPath,
    required_read_manifest_artifact: null,
  };
}

function terminalCost(terminal: TerminalUsage | undefined): number | undefined {
  const messages = terminal?.messages;
  if (messages !== undefined) {
    return aggregateAssistantMessages(messages)?.cost_micro_usd;
  }
  const micro = number(terminal?.cost_micro_usd ?? terminal?.costMicroUsd);
  if (micro !== undefined && micro >= 0) return Math.ceil(micro);
  const dollars = number(terminal?.cost_usd ?? terminal?.costUsd);
  if (dollars !== undefined && dollars >= 0) return Math.ceil(dollars * 1_000_000);
  return undefined;
}

interface AssistantMessageAggregate {
  readonly turns: number;
  readonly tokens: MutableMetrics["tokens"];
  readonly cost_micro_usd: number | undefined;
}

/** The SDK reports one usage object per assistant message; retain exact totals. */
function aggregateAssistantMessages(
  messages: readonly unknown[],
): AssistantMessageAggregate | undefined {
  const assistants = messages.filter((message) =>
    message !== null && typeof message === "object" &&
    (message as Record<string, unknown>).role === "assistant"
  ) as readonly Record<string, unknown>[];
  if (assistants.length === 0) return undefined;

  const sums = {
    input: 0,
    output: 0,
    cache_read: 0,
    cache_write: 0,
    reasoning: 0,
  };
  const seen = {
    input: false,
    output: false,
    cache_read: false,
    cache_write: false,
    reasoning: false,
  };
  let cost = 0;
  let costKnown = true;
  for (const assistant of assistants) {
    const usage = assistant.usage as Record<string, unknown> | undefined;
    if (usage === undefined) {
      costKnown = false;
      continue;
    }
    const values = {
      input: number(usage.input ?? usage.input_tokens ?? usage.inputTokens),
      output: number(usage.output ?? usage.output_tokens ?? usage.outputTokens),
      cache_read: number(usage.cache_read ?? usage.cacheRead ?? usage.cache_read_tokens),
      cache_write: number(usage.cache_write ?? usage.cacheWrite ?? usage.cache_write_tokens),
      reasoning: number(usage.reasoning ?? usage.reasoning_tokens ?? usage.reasoningTokens),
    };
    for (const key of Object.keys(values) as (keyof typeof values)[]) {
      const value = values[key];
      if (value !== undefined) {
        sums[key] += value;
        seen[key] = true;
      }
    }
    const usageCost = usage.cost as Record<string, unknown> | undefined;
    const amount = number(usageCost?.total);
    if (amount === undefined || amount < 0) costKnown = false;
    else cost += amount;
  }
  return {
    turns: assistants.length,
    tokens: {
      input: seen.input ? sums.input : 0,
      output: seen.output ? sums.output : 0,
      cache_read: seen.cache_read ? sums.cache_read : 0,
      cache_write: seen.cache_write ? sums.cache_write : 0,
      reasoning: seen.reasoning ? sums.reasoning : null,
    },
    cost_micro_usd: costKnown ? Math.ceil(cost * 1_000_000) : undefined,
  };
}

function makeRequiredReadManifest(
  required: PiAssignmentPacket["required_reads"],
  observed: readonly RequiredReadObservation[],
): RequiredReadManifest {
  const satisfied = required.filter((expected) =>
    observed.some((actual) =>
      actual.success && actual.canonical_path === expected.canonical_path &&
      actual.blake3 === expected.blake3
    )
  );
  return {
    required,
    satisfied,
    observed,
    missing: required.filter((expected) => !satisfied.includes(expected)),
  };
}

function string(value: unknown): string | undefined {
  return typeof value === "string" ? value : undefined;
}

function number(value: unknown): number | undefined {
  return typeof value === "number" && Number.isFinite(value) ? value : undefined;
}
