import {
  type ActorToolV1,
  type ApplicationSourceDefinitionV1,
  type CandidateSubmissionV1,
  compileApplicationV1,
  defineApplicationSourceV1,
  type ProductTicketProposalV1,
  type QualityFullSuiteInvocationV1,
  type QualityReviewSubmissionV1,
  type TicketPolicyV1,
  validateCandidateSubmissionV1,
  validateProductTicketProposalV1,
  validateQualityFullSuiteInvocationV1,
  validateQualityReviewSubmissionV1,
} from "@factory/sdk";

const template = (source_path: string, placeholders: readonly string[]) => ({
  source_path,
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
  "publication_create",
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

export const xshApplicationV1: ApplicationSourceDefinitionV1 = defineApplicationSourceV1({
  format_version: 1,
  application_key: "xsh",
  // This policy revision succeeds the fresh-registry root already admitted to
  // the live registry. Pinning that exact bundle makes a policy change an
  // append-only lineage step rather than an accidental application fork.
  predecessor_bundle: "932106af6a4808cf4c2c26c9f2819bc0dfd6742635c73e0d3dc9149d374e6c4f",
  repository: {
    repository_key: "xsh-product",
    canonical_local_path: "/Users/josh/d/laputa-systems/xsh",
    default_branch: "master",
    delivery_mode: "local_fast_forward_only",
  },
  mission_template: template(
    "templates/mission.md",
    [],
  ),
  assignment_role_profiles: [
    {
      assignment_role: "product_research",
      system_template: template(
        "templates/product-system.md",
        ["ASSIGNMENT_ID", "MISSION"],
      ),
      assignment_template: template(
        "templates/product-assignment.md",
        ["ASSIGNMENT_ID", "TARGET"],
      ),
      tools: productActorTools,
      model: productModel,
      limits: { turn_limit: 12, wall_limit_millis: 600_000, output_byte_limit: 16_777_216 },
    },
    {
      assignment_role: "engineering",
      system_template: template(
        "templates/engineering-system.md",
        ["ASSIGNMENT_ID", "MISSION"],
      ),
      assignment_template: template(
        "templates/engineering-assignment.md",
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
      assignment_role: "quality",
      system_template: template(
        "templates/quality-system.md",
        ["ASSIGNMENT_ID", "MISSION"],
      ),
      assignment_template: template(
        "templates/quality-assignment.md",
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
