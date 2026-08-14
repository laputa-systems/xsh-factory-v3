/**
 * Closed V1 application authoring data. This is intentionally data only: no
 * callback, metadata, predicate, custom tool, or plugin field can cross the
 * generic kernel boundary.
 */
export type OfficeV1 = "product_research" | "engineering" | "quality";

export type ActorToolV1 =
  | "workspace_read"
  | "workspace_write"
  | "workspace_edit"
  | "workspace_search"
  | "workspace_list"
  | "shell"
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

export type ThinkingLevelV1 = "none" | "low" | "medium" | "high" | "xhigh";
export type ModelCapabilityV1 = "reasoning";
export type ApprovedToolV1 = "cargo" | "git" | "deno";
export type DeliveryModeV1 = "local_fast_forward_only";

export interface TemplateDeclarationV1 {
  readonly source_path: string;
  readonly placeholders: readonly string[];
  readonly rendered_byte_limit: number;
}

/** A template declaration after the compiler binds it to exact source bytes. */
export interface TemplateArtifactV1 extends TemplateDeclarationV1 {
  readonly digest: string;
}

export interface ModelProfileV1 {
  readonly provider: string;
  readonly model_id: string;
  readonly thinking_level: ThinkingLevelV1;
  readonly context_token_limit: number;
  readonly output_token_limit: number;
  readonly price_input_micro_usd_per_million_tokens: number;
  readonly price_output_micro_usd_per_million_tokens: number;
  readonly price_cache_read_micro_usd_per_million_tokens: number;
  readonly price_cache_write_micro_usd_per_million_tokens: number;
  readonly capability_flags: readonly ModelCapabilityV1[];
}

export interface SessionLimitsV1 {
  readonly turn_limit: number;
  readonly wall_limit_millis: number;
  readonly output_byte_limit: number;
}

export interface OfficeProfileV1 {
  readonly office: OfficeV1;
  readonly system_template: TemplateArtifactV1;
  readonly assignment_template: TemplateArtifactV1;
  readonly tools: readonly ActorToolV1[];
  readonly model: ModelProfileV1;
  readonly limits: SessionLimitsV1;
}

export interface RepositoryBindingV1 {
  readonly repository_key: string;
  readonly canonical_local_path: string;
  readonly default_branch: string;
  readonly delivery_mode: DeliveryModeV1;
}

export interface TicketPolicyV1 {
  readonly low_water: number;
  readonly target: number;
  readonly maximum: number;
  readonly proposal_maximum: number;
  readonly ticket_bounds: {
    readonly narrative_byte_limit: number;
    readonly acceptance_criteria_limit: number;
    readonly contract_read_limit: number;
  };
}

export interface RequiredReadV1 {
  readonly path: string;
  readonly reason: string;
}

export type ExecutableV1 =
  | { readonly approved_tool: ApprovedToolV1 }
  | { readonly repository_path: string };

export interface EnvironmentAdditionV1 {
  readonly name: string;
  readonly value: string;
}

export interface CommandProfileV1 {
  readonly name: string;
  readonly executable: ExecutableV1;
  readonly argv: readonly string[];
  readonly working_directory: string;
  readonly environment: readonly EnvironmentAdditionV1[];
  readonly timeout_millis: number;
  readonly stdout_byte_limit: number;
  readonly stderr_byte_limit: number;
  readonly expected_exit_status: number;
}

export interface ValidationProfilesV1 {
  readonly focused: readonly CommandProfileV1[];
  readonly full: readonly CommandProfileV1[];
}

export interface GitPolicyV1 {
  readonly forbidden_paths: readonly string[];
  readonly delivery_mode: DeliveryModeV1;
  readonly provenance_trailers_required: true;
}

export interface CommitMessagePolicyV1 {
  readonly subject_byte_limit: number;
  readonly body_byte_limit: number;
}

