/** Generic, sealed inputs and physical seams for one paid assignment. */

export type HostToolName =
  | "workspace_read"
  | "workspace_write"
  | "workspace_edit"
  | "workspace_search"
  | "workspace_list"
  | "shell"
  | "forum_read"
  | "forum_write"
  | "forum_search"
  | "forum_list_topics"
  | "forum_list_threads"
  | "forum_read_thread"
  | "forum_create_topic"
  | "forum_create_thread"
  | "forum_post"
  | "artifact_seal"
  | "artifact_read"
  | "product_submit_ticket"
  | "candidate_checkpoint_regression"
  | "candidate_submit"
  | "quality_run_full_suite"
  | "quality_submit_review"
  | "work_complete";

export interface PiModelDescriptor {
  readonly provider: string;
  readonly model_id: string;
  readonly thinking_level: "none" | "low" | "medium" | "high" | "xhigh";
  readonly context_token_limit: number;
  readonly output_token_limit: number;
  readonly price_input_micro_usd_per_million_tokens: number;
  readonly price_output_micro_usd_per_million_tokens: number;
  readonly price_cache_read_micro_usd_per_million_tokens: number;
  readonly price_cache_write_micro_usd_per_million_tokens: number;
  readonly capability_flags: readonly ModelCapabilityV1[];
}

export type ModelCapabilityV1 = "reasoning";

export type CredentialSource =
  | { readonly kind: "pi_auth_store"; readonly path: string }
  | { readonly kind: "environment"; readonly name: string };

export interface RequiredReadAssertion {
  readonly canonical_path: string;
  readonly blake3: string;
  readonly reason: string;
}

export interface PiAssignmentPacket {
  readonly format_version: 1;
  readonly assignment_id: string;
  readonly office: string;
  readonly campaign_id: string;
  readonly application_revision_id: string;
  readonly kernel_build_id: string;
  readonly repository_base_identity: string;
  readonly factory_base_identity: string;
  readonly target: string;
  /** Immutable durable context; custom tools never choose these identities. */
  readonly ticket_attempt_id: string | null;
  /** Immutable Quality candidate context paired with `ticket_attempt_id`. */
  readonly candidate_id: string | null;
  /** Digest of the immutable upstream packet, checked by the required verifier. */
  readonly packet_digest: string;
  readonly system_prompt_artifact_id: number | string;
  readonly assignment_prompt_artifact_id: number | string;
  readonly required_read_manifest_artifact_id: number | string;
  readonly system_prompt_digest: string;
  readonly assignment_prompt_digest: string;
  readonly aggregate_revision: string;
  readonly runtime: AssignmentRuntimeDescriptor;
  readonly aggregate_cost_remaining_micro_usd: number;
  readonly legal_terminal_operations: readonly HostToolName[];
  readonly workspace_root: string;
  readonly staging_root: string;
  /** Final prompt bytes are sealed upstream and are immutable host inputs. */
  readonly system_prompt_bytes: Uint8Array;
  readonly assignment_prompt_bytes: Uint8Array;
  readonly model: PiModelDescriptor;
  readonly limits: {
    readonly turn_limit: number;
    readonly wall_limit_millis: number;
    readonly output_byte_limit: number;
  };
  readonly tools: readonly HostToolName[];
  readonly required_reads: readonly RequiredReadAssertion[];
  readonly terminal_submission_required?: boolean;
}

/** Cross-language name for the closed packet; PiAssignmentPacket is the ergonomic view. */
export type AssignmentPacketV1 = PiAssignmentPacket;

export interface AssignmentRuntimeDescriptor {
  readonly deno_executable: string;
  readonly deno_version: string;
  readonly source_graph_digest: string;
  readonly resolved_dependency_graph_digest: string;
  readonly deno_json_digest: string;
  readonly deno_lock_digest: string;
  readonly pi_version: string;
  readonly credential_source: CredentialSource;
}

/** A custom SDK tool supplied by the daemon/adapter, already authority-bound. */
export interface PiToolAdapter {
  readonly name: HostToolName;
  readonly sdk_definition: {
    readonly description: string;
    readonly input_schema: Readonly<Record<string, unknown>>;
    readonly invoke: (input: unknown) => Promise<unknown>;
  };
}

