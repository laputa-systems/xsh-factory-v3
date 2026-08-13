/**
 * Process-custody entrypoint for one fresh Pi assignment.
 *
 * The daemon supplies the concrete verifier, CAS sealer, required-read
 * attestation, and terminal authority through this narrow binding seam. The
 * host itself owns stdin/FD 0, packet admission, SDK construction, limits,
 * transcript sealing, and the no-resume lifecycle. No socket path, database
 * URL, or provider call is discovered here.
 */
import { createSdkPiSessionFactory } from "./sdk-factory.ts";
import {
  inheritedAuthority,
  type PiHostEntrypointDependencies,
  runPiHostEntrypoint,
} from "./entrypoint.ts";
import {
  createFramedToolAdapters,
  DaemonRequiredReadVerifier,
  FramedActorClient,
  InheritedFrameTransport,
} from "./framed-actor.ts";
import { canonicalJson, RESPONSE_FRAME_MAX_BYTES } from "../factory-sdk/protocol.ts";
import type {
  ArtifactSealer,
  AuthorityAdmissionFrame,
  NormalizedSessionSummary,
  PiAssignmentPacket,
  PiHostResult,
  PiSessionFactory,
  PiToolAdapter,
  RequiredReadManifest,
  RequiredReadVerifier,
  TerminalSubmission,
} from "./types.ts";
import type { PiHostDependencies } from "./host.ts";

export interface PiHostMainBindings extends
  Omit<
    PiHostDependencies,
    | "authority"
    | "session_factory"
    | "custom_tools"
    | "artifact_sealer"
    | "required_read_verifier"
    | "terminal_submission"
    | "packet_integrity_verifier"
  > {
  readonly authority?: PiHostDependencies["authority"];
  readonly actor_client?: FramedActorClient;
  readonly session_factory?: PiSessionFactory;
  readonly packet_integrity_verifier?: PiHostDependencies["packet_integrity_verifier"];
  readonly artifact_sealer?: ArtifactSealer;
  readonly artifact_sealer_factory?: (
    packet: PiAssignmentPacket,
    admission: AuthorityAdmissionFrame,
  ) => ArtifactSealer | Promise<ArtifactSealer>;
  readonly required_read_verifier?: RequiredReadVerifier;
  readonly required_read_verifier_factory?:
    PiHostEntrypointDependencies["required_read_verifier_factory"];
  readonly terminal_submission?: TerminalSubmission;
  readonly terminal_submission_factory?:
    PiHostEntrypointDependencies["terminal_submission_factory"];
  readonly custom_tools?: PiHostDependencies["custom_tools"];
}

/** Runs exactly one host over the inherited daemon descriptor (stdin/FD 0). */
export async function runPiHostMain(bindings: PiHostMainBindings = {}): Promise<PiHostResult> {
  // Deno.stdin deliberately exposes a read-only Stdin surface even when FD 0
  // is a full-duplex Unix socket. Reopening the already-inherited descriptor
  // produces the Deno.FsFile required for framed replies; it neither discovers
  // nor connects to a socket path. Actor sessions run under the plan's
  // deliberate `-A` cooperative same-user trust model.
  // Deno serializes operations per FsFile resource. Duplicate the inherited
  // full-duplex socket so the long-lived response reader cannot prevent a
  // later request write on the same resource lock.
  const inheritedReadFile = await Deno.open("/dev/fd/0", { read: true });
  const inheritedWriteFile = await Deno.open("/dev/fd/0", { write: true });
  const inheritedTransport = bindings.actor_client === undefined
    ? new InheritedFrameTransport(inheritedReadFile, inheritedWriteFile)
    : undefined;
  const client = bindings.actor_client ?? new FramedActorClient(inheritedTransport!);
  let nextCommandId = 0;
  const packetVerifier = bindings.packet_integrity_verifier ?? (async (
    _packet: PiAssignmentPacket,
    canonicalBytes?: Uint8Array,
    expectedDigest?: string,
  ): Promise<boolean> => {
    if (canonicalBytes === undefined || expectedDigest === undefined) return false;
    const response = await client.call<{
      readonly packet_digest?: unknown;
      readonly verified?: unknown;
    }>("session.verify_packet", {
      packet_digest: expectedDigest,
      packet_bytes_b64: base64Encode(canonicalBytes),
    });
    return response.verified === true && response.packet_digest === expectedDigest;
  });
  try {
    return await runPiHostEntrypoint({
      ...bindings,
      packet_integrity_verifier: packetVerifier,
      required_read_verifier: bindings.required_read_verifier ?? new DaemonRequiredReadVerifier(),
      authority: bindings.authority ?? inheritedAuthority(inheritedReadFile, inheritedTransport),
      session_factory: bindings.session_factory ?? createSdkPiSessionFactory(),
      artifact_sealer_factory: bindings.artifact_sealer_factory ??
        ((packet, admission) =>
          bindings.artifact_sealer ??
            createFramedArtifactSealer(client, packet, admission.session_revision, () =>
              ++nextCommandId)),
      terminal_submission_factory: bindings.terminal_submission_factory ??
        ((_packet, admission) =>
          bindings.terminal_submission ??
            createFramedTerminalSubmission(
              client,
              admission.session_revision,
              () => ++nextCommandId,
            )),
      custom_tool_factory: bindings.custom_tools === undefined
        ? (packet, admission) =>
          createInheritedCommonTools(
            client,
            packet.tools,
            packet.office,
            admission.session_revision,
            () => ++nextCommandId,
          )
        : undefined,
    });
  } finally {
    inheritedReadFile.close();
    inheritedWriteFile.close();
  }
}