/** The exact Deno representation of Rust `ApplicationBundleV1`. */
export interface ApplicationBundleV1 {
  readonly format_version: 1;
  readonly application_key: string;
  readonly predecessor_bundle: string | null;
  readonly repository: RepositoryBindingV1;
  readonly mission_template: TemplateArtifactV1;
  readonly office_profiles: readonly OfficeProfileV1[];
  readonly ticket_policy: TicketPolicyV1;
  readonly required_reads: readonly RequiredReadV1[];
  readonly reproducer_profiles: readonly CommandProfileV1[];
  readonly validation_profiles: ValidationProfilesV1;
  readonly git_policy: GitPolicyV1;
  readonly commit_message_policy: CommitMessagePolicyV1;
}

/** Application authoring data before source templates are hashed. */
export type ApplicationSourceOfficeProfileV1 =
  & Omit<
    OfficeProfileV1,
    "system_template" | "assignment_template"
  >
  & {
    readonly system_template: TemplateDeclarationV1;
    readonly assignment_template: TemplateDeclarationV1;
  };

/** Application source data is completed into an admitted bundle by the compiler. */
export type ApplicationSourceBundleV1 =
  & Omit<
    ApplicationBundleV1,
    "mission_template" | "office_profiles"
  >
  & {
    readonly mission_template: TemplateDeclarationV1;
    readonly office_profiles: readonly ApplicationSourceOfficeProfileV1[];
  };

/** A validated, immutable V1 bundle with digests bound to exact source bytes. */
export type ApplicationDefinitionV1 = Readonly<ApplicationBundleV1>;
/** A validated, immutable application source declaration for the compiler. */
export type ApplicationSourceDefinitionV1 = Readonly<ApplicationSourceBundleV1>;

const byteLength = new TextEncoder();
const offices: readonly OfficeV1[] = ["product_research", "engineering", "quality"];
const tools: readonly ActorToolV1[] = [
  "workspace_read",
  "workspace_write",
  "workspace_edit",
  "workspace_search",
  "workspace_list",
  "shell",
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
];

/**
 * Defines closed, non-executable application data and rejects unknown fields
 * at every object boundary. The result is recursively frozen so a later caller
 * cannot mutate policy after validation.
 */
export function defineApplicationV1(input: ApplicationBundleV1): ApplicationDefinitionV1 {
  validateApplication(input, true);
  return deepFreeze(input) as ApplicationDefinitionV1;
}

/**
 * Defines application source data whose template digests are supplied by the
 * concrete compiler after it reads the declared files.
 */
export function defineApplicationSourceV1(
  input: ApplicationSourceBundleV1,
): ApplicationSourceDefinitionV1 {
  validateApplication(input, false);
  return deepFreeze(input) as ApplicationSourceDefinitionV1;
}

function validateApplication(
  input: ApplicationBundleV1 | ApplicationSourceBundleV1,
  requireTemplateDigests: boolean,
): void {
  exactObject(input, "application", [
    "format_version",
    "application_key",
    "predecessor_bundle",
    "repository",
    "mission_template",
    "office_profiles",
    "ticket_policy",
    "required_reads",
    "reproducer_profiles",
    "validation_profiles",
    "git_policy",
    "commit_message_policy",
  ]);
  if (input.format_version !== 1) fail("application.format_version must be 1");
  applicationKey(input.application_key, "application.application_key");
  if (input.predecessor_bundle !== null) {
    digest(input.predecessor_bundle, "application.predecessor_bundle");
  }
  repository(input.repository);
  template(input.mission_template, "application.mission_template", requireTemplateDigests);
  officeProfiles(input.office_profiles, requireTemplateDigests);
  ticketPolicy(input.ticket_policy);
  requiredReads(input.required_reads);
  commands(input.reproducer_profiles, "application.reproducer_profiles", false);
  validations(input.validation_profiles);
  gitPolicy(input.git_policy);
  commitMessagePolicy(input.commit_message_policy);
}

function repository(value: RepositoryBindingV1): void {
  exactObject(value, "repository", [
    "repository_key",
    "canonical_local_path",
    "default_branch",
    "delivery_mode",
  ]);
  text(value.repository_key, "repository.repository_key", 160);
  absolutePath(value.canonical_local_path, "repository.canonical_local_path");
  text(value.default_branch, "repository.default_branch", 240);
  if (/\s|\.\.|\/$/.test(value.default_branch)) fail("repository.default_branch is unsafe");
  if (value.delivery_mode !== "local_fast_forward_only") {
    fail("repository.delivery_mode is invalid");
  }
}