/** The daemon-bound wrapper independently hashes exact workspace-read bytes. */
export interface RequiredReadVerifier {
  verify(result: unknown): Promise<RequiredReadObservation | undefined>;
}

export interface PiSessionLike {
  subscribe(listener: (event: unknown) => void): () => void;
  prompt(text: string): Promise<void>;
  dispose(): void;
  abort?(): Promise<void>;
}

export interface PiSessionFactoryContext {
  readonly authority_file?: Deno.FsFile;
  readonly custom_tools: readonly PiToolAdapter[];
}

export interface PiSessionFactory {
  create(
    packet: PiAssignmentPacket,
    context: PiSessionFactoryContext,
  ): Promise<PiSessionLike>;
}

export interface AuthorityLiveness {
  /** This is the inherited connected daemon file, never a socket path. */
  readonly file: Deno.FsFile;
  /** Waits for the daemon's one-time admission frame before Pi construction. */
  readonly await_admission: () => Promise<AuthorityAdmissionFrame>;
  is_alive(): boolean | Promise<boolean>;
  on_loss?(listener: () => void): () => void;
}

/** Exact startup gate carried over the inherited connected actor on stdin (FD 0). */
export interface AuthorityAdmissionFrame {
  readonly type: "session.admitted";
  readonly protocol_version: 1;
  readonly assignment_id: string;
  readonly session_id: number;
  /** Kernel session revision used for idempotent daemon-bound writes. */
  readonly session_revision: number;
  readonly packet_digest: string;
  /** Canonical JSON bytes, base64-encoded only for this startup frame. */
  readonly packet_b64: string;
}

export interface ArtifactSealReceipt {
  readonly artifact_id: number | string;
  readonly digest: string;
  readonly byte_length: number;
}

export interface ArtifactSealer {
  seal(
    path: string,
    role: "pi_transcript_gzip" | "required_read_manifest",
  ): Promise<ArtifactSealReceipt>;
}

export interface TerminalSubmission {
  submit(
    operation: HostToolName | null,
    payload: unknown,
    manifest: RequiredReadManifest,
    summary: NormalizedSessionSummary,
  ): Promise<void>;
}

export interface RequiredReadObservation {
  readonly canonical_path: string;
  readonly blake3: string;
  readonly success: boolean;
}

export interface RequiredReadManifest {
  readonly required: readonly RequiredReadAssertion[];
  readonly satisfied: readonly RequiredReadAssertion[];
  readonly observed: readonly RequiredReadObservation[];
  readonly missing: readonly RequiredReadAssertion[];
}

export interface NormalizedSessionSummary {
  readonly assignment_id: string;
  readonly office: string;
  readonly stop_reason: string;
  readonly failure_reason: string | null;
  readonly cost_status: "known" | "unknown";
  readonly cost_micro_usd: number | null;
  readonly turns: number;
  readonly tokens: {
    readonly input: number;
    readonly output: number;
    readonly cache_read: number;
    readonly cache_write: number;
    readonly reasoning: number | null;
  };
  readonly output_bytes: number;
  readonly tool_calls: number;
  readonly nonzero_tool_results: number;
  readonly retries: number;
  readonly model: PiModelDescriptor;
  readonly runtime: {
    readonly deno_version: string;
    readonly pi_sdk_version: string;
  };
  readonly transcript_path: string;
  readonly transcript_gzip_path: string;
  readonly transcript_artifact: ArtifactSealReceipt | null;
  readonly required_read_manifest_path: string;
  readonly required_read_manifest_artifact: ArtifactSealReceipt | null;
}

export interface PiHostResult {
  readonly status:
    | "succeeded"
    | "cost_unknown"
    | "disconnected"
    | "required_reads_missing"
    | "terminal_submission_missing"
    | "failed";
  readonly summary: NormalizedSessionSummary;
  readonly required_read_manifest: RequiredReadManifest;
  readonly error: string | null;
}
