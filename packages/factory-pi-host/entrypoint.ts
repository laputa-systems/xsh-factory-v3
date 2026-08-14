import { MAX_ASSIGNMENT_PACKET_BYTES, readSessionAdmissionFrame } from "./transcript.ts";
import { canonicalJson } from "../factory-sdk/protocol.ts";
import { type PiHostDependencies, runAssignment } from "./host.ts";
import { ASSIGNMENT_EVIDENCE_ROLES_V1 } from "./types.ts";
import type {
  ArtifactSealer,
  AuthorityAdmissionFrame,
  AuthorityLiveness,
  HostToolName,
  ModelCapabilityV1,
  PiAssignmentPacket,
  PiHostResult,
  PiToolAdapter,
  RequiredReadVerifier,
  TerminalSubmission,
} from "./types.ts";
import type { InheritedFrameTransport } from "./framed-actor.ts";

/**
 * Decodes the closed JSON packet carried by the daemon's startup attestation.
 * No packet file, environment value, or database connection is consulted.
 */
export function decodeAssignmentPacketV1(bytes: Uint8Array): PiAssignmentPacket {
  const source = new TextDecoder("utf-8", { fatal: true }).decode(bytes);
  let value: unknown;
  try {
    value = JSON.parse(source);
  } catch (error) {
    throw new Error(
      `assignment packet JSON is invalid: ${error instanceof Error ? error.message : error}`,
    );
  }
  if (canonicalJson(value) !== source) {
    throw new Error("assignment packet bytes are not canonical V1 JSON");
  }
  const root = record(value, "assignment packet", [
    "format_version",
    "assignment_id",
    "campaign_id",
    "application_revision_id",
    "kernel_build_id",
    "repository_base_identity",
    "factory_base_identity",
    "assignment_role",
    "target",
    "ticket_attempt_id",
    "candidate_id",
    "packet_digest",
    "system_prompt_artifact_id",
    "assignment_prompt_artifact_id",
    "required_read_manifest_artifact_id",
    "system_prompt_digest",
    "assignment_prompt_digest",
    "system_prompt_bytes_b64",
    "assignment_prompt_bytes_b64",
    "workspace_root",
    "staging_root",
    "model",
    "limits",
    "runtime",
    "tools",
    "required_reads",
    "assignment_evidence",
    "terminal_operations",
    "remaining_campaign_allowance_micro_usd",
    "aggregate_revision",
  ]);
  if (root.format_version !== 1) throw new Error("assignment packet format_version is unsupported");
  const model = record(root.model, "packet.model", [
    "provider",
    "model_id",
    "thinking_level",
    "context_token_limit",
    "output_token_limit",
    "price_input_micro_usd_per_million_tokens",
    "price_output_micro_usd_per_million_tokens",
    "price_cache_read_micro_usd_per_million_tokens",
    "price_cache_write_micro_usd_per_million_tokens",
    "capability_flags",
  ]);
  const limits = record(root.limits, "packet.limits", [
    "turn_limit",
    "wall_limit_millis",
    "output_byte_limit",
  ]);
  const runtime = record(root.runtime, "packet.runtime", [
    "deno_executable",
    "deno_version",
    "source_graph_digest",
    "resolved_dependency_graph_digest",
    "deno_json_digest",
    "deno_lock_digest",
    "pi_version",
    "credential_source",
  ]);
  const credential = record(runtime.credential_source, "packet.runtime.credential_source", [
    "kind",
    "name",
    "path",
  ]);
  if (credential.kind === "environment") {
    if (typeof credential.name !== "string" || credential.path !== null) {
      throw new Error("environment credential name is missing");
    }
  } else if (credential.kind === "pi_auth_store") {
    if (typeof credential.path !== "string" || credential.name !== null) {
      throw new Error("Pi auth-store path is missing");
    }
  } else throw new Error("credential source kind is invalid");

  const packet: PiAssignmentPacket = {
    format_version: 1,
    assignment_id: numericIdentity(root.assignment_id, "assignment_id"),
    assignment_role: string(root.assignment_role, "assignment_role"),
    campaign_id: numericIdentity(root.campaign_id, "campaign_id"),
    application_revision_id: numericIdentity(
      root.application_revision_id,
      "application_revision_id",
    ),
    kernel_build_id: string(root.kernel_build_id, "kernel_build_id"),
    repository_base_identity: string(
      root.repository_base_identity,
      "repository_base_identity",
    ),
    factory_base_identity: string(root.factory_base_identity, "factory_base_identity"),
    ticket_attempt_id: nullableNumericIdentity(root.ticket_attempt_id, "ticket_attempt_id"),
    candidate_id: nullableNumericIdentity(root.candidate_id, "candidate_id"),
    packet_digest: digest(root.packet_digest, "packet_digest"),
    system_prompt_artifact_id: identifier(
      root.system_prompt_artifact_id,
      "system_prompt_artifact_id",
    ),
    assignment_prompt_artifact_id: identifier(
      root.assignment_prompt_artifact_id,
      "assignment_prompt_artifact_id",
    ),
    required_read_manifest_artifact_id: identifier(
      root.required_read_manifest_artifact_id,
      "required_read_manifest_artifact_id",
    ),
    system_prompt_digest: string(root.system_prompt_digest, "system_prompt_digest"),
    assignment_prompt_digest: string(root.assignment_prompt_digest, "assignment_prompt_digest"),
    aggregate_cost_remaining_micro_usd: integer(
      root.remaining_campaign_allowance_micro_usd,
      "remaining_campaign_allowance_micro_usd",
    ),
    aggregate_revision: aggregateRevision(root.aggregate_revision),
    legal_terminal_operations: strings(
      root.terminal_operations,
      "terminal_operations",
    ) as HostToolName[],
    target: string(root.target, "target"),
    workspace_root: string(root.workspace_root, "workspace_root"),
    staging_root: string(root.staging_root, "staging_root"),
    system_prompt_bytes: base64(root.system_prompt_bytes_b64, "system_prompt_bytes_b64"),
    assignment_prompt_bytes: base64(
      root.assignment_prompt_bytes_b64,
      "assignment_prompt_bytes_b64",
    ),
    model: {
      provider: string(model.provider, "model.provider"),
      model_id: string(model.model_id, "model.model_id"),
      thinking_level: oneOf(
        model.thinking_level,
        ["none", "low", "medium", "high", "xhigh"],
        "model.thinking_level",
      ),
      context_token_limit: integer(model.context_token_limit, "model.context_token_limit"),
      output_token_limit: integer(model.output_token_limit, "model.output_token_limit"),
      price_input_micro_usd_per_million_tokens: integer(
        model.price_input_micro_usd_per_million_tokens,
        "model.input price",
      ),
      price_output_micro_usd_per_million_tokens: integer(
        model.price_output_micro_usd_per_million_tokens,
        "model.output price",
      ),
      price_cache_read_micro_usd_per_million_tokens: integer(
        model.price_cache_read_micro_usd_per_million_tokens,
        "model.cache read price",
      ),
      price_cache_write_micro_usd_per_million_tokens: integer(
        model.price_cache_write_micro_usd_per_million_tokens,
        "model.cache write price",
      ),
      capability_flags: modelCapabilities(model.capability_flags),
    },
    limits: {
      turn_limit: integer(limits.turn_limit, "limits.turn_limit"),
      wall_limit_millis: integer(limits.wall_limit_millis, "limits.wall_limit_millis"),
      output_byte_limit: integer(limits.output_byte_limit, "limits.output_byte_limit"),
    },
    tools: strings(root.tools, "tools") as PiAssignmentPacket["tools"],
    required_reads: requiredReads(root.required_reads),
    assignment_evidence: assignmentEvidence(
      root.assignment_evidence,
      string(root.assignment_role, "assignment_role"),
    ),
    runtime: {
      deno_executable: string(runtime.deno_executable, "runtime.deno_executable"),
      deno_version: string(runtime.deno_version, "runtime.deno_version"),
      source_graph_digest: digest(runtime.source_graph_digest, "runtime.source_graph_digest"),
      resolved_dependency_graph_digest: digest(
        runtime.resolved_dependency_graph_digest,
        "runtime.resolved_dependency_graph_digest",
      ),
      deno_json_digest: digest(runtime.deno_json_digest, "runtime.deno_json_digest"),
      deno_lock_digest: digest(runtime.deno_lock_digest, "runtime.deno_lock_digest"),
      pi_version: string(runtime.pi_version, "runtime.pi_version"),
      credential_source: credential.kind === "environment"
        ? { kind: "environment", name: credential.name as string }
        : { kind: "pi_auth_store", path: credential.path as string },
    },
    terminal_submission_required: true,
  };
  return packet;
}

