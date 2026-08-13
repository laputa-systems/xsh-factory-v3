import {
  type ActorToolV1,
  type ApplicationDefinitionV1,
  type CandidateSubmissionV1,
  compileApplicationV1,
  defineApplicationV1,
  type ProductTicketProposalV1,
  type QualityFullSuiteInvocationV1,
  type QualityReviewSubmissionV1,
  type TicketPolicyV1,
  validateCandidateSubmissionV1,
  validateProductTicketProposalV1,
  validateQualityFullSuiteInvocationV1,
  validateQualityReviewSubmissionV1,
} from "@factory/sdk";

const template = (source_path: string, digest: string, placeholders: readonly string[]) => ({
  source_path,
  digest,
  placeholders,
  rendered_byte_limit: 32_768,
});

const command = (
  name: string,
  argv: readonly string[],
  timeout_millis: number,
  stream_byte_limit: number,
) => ({
  name,
  executable: { approved_tool: "cargo" as const },
  argv,
  working_directory: ".",
  environment: [],
  timeout_millis,
  stdout_byte_limit: stream_byte_limit,
  stderr_byte_limit: stream_byte_limit,
  expected_exit_status: 0,
});

const commonActorTools: readonly ActorToolV1[] = [
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
];

const researchReviewModel = {
  provider: "openrouter",
  model_id: "deepseek/deepseek-v4-flash-0731",
  thinking_level: "high" as const,
  context_token_limit: 1_048_576,
  output_token_limit: 65_536,
  price_input_micro_usd_per_million_tokens: 90_000,
  price_output_micro_usd_per_million_tokens: 180_000,
  price_cache_read_micro_usd_per_million_tokens: 18_000,
  price_cache_write_micro_usd_per_million_tokens: 0,
  capability_flags: ["reasoning" as const],
};

const engineeringModel = {
  provider: "openrouter",
  model_id: "openai/gpt-5.6-luna",
  // Pi 0.84.1's frozen descriptor exposes `xhigh`, not `high`, for Luna.
  thinking_level: "xhigh" as const,
  context_token_limit: 1_050_000,
  output_token_limit: 128_000,
  price_input_micro_usd_per_million_tokens: 100_000,
  price_output_micro_usd_per_million_tokens: 600_000,
  price_cache_read_micro_usd_per_million_tokens: 10_000,
  price_cache_write_micro_usd_per_million_tokens: 125_000,
  capability_flags: ["reasoning" as const],
};

/**
 * Product buffer pressure is declarative application policy. The generic
 * kernel enforces these inequalities exactly but never decides which XSH
 * defect deserves sponsorship.
 */
export const xshProductTicketPolicyV1: TicketPolicyV1 = {
  low_water: 2,
  target: 3,
  maximum: 5,
  proposal_maximum: 3,
  ticket_bounds: {
    narrative_byte_limit: 16_384,
    acceptance_criteria_limit: 32,
    contract_read_limit: 16,
  },
};

/**
 * XSH's application-side proposal check adds one product policy fact to the
 * generic closed shape: the named contract owner must be among the contract
 * files Product supplies for later Engineering and Quality review.
 */
export function validateXshProductProposalV1(proposal: ProductTicketProposalV1): void {
  validateProductTicketProposalV1(proposal, xshProductTicketPolicyV1);
  if (!proposal.contract_reads.some((read) => read.path === proposal.contract_owner)) {
    throw new TypeError(
      "invalid XSH Product proposal: contract_owner must name one supplied contract read",
    );
  }
}

/**
 * XSH adds no prose-based escape hatch to Quality: the generic contract
 * already requires exact passed full-suite custody plus sealed, bounded
 * rationale, risks, and probes. This named validator keeps the product
 * boundary explicit for callers without giving XSH an authority callback.
 */
export function validateXshQualityReviewV1(review: QualityReviewSubmissionV1): void {
  validateQualityReviewSubmissionV1(review);
}

/** XSH permits only the application-owned `full` profile for Quality. */
export function validateXshQualityFullSuiteV1(input: QualityFullSuiteInvocationV1): void {
  validateQualityFullSuiteInvocationV1(input);
  if (input.validation_profile !== "full") {
    throw new TypeError("invalid XSH Quality operation: validation_profile must be full");
  }
}

/** Engineering adds no product-specific escape hatch to the closed candidate result. */
export function validateXshCandidateSubmissionV1(input: CandidateSubmissionV1): void {
  validateCandidateSubmissionV1(input);
}