function template(
  value: TemplateDeclarationV1 | TemplateArtifactV1,
  location: string,
  requireDigest: boolean,
): void {
  exactObject(
    value,
    location,
    requireDigest
      ? ["source_path", "digest", "placeholders", "rendered_byte_limit"]
      : ["source_path", "placeholders", "rendered_byte_limit"],
  );
  relativePath(value.source_path, `${location}.source_path`);
  if (requireDigest) {
    if (!("digest" in value)) fail(`${location}.digest is required`);
    digest(value.digest, `${location}.digest`);
  }
  positiveInteger(value.rendered_byte_limit, `${location}.rendered_byte_limit`);
  const known = new Set<string>();
  for (const placeholder of value.placeholders) {
    if (!/^[A-Z0-9_]{1,64}$/.test(placeholder)) fail(`${location}.placeholders is invalid`);
    if (known.has(placeholder)) fail(`${location}.placeholders has a duplicate`);
    known.add(placeholder);
  }
}

function officeProfiles(
  profiles: readonly (OfficeProfileV1 | ApplicationSourceOfficeProfileV1)[],
  requireTemplateDigests: boolean,
): void {
  if (profiles.length !== offices.length) {
    fail("application.office_profiles must have every fixed office");
  }
  const known = new Set<OfficeV1>();
  for (const profile of profiles) {
    exactObject(profile, "office profile", [
      "office",
      "system_template",
      "assignment_template",
      "tools",
      "model",
      "limits",
    ]);
    if (!offices.includes(profile.office) || known.has(profile.office)) {
      fail("application.office_profiles must have one of each fixed office");
    }
    known.add(profile.office);
    template(profile.system_template, "office profile.system_template", requireTemplateDigests);
    template(
      profile.assignment_template,
      "office profile.assignment_template",
      requireTemplateDigests,
    );
    officeTools(profile.office, profile.tools);
    model(profile.model);
    sessionLimits(profile.limits);
  }
}

function officeTools(office: OfficeV1, values: readonly ActorToolV1[]): void {
  if (values.length === 0) fail("office profile.tools must not be empty");
  const known = new Set<ActorToolV1>();
  for (const tool of values) {
    if (!tools.includes(tool) || known.has(tool)) fail("office profile.tools is invalid");
    known.add(tool);
    if (tool === "product_submit_ticket" && office !== "product_research") {
      fail("product tool wrong office");
    }
    if (
      (tool === "candidate_checkpoint_regression" || tool === "candidate_submit") &&
      office !== "engineering"
    ) {
      fail("candidate tool wrong office");
    }
    if (
      (tool === "quality_run_full_suite" || tool === "quality_submit_review") &&
      office !== "quality"
    ) {
      fail("quality tool wrong office");
    }
  }
}