function modelCapabilities(value: unknown): ModelCapabilityV1[] {
  const flags = strings(value, "model.capability_flags");
  if (flags.some((flag) => flag !== "reasoning") || new Set(flags).size !== flags.length) {
    throw new Error("model.capability_flags contains an unknown or duplicate capability");
  }
  return flags as ModelCapabilityV1[];
}

export interface PiHostEntrypointDependencies extends
  Omit<
    PiHostDependencies,
    | "authority"
    | "custom_tools"
    | "artifact_sealer"
    | "required_read_verifier"
    | "terminal_submission"
  > {
  readonly authority: AuthorityLiveness;
  readonly artifact_sealer?: ArtifactSealer;
  readonly artifact_sealer_factory?: (
    packet: PiAssignmentPacket,
    admission: AuthorityAdmissionFrame,
  ) => ArtifactSealer | Promise<ArtifactSealer>;
  readonly required_read_verifier?: RequiredReadVerifier;
  readonly required_read_verifier_factory?: (
    packet: PiAssignmentPacket,
    admission: AuthorityAdmissionFrame,
  ) => RequiredReadVerifier | Promise<RequiredReadVerifier>;
  readonly terminal_submission?: TerminalSubmission;
  readonly terminal_submission_factory?: (
    packet: PiAssignmentPacket,
    admission: AuthorityAdmissionFrame,
  ) => TerminalSubmission | Promise<TerminalSubmission>;
  readonly custom_tools?: readonly PiToolAdapter[];
  readonly custom_tool_factory?: (
    packet: PiAssignmentPacket,
    admission: AuthorityAdmissionFrame,
  ) => readonly PiToolAdapter[] | Promise<readonly PiToolAdapter[]>;
}