export const xshApplicationV1: ApplicationDefinitionV1 = defineApplicationV1({
  format_version: 1,
  application_key: "xsh",
  predecessor_bundle: null,
  repository: {
    repository_key: "xsh-product",
    canonical_local_path: "/Users/josh/d/laputa-systems/xsh",
    default_branch: "master",
    delivery_mode: "local_fast_forward_only",
  },
  mission_template: template(
    "templates/mission.md",
    "238e6ad15801eba875197f4a96aed1345efab91df5728b35864d9ab7c2769bbb",
    [],
  ),
  office_profiles: [
    {
      office: "product_research",
      system_template: template(
        "templates/product-system.md",
        "93e73e897e39d4e47ba381841008ab665d6084a673400f3a72abce8de9da6e1a",
        ["ASSIGNMENT_ID", "MISSION"],
      ),
      assignment_template: template(
        "templates/product-assignment.md",
        "45af90aff658aaa330ee42e8fc54f7d2507eb2050d663facc1ffb13a1f7a5122",
        ["ASSIGNMENT_ID", "TARGET"],
      ),
      tools: [
        ...commonActorTools,
        "product_submit_ticket",
        "work_complete",
      ],
      model: researchReviewModel,
      // Product must inspect a real language implementation and prove a
      // deterministic reproducer; 24 turns was exhausted before terminal
      // submission in the first live campaign. The aggregate cost authority
      // remains the hard spend bound.
      limits: { turn_limit: 48, wall_limit_millis: 1_800_000, output_byte_limit: 67_108_864 },
    },
    {
      office: "engineering",
      system_template: template(
        "templates/engineering-system.md",
        "f4f856900042bc84f6862b8605297064150dddb7054c49a3abf8a26fa99c7071",
        ["ASSIGNMENT_ID", "MISSION"],
      ),
      assignment_template: template(
        "templates/engineering-assignment.md",
        "3160178e4d7c5981d60522f174afa6b43cf275ff863a4b30bc497709a64122b5",
        ["ASSIGNMENT_ID", "TARGET"],
      ),
      tools: [
        ...commonActorTools,
        "artifact_read",
        "candidate_checkpoint_regression",
        "candidate_submit",
      ],
      model: engineeringModel,
      limits: { turn_limit: 220, wall_limit_millis: 1_800_000, output_byte_limit: 67_108_864 },
    },
    {
      office: "quality",
      system_template: template(
        "templates/quality-system.md",
        "de14293ac66d6496c73649f2fe9feea886e8a31caf81c6a98eb920b10e442a29",
        ["ASSIGNMENT_ID", "MISSION"],
      ),
      assignment_template: template(
        "templates/quality-assignment.md",
        "be05a0a0d56c8dca558d51b955324a52b0f02f5fab0c419dce74cc73f467965e",
        ["ASSIGNMENT_ID", "TARGET"],
      ),
      tools: [
        ...commonActorTools,
        "artifact_read",
        "quality_run_full_suite",
        "quality_submit_review",
      ],
      model: researchReviewModel,
      limits: { turn_limit: 24, wall_limit_millis: 1_800_000, output_byte_limit: 67_108_864 },
    },
  ],
  ticket_policy: xshProductTicketPolicyV1,
  required_reads: [
    { path: "AGENTS.md", reason: "product operating contract" },
    { path: "docs/CHAPTER-01-why-xsh.md", reason: "product mission" },
    { path: "docs/TEST-MAP.md", reason: "authoritative validation map" },
  ],
  reproducer_profiles: [
    command(
      "reproducer",
      ["run", "--quiet", "--locked", "--bin", "xsh", "--", "/dev/stdin"],
      300_000,
      4_194_304,
    ),
  ],
  validation_profiles: {
    focused: [command("focused", ["test", "--locked", "-p", "xsh", "--lib"], 900_000, 67_108_864)],
    full: [command("full", ["test", "--locked"], 1_800_000, 67_108_864)],
  },
  git_policy: {
    forbidden_paths: [".git"],
    delivery_mode: "local_fast_forward_only",
    provenance_trailers_required: true,
  },
  commit_message_policy: { subject_byte_limit: 72, body_byte_limit: 4096 },
});

/**
 * Installation-only compiler entrypoint. Imported application declarations
 * remain inert; executing this module directly validates all seven templates
 * and emits only the canonical closed bundle to stdout.
 */
if (import.meta.main) {
  const sourceRoot = new URL("./", import.meta.url);
  if (sourceRoot.protocol !== "file:") {
    throw new TypeError("the XSH application compiler requires a local file source root");
  }
  const compiled = await compileApplicationV1(
    xshApplicationV1,
    decodeURIComponent(sourceRoot.pathname),
  );
  await Deno.stdout.write(compiled.canonical_bytes);
}
