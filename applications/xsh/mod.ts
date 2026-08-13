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
  expected_exit_status = 0,
) => ({
  name,
  executable: { approved_tool: "cargo" as const },
  argv,
  working_directory: ".",
  environment: [],
  timeout_millis,
  stdout_byte_limit: stream_byte_limit,
  stderr_byte_limit: stream_byte_limit,
  expected_exit_status,
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
];

// Product is deliberately a narrow evidence collector.  It can read the
// assigned contracts, make the one supplied reproduction run, seal that
// observation, and submit it.  Source discovery, implementation, and review
// belong to the later assignments, so exposing their tools here only creates
// duplicated paid work.
const productActorTools: readonly ActorToolV1[] = [
  "workspace_read",
  "shell",
  "artifact_seal",
  "product_submit_ticket",
  "work_complete",
];

const researchReviewModel = {
  provider: "openrouter",
  model_id: "deepseek/deepseek-v4-flash-0731",
  thinking_level: "high" as const,
  context_token_limit: 1_048_576,
  output_token_limit: 384_000,
  price_input_micro_usd_per_million_tokens: 80_000,
  price_output_micro_usd_per_million_tokens: 180_000,
  price_cache_read_micro_usd_per_million_tokens: 16_000,
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

// Product has one prescribed evidence-collection path. A reliable no-
// reasoning model is less expensive than repeatedly paying a weaker model to
// rediscover the checkout or implementation details that Product does not own.
const productModel = { ...engineeringModel, thinking_level: "none" as const };

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
  // The installed V1 bundle is the explicit lineage parent for this
  // validation-profile and prompt revision. Admission rejects an accidental
  // fork from any other active application bundle.
  predecessor_bundle: "55578f0f41072ce4d47dff7d968a96c5e225175035ec4fbe52831fe571e1a877",
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
        "cbd4124015d8a62cd8cc3e0340b8d2cfd507e186097fa67cd872acb53f1b5edb",
        ["ASSIGNMENT_ID", "MISSION"],
      ),
      assignment_template: template(
        "templates/product-assignment.md",
        "45af90aff658aaa330ee42e8fc54f7d2507eb2050d663facc1ffb13a1f7a5122",
        ["ASSIGNMENT_ID", "TARGET"],
      ),
      tools: productActorTools,
      model: productModel,
      limits: { turn_limit: 12, wall_limit_millis: 600_000, output_byte_limit: 16_777_216 },
    },
    {
      office: "engineering",
      system_template: template(
        "templates/engineering-system.md",
        "7e96b5c147fee2da3163087eb6c1c3e361425859372bdd4b387467d6f827adda",
        ["ASSIGNMENT_ID", "MISSION"],
      ),
      assignment_template: template(
        "templates/engineering-assignment.md",
        "dd247f1695771d16d869bf126cf1121086b976eb91ea48b039b5a91a500d86c0",
        [
          "ASSIGNMENT_ID",
          "TARGET",
          "REGRESSION_COMMAND",
          "REGRESSION_EXPECTED_FAILURE",
        ],
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
        "artifact_seal",
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
      3,
    ),
  ],
  validation_profiles: {
    focused: [command("focused", ["test", "--locked", "-p", "xsh", "--lib"], 900_000, 67_108_864)],
    // `cargo test` includes integration coverage that assumes a product
    // checkout's development fixtures. The XSH integration target is the
    // supported, hermetic full behavioral suite for an isolated candidate
    // worktree.
    full: [command("full", ["test", "--locked", "--test", "integration"], 1_800_000, 67_108_864)],
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