function model(value: ModelProfileV1): void {
  exactObject(value, "model", [
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
  text(value.provider, "model.provider", 160);
  text(value.model_id, "model.model_id", 240);
  if (!["none", "low", "medium", "high", "xhigh"].includes(value.thinking_level)) {
    fail("model.thinking_level");
  }
  positiveInteger(value.context_token_limit, "model.context_token_limit");
  positiveInteger(value.output_token_limit, "model.output_token_limit");
  nonnegativeInteger(value.price_input_micro_usd_per_million_tokens, "model.input price");
  nonnegativeInteger(value.price_output_micro_usd_per_million_tokens, "model.output price");
  nonnegativeInteger(value.price_cache_read_micro_usd_per_million_tokens, "model.cache read price");
  nonnegativeInteger(
    value.price_cache_write_micro_usd_per_million_tokens,
    "model.cache write price",
  );
  const flags = new Set<ModelCapabilityV1>();
  for (const flag of value.capability_flags) {
    if (flag !== "reasoning" || flags.has(flag)) fail("model.capability_flags");
    flags.add(flag);
  }
}

function sessionLimits(value: SessionLimitsV1): void {
  exactObject(value, "session limits", ["turn_limit", "wall_limit_millis", "output_byte_limit"]);
  positiveInteger(value.turn_limit, "session limits.turn_limit");
  positiveInteger(value.wall_limit_millis, "session limits.wall_limit_millis");
  positiveInteger(value.output_byte_limit, "session limits.output_byte_limit");
}

function ticketPolicy(value: TicketPolicyV1): void {
  exactObject(value, "ticket policy", [
    "low_water",
    "target",
    "maximum",
    "proposal_maximum",
    "ticket_bounds",
  ]);
  positiveInteger(value.low_water, "ticket policy.low_water");
  positiveInteger(value.target, "ticket policy.target");
  positiveInteger(value.maximum, "ticket policy.maximum");
  positiveInteger(value.proposal_maximum, "ticket policy.proposal_maximum");
  if (value.low_water > value.target || value.target > value.maximum) fail("ticket policy bounds");
  exactObject(value.ticket_bounds, "ticket bounds", [
    "narrative_byte_limit",
    "acceptance_criteria_limit",
    "contract_read_limit",
  ]);
  positiveInteger(value.ticket_bounds.narrative_byte_limit, "ticket bounds.narrative_byte_limit");
  positiveInteger(
    value.ticket_bounds.acceptance_criteria_limit,
    "ticket bounds.acceptance_criteria_limit",
  );
  positiveInteger(value.ticket_bounds.contract_read_limit, "ticket bounds.contract_read_limit");
}

function requiredReads(values: readonly RequiredReadV1[]): void {
  if (values.length === 0) fail("application.required_reads must not be empty");
  const known = new Set<string>();
  for (const value of values) {
    exactObject(value, "required read", ["path", "reason"]);
    relativePath(value.path, "required read.path");
    // This exact text becomes assignment-packet provenance, whose closed
    // wire contract permits at most 240 bytes.
    text(value.reason, "required read.reason", 240);
    if (known.has(value.path)) fail("required read path is duplicated");
    known.add(value.path);
  }
}

function commands(values: readonly CommandProfileV1[], location: string, required: boolean): void {
  if (required && values.length === 0) fail(`${location} must not be empty`);
  const known = new Set<string>();
  for (const value of values) {
    exactObject(value, "command", [
      "name",
      "executable",
      "argv",
      "working_directory",
      "environment",
      "timeout_millis",
      "stdout_byte_limit",
      "stderr_byte_limit",
      "expected_exit_status",
    ]);
    text(value.name, "command.name", 160);
    if (known.has(value.name)) fail(`${location} command is duplicated`);
    known.add(value.name);
    executable(value.executable);
    for (const argument of value.argv) {
      if (argument.includes("\0")) fail("command.argv contains NUL");
    }
    relativePath(value.working_directory, "command.working_directory");
    environment(value.environment);
    positiveInteger(value.timeout_millis, "command.timeout_millis");
    positiveInteger(value.stdout_byte_limit, "command.stdout_byte_limit");
    positiveInteger(value.stderr_byte_limit, "command.stderr_byte_limit");
    if (!Number.isInteger(value.expected_exit_status)) fail("command.expected_exit_status");
  }
}

function executable(value: ExecutableV1): void {
  if (typeof value !== "object" || value === null || Array.isArray(value)) {
    fail("command.executable must be an object");
  }
  if (Object.getPrototypeOf(value) !== Object.prototype) {
    fail("command.executable has an unsupported prototype");
  }
  const keys = Reflect.ownKeys(value);
  if (keys.length !== 1 || (keys[0] !== "approved_tool" && keys[0] !== "repository_path")) {
    fail("command.executable must have exactly one closed variant");
  }
  const tool = Reflect.get(value, "approved_tool");
  const path = Reflect.get(value, "repository_path");
  if (tool !== undefined) {
    if (tool !== "cargo" && tool !== "git" && tool !== "deno") fail("command.approved_tool");
    return;
  }
  if (path !== undefined) {
    relativePath(expectedString(path, "command.repository_path"), "command.repository_path");
    return;
  }
  fail("command.executable has no variant");
}

function environment(values: readonly EnvironmentAdditionV1[]): void {
  const names = new Set<string>();
  for (const value of values) {
    exactObject(value, "environment", ["name", "value"]);
    if (!/^[A-Z0-9_]{1,160}$/.test(value.name)) fail("environment.name");
    if (value.value.includes("\0")) fail("environment.value contains NUL");
    if (names.has(value.name)) fail("environment.name is duplicated");
    names.add(value.name);
  }
}

function validations(value: ValidationProfilesV1): void {
  exactObject(value, "validation profiles", ["focused", "full"]);
  commands(value.focused, "validation.focused", true);
  commands(value.full, "validation.full", true);
}

function gitPolicy(value: GitPolicyV1): void {
  exactObject(value, "Git policy", [
    "forbidden_paths",
    "delivery_mode",
    "provenance_trailers_required",
  ]);
  const known = new Set<string>();
  for (const path of value.forbidden_paths) {
    relativePath(path, "Git policy.forbidden_paths");
    if (known.has(path)) fail("Git policy.forbidden_paths is duplicated");
    known.add(path);
  }
  if (value.delivery_mode !== "local_fast_forward_only") fail("Git policy.delivery_mode");
  if (value.provenance_trailers_required !== true) fail("Git policy.provenance_trailers_required");
}

function commitMessagePolicy(value: CommitMessagePolicyV1): void {
  exactObject(value, "commit message policy", ["subject_byte_limit", "body_byte_limit"]);
  positiveInteger(value.subject_byte_limit, "commit message policy.subject_byte_limit");
  positiveInteger(value.body_byte_limit, "commit message policy.body_byte_limit");
}

function exactObject(
  value: unknown,
  location: string,
  keys: readonly string[],
): asserts value is object {
  if (typeof value !== "object" || value === null || Array.isArray(value)) {
    fail(`${location} must be an object`);
  }
  if (Object.getPrototypeOf(value) !== Object.prototype) {
    fail(`${location} has an unsupported prototype`);
  }
  const actualStrings = Reflect.ownKeys(value)
    .map((key) => {
      if (typeof key !== "string") fail(`${location} has a symbol field`);
      return key;
    })
    .sort();
  const expected = [...keys].sort();
  if (
    actualStrings.length !== expected.length ||
    actualStrings.some((key, index) => key !== expected[index])
  ) {
    fail(`${location} has an unknown, missing, or non-enumerable field`);
  }
}

function applicationKey(value: string, location: string): void {
  if (!/^[a-z0-9-]{1,80}$/.test(value)) fail(`${location} must be a lower-case key`);
}

function text(value: string, location: string, maximum: number): void {
  if (
    typeof value !== "string" || value.length === 0 ||
    byteLength.encode(value).byteLength > maximum || value.includes("\0")
  ) {
    fail(`${location} is invalid`);
  }
}

function expectedString(value: unknown, location: string): string {
  if (typeof value !== "string") fail(`${location} must be a string`);
  return value;
}

function digest(value: string, location: string): void {
  if (!/^[a-f0-9]{64}$/.test(value)) fail(`${location} must be a lower-case BLAKE3 digest`);
}

function relativePath(value: string, location: string): void {
  if (
    typeof value !== "string" ||
    value.length === 0 ||
    value.includes("\0") ||
    value.startsWith("/") ||
    value.includes("\\") ||
    (value !== "." &&
      value.split("/").some((segment) => segment === "" || segment === "." || segment === ".."))
  ) {
    fail(`${location} must be a safe relative path`);
  }
}

function absolutePath(value: string, location: string): void {
  if (typeof value !== "string" || !value.startsWith("/") || value.includes("\0")) {
    fail(`${location} must be an absolute host path`);
  }
}

function positiveInteger(value: number, location: string): void {
  if (!Number.isSafeInteger(value) || value <= 0) fail(`${location} must be a positive integer`);
}

function nonnegativeInteger(value: number, location: string): void {
  if (!Number.isSafeInteger(value) || value < 0) fail(`${location} must be a nonnegative integer`);
}

function deepFreeze<T>(value: T): T {
  if (typeof value === "object" && value !== null && !Object.isFrozen(value)) {
    for (const child of Object.values(value)) deepFreeze(child);
    Object.freeze(value);
  }
  return value;
}

function fail(message: string): never {
  throw new TypeError(`invalid ApplicationBundleV1: ${message}`);
}