/** Creates the generic framed operation seam used by process-custody wiring. */
export async function createInheritedActorClient(): Promise<FramedActorClient> {
  const inheritedReadFile = await Deno.open("/dev/fd/0", { read: true });
  const inheritedWriteFile = await Deno.open("/dev/fd/0", { write: true });
  return new FramedActorClient(
    new InheritedFrameTransport(inheritedReadFile, inheritedWriteFile),
  );
}

/**
 * Binds only the common daemon operation wrappers. Forum adapters and the
 * typed CAS/read/terminal gates remain explicit bindings because their
 * operation-specific receipt/evidence contracts belong to the daemon.
 */
export function createInheritedCommonTools(
  client: FramedActorClient,
  names: Parameters<typeof createFramedToolAdapters>[1],
  office: string,
  sessionRevision = 0,
  nextCommandId: () => number = (() => {
    let next = 0;
    return () => ++next;
  })(),
): readonly PiToolAdapter[] {
  const effectiveNames = office === "engineering"
    ? names.filter((name) => name !== "artifact_seal")
    : names;
  return createFramedToolAdapters(client, effectiveNames, {
    session_revision: safeNumber(sessionRevision, "session_revision", 0),
    next_command_id: nextCommandId,
  });
}

function createFramedArtifactSealer(
  client: FramedActorClient,
  packet: PiAssignmentPacket,
  sessionRevision: number,
  nextCommandId: () => number,
): ArtifactSealer {
  return {
    async seal(path, role) {
      const staging_relative_path = relativeStagingPath(packet.staging_root, path);
      return await client.call("session.seal_artifact", {
        client_command_id: `host-seal-${nextCommandId()}`,
        expected_revision: safeNumber(sessionRevision, "session_revision", 0),
        staging_relative_path,
        role,
        byte_limit: RESPONSE_FRAME_MAX_BYTES,
      });
    },
  };
}

function createFramedTerminalSubmission(
  client: FramedActorClient,
  sessionRevision: number,
  nextCommandId: () => number,
): TerminalSubmission {
  return {
    async submit(operation, payload, _manifest, summary) {
      const transcript = summary.transcript_artifact;
      if (transcript === null) {
        throw new Error("terminal submission requires sealed transcript");
      }
      await client.call("session.submit_terminal", {
        client_command_id: `host-terminal-${nextCommandId()}`,
        expected_revision: safeNumber(sessionRevision, "session_revision", 0),
        terminal_operation: operation,
        terminal_payload_b64: base64Encode(new TextEncoder().encode(canonicalJson(payload))),
        transcript_artifact_id: safeNumber(transcript.artifact_id, "transcript_artifact_id"),
        input_tokens: summary.tokens.input,
        output_tokens: summary.tokens.output,
        cache_read_tokens: summary.tokens.cache_read,
        cache_write_tokens: summary.tokens.cache_write,
        reasoning_tokens: summary.tokens.reasoning,
        reported_cost_micro_usd: summary.cost_micro_usd,
        stop_reason: normalizeClosedStopReason(summary.stop_reason),
      });
    },
  };
}

function normalizeClosedStopReason(value: string): string {
  if (value === "completed") return "completed";
  if (value === "daemon_disconnected") return "daemon_disconnected";
  if (value === "unknown_cost") return "unknown_cost";
  if (
    value === "output_limit" || value === "aggregate_cost" ||
    value === "aggregate_cost_exceeded"
  ) return "output_limit";
  if (value === "turn_limit" || value === "wall_limit") return "deadline";
  if (value === "cancelled") return "cancelled";
  if (value === "nonzero_exit") return "nonzero_exit";
  return "protocol_error";
}

function relativeStagingPath(root: string, path: string): string {
  const prefix = root.endsWith("/") ? root : `${root}/`;
  if (!path.startsWith(prefix)) throw new Error("sealed artifact is outside packet staging root");
  const relative = path.slice(prefix.length);
  if (relative.length === 0 || relative.includes("\0") || relative.startsWith("/")) {
    throw new Error("sealed artifact path is invalid");
  }
  return relative;
}

function safeNumber(value: number | string, name: string, minimum = 1): number {
  const parsed = typeof value === "number" ? value : Number(value);
  if (!Number.isSafeInteger(parsed) || parsed < minimum) {
    throw new Error(`${name} is not a safe integer`);
  }
  return parsed;
}

function base64Encode(bytes: Uint8Array): string {
  let binary = "";
  for (const byte of bytes) binary += String.fromCharCode(byte);
  return btoa(binary);
}

if (import.meta.main) {
  try {
    await runPiHostMain();
  } catch (error) {
    console.error(
      `factory-pi-host halted: ${error instanceof Error ? error.message : String(error)}`,
    );
    Deno.exit(78);
  }
}

// Keep these imports visible in the public process seam for downstream wiring
// without introducing a second set of adapter types.
export type { ArtifactSealer, NormalizedSessionSummary, RequiredReadManifest };