/** Consumes one daemon startup attestation, then constructs exactly one Pi session. */
export async function runPiHostEntrypoint(
  dependencies: PiHostEntrypointDependencies,
): Promise<PiHostResult> {
  const frame = await dependencies.authority.await_admission();
  const packetBytes = base64(frame.packet_b64, "packet_b64", MAX_ASSIGNMENT_PACKET_BYTES);
  const packet = decodeAssignmentPacketV1(packetBytes);
  if (
    packet.assignment_id !== frame.assignment_id || packet.packet_digest !== frame.packet_digest
  ) {
    throw new Error("startup packet does not match the daemon admission attestation");
  }
  const customTools = dependencies.custom_tool_factory === undefined
    ? dependencies.custom_tools
    : await dependencies.custom_tool_factory(packet, frame);
  const artifactSealer = dependencies.artifact_sealer_factory === undefined
    ? dependencies.artifact_sealer
    : await dependencies.artifact_sealer_factory(packet, frame);
  if (artifactSealer === undefined) throw new Error("artifact sealer is required");
  const requiredReadVerifier = dependencies.required_read_verifier_factory === undefined
    ? dependencies.required_read_verifier
    : await dependencies.required_read_verifier_factory(packet, frame);
  const terminalSubmission = dependencies.terminal_submission_factory === undefined
    ? dependencies.terminal_submission
    : await dependencies.terminal_submission_factory(packet, frame);
  const authority: AuthorityLiveness = {
    ...dependencies.authority,
    await_admission: () => Promise.resolve(frame),
  };
  return await runAssignment(packet, {
    ...dependencies,
    authority,
    artifact_sealer: artifactSealer,
    required_read_verifier: requiredReadVerifier,
    terminal_submission: terminalSubmission,
    custom_tools: customTools,
    // The caller's verifier is mandatory and must cross-check the decoded
    // packet against the kernel's immutable packet identity. The frame check
    // below only binds that verifier's input to this daemon admission; it is
    // not itself a cryptographic seal.
    packet_integrity_verifier: async (candidate) =>
      candidate.packet_digest === frame.packet_digest &&
      await dependencies.packet_integrity_verifier(candidate, packetBytes, frame.packet_digest),
  });
}

/** The process-custody convention is inherited connected FD 0, never a socket path. */
export function inheritedAuthority(
  file: Deno.FsFile = Deno.stdin as unknown as Deno.FsFile,
  transport?: InheritedFrameTransport,
): AuthorityLiveness {
  let admission: Promise<AuthorityAdmissionFrame> | undefined;
  return {
    file,
    await_admission: () => admission ??= readSessionAdmissionFrame(file),
    is_alive: async () => {
      if (transport !== undefined) return transport.isAlive();
      try {
        if (typeof file.stat === "function") await file.stat();
        return true;
      } catch {
        return false;
      }
    },
    on_loss: transport === undefined ? undefined : (listener) => transport.onLoss(listener),
  };
}

function record(
  value: unknown,
  name: string,
  required: readonly string[],
  optional: readonly string[] = [],
): Record<string, unknown> {
  if (value === null || typeof value !== "object" || Array.isArray(value)) {
    throw new Error(`${name} must be an object`);
  }
  const result = value as Record<string, unknown>;
  const allowed = new Set([...required, ...optional]);
  for (const key of Object.keys(result)) {
    if (!allowed.has(key)) throw new Error(`${name} has unknown field ${key}`);
  }
  for (const key of required) {
    if (!(key in result)) throw new Error(`${name} is missing field ${key}`);
  }
  return result;
}

function string(value: unknown, name: string): string {
  if (typeof value !== "string" || value.length === 0) {
    throw new Error(`${name} must be a nonempty string`);
  }
  return value;
}

function digest(value: unknown, name: string): string {
  const result = string(value, name);
  if (!/^[a-f0-9]{64}$/.test(result)) throw new Error(`${name} must be a lower-case BLAKE3 digest`);
  return result;
}

function numericIdentity(value: unknown, name: string): string {
  if (!Number.isSafeInteger(value) || (value as number) <= 0) {
    throw new Error(`${name} must be a positive safe integer`);
  }
  return String(value);
}

/** Aggregate revisions begin at zero; unlike database identities, zero is valid. */
function aggregateRevision(value: unknown): string {
  return String(integer(value, "aggregate_revision"));
}

function nullableNumericIdentity(value: unknown, name: string): string | null {
  if (value === null) return null;
  return numericIdentity(value, name);
}

function integer(value: unknown, name: string): number {
  if (!Number.isSafeInteger(value) || (value as number) < 0) {
    throw new Error(`${name} must be a safe integer`);
  }
  return value as number;
}

function identifier(value: unknown, name: string): number | string {
  if (typeof value === "string") return string(value, name);
  return integer(value, name);
}

function strings(value: unknown, name: string): string[] {
  if (!Array.isArray(value) || value.some((item) => typeof item !== "string")) {
    throw new Error(`${name} must be a string array`);
  }
  return [...value] as string[];
}

function oneOf<T extends string>(value: unknown, values: readonly T[], name: string): T {
  if (typeof value !== "string" || !values.includes(value as T)) {
    throw new Error(`${name} is invalid`);
  }
  return value as T;
}

function base64(value: unknown, name: string, maximumBytes?: number): Uint8Array {
  const encoded = string(value, name);
  if (
    encoded.length % 4 !== 0 || !/^[A-Za-z0-9+/]*={0,2}$/.test(encoded) ||
    encoded.includes("=") && !encoded.endsWith("=") && !encoded.endsWith("==")
  ) {
    throw new Error(`${name} is invalid base64`);
  }
  try {
    const binary = atob(encoded);
    if (maximumBytes !== undefined && binary.length > maximumBytes) {
      throw new Error(`decoded bytes exceed ${maximumBytes}`);
    }
    // Require the canonical base64 spelling so alternate/ambiguous text does
    // not become a second representation of the admitted packet bytes.
    if (btoa(binary) !== encoded) throw new Error("noncanonical base64");
    return Uint8Array.from(binary, (character) => character.charCodeAt(0));
  } catch (error) {
    throw new Error(`${name} is invalid base64: ${error instanceof Error ? error.message : error}`);
  }
}

function requiredReads(value: unknown): PiAssignmentPacket["required_reads"] {
  if (!Array.isArray(value)) throw new Error("required_reads must be an array");
  return value.map((item, index) => {
    const read = record(item, `required_reads[${index}]`, ["path", "digest", "reason"]);
    return {
      canonical_path: string(read.path, `required_reads[${index}].path`),
      blake3: digest(read.digest, `required_reads[${index}].digest`),
      reason: string(read.reason, `required_reads[${index}].reason`),
    };
  });
}

function assignmentEvidence(
  value: unknown,
  assignment_role: string,
): PiAssignmentPacket["assignment_evidence"] {
  if (!Array.isArray(value) || value.length > 24) {
    throw new Error("assignment_evidence must contain at most 24 references");
  }
  if (assignment_role === "product_research" && value.length !== 0) {
    throw new Error("Product assignment_evidence must be empty");
  }
  if (assignment_role !== "product_research" && value.length === 0) {
    throw new Error("Engineering and Quality assignment_evidence is required");
  }
  const known = new Set<string>();
  return value.map((item, index) => {
    const evidence = record(item, `assignment_evidence[${index}]`, [
      "role",
      "artifact_id",
      "digest",
      "byte_length",
    ]);
    const role = assignmentEvidenceRole(evidence.role, `assignment_evidence[${index}].role`);
    const byteLength = integer(evidence.byte_length, `assignment_evidence[${index}].byte_length`);
    if (known.has(role)) throw new Error("assignment_evidence roles must be unique");
    known.add(role);
    return {
      role,
      artifact_id: identifier(evidence.artifact_id, `assignment_evidence[${index}].artifact_id`),
      digest: digest(evidence.digest, `assignment_evidence[${index}].digest`),
      byte_length: byteLength,
    };
  });
}

function assignmentEvidenceRole(
  value: unknown,
  field: string,
): PiAssignmentPacket["assignment_evidence"][number]["role"] {
  return oneOf(
    value,
    ASSIGNMENT_EVIDENCE_ROLES_V1,
    field,
  ) as PiAssignmentPacket["assignment_evidence"][number]["role"];
}
