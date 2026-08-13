import { assert, assertEquals, assertThrows } from "@std/assert";
import {
  type CandidateSubmissionV1,
  PRODUCT_SUBMIT_TICKET_INPUT_SCHEMA_V1,
  type ProductTicketProposalV1,
  QUALITY_RUN_FULL_SUITE_INPUT_SCHEMA_V1,
  QUALITY_SUBMIT_REVIEW_INPUT_SCHEMA_V1,
  type QualityReviewSubmissionV1,
} from "@factory/sdk";
import {
  validateXshCandidateSubmissionV1,
  validateXshProductProposalV1,
  validateXshQualityFullSuiteV1,
  validateXshQualityReviewV1,
  xshApplicationV1,
  xshProductTicketPolicyV1,
} from "../applications/xsh/mod.ts";

const digest = (character: string): string => character.repeat(64);
const artifact = (artifact_id: number, character: string, byte_length = 1) => ({
  artifact_id,
  digest: digest(character),
  byte_length,
});

function proposal(): ProductTicketProposalV1 {
  return {
    title: "observable public defect",
    mission_value: "users receive the documented result",
    scope: "public command output",
    contract_owner: "docs/contract.md",
    risk: "compatibility",
    narrative: artifact(1, "a", 20),
    evidence: artifact(2, "b", 20),
    acceptance_criteria: ["the documented output is returned"],
    contract_reads: [{ path: "docs/contract.md", reason: "defines the public behavior" }],
    duplicate_search: { query: "observable public defect", limit: 20 },
    reproducer_profile: "reproducer",
    reproducer: {
      comparison_rule_version: 1,
      command: artifact(3, "c", 20),
      stdin: artifact(13, "d", 20),
      expected_observation: {
        exit_status: 0,
        stdout: artifact(4, "d"),
        stderr: artifact(5, "e", 0),
      },
      first_observation: {
        exit_status: 1,
        stdout: artifact(6, "f"),
        stderr: artifact(7, "0", 0),
      },
      second_observation: {
        exit_status: 1,
        stdout: artifact(8, "f"),
        stderr: artifact(9, "0", 0),
      },
    },
  };
}

Deno.test("XSH Product policy exposes the bounded buffer and repeatable proposal tool", () => {
  assertEquals(xshProductTicketPolicyV1.low_water, 2);
  assertEquals(xshProductTicketPolicyV1.target, 3);
  assertEquals(xshProductTicketPolicyV1.maximum, 5);
  assertEquals(xshProductTicketPolicyV1.proposal_maximum, 3);
  const product = xshApplicationV1.office_profiles.find((profile) =>
    profile.office === "product_research"
  );
  assert(product !== undefined);
  assert(product.tools.includes("product_submit_ticket"));
  assert(product.tools.includes("work_complete"));
  for (
    const tool of [
      "workspace_read",
      "workspace_write",
      "workspace_edit",
      "workspace_search",
      "workspace_list",
      "shell",
      "forum_search",
      "forum_post",
      "artifact_seal",
    ] as const
  ) {
    assert(product.tools.includes(tool));
  }
  assertEquals(PRODUCT_SUBMIT_TICKET_INPUT_SCHEMA_V1.additionalProperties, false);
});

Deno.test("XSH application pins exact repository, reads, and deterministic command profiles", () => {
  assertEquals(
    xshApplicationV1.repository.canonical_local_path,
    "/Users/josh/d/laputa-systems/xsh",
  );
  assertEquals(xshApplicationV1.repository.default_branch, "master");
  for (const profile of xshApplicationV1.office_profiles) {
    assertEquals(profile.system_template.placeholders, ["ASSIGNMENT_ID", "MISSION"]);
    assertEquals(profile.assignment_template.placeholders, ["ASSIGNMENT_ID", "TARGET"]);
  }
  assertEquals(
    xshApplicationV1.required_reads.map((read) => read.path),
    ["AGENTS.md", "docs/CHAPTER-01-why-xsh.md", "docs/TEST-MAP.md"],
  );
  assertEquals(xshApplicationV1.reproducer_profiles, [{
    name: "reproducer",
    executable: { approved_tool: "cargo" },
    argv: ["run", "--quiet", "--locked", "--bin", "xsh", "--", "/dev/stdin"],
    working_directory: ".",
    environment: [],
    timeout_millis: 300_000,
    stdout_byte_limit: 4_194_304,
    stderr_byte_limit: 4_194_304,
    expected_exit_status: 0,
  }]);
  assertEquals(xshApplicationV1.validation_profiles.full[0].argv, ["test", "--locked"]);
});

Deno.test("XSH Product proposal validator binds owner to a stated contract read", () => {
  validateXshProductProposalV1(proposal());
  assertThrows(
    () => validateXshProductProposalV1({ ...proposal(), contract_owner: "docs/other.md" }),
    TypeError,
    "contract_owner",
  );
});

Deno.test("XSH Quality policy exposes the independent validation and sealed review tools", () => {
  const quality = xshApplicationV1.office_profiles.find((profile) => profile.office === "quality");
  assert(quality !== undefined);
  assert(quality.tools.includes("artifact_seal"));
  assert(quality.tools.includes("quality_run_full_suite"));
  assert(quality.tools.includes("quality_submit_review"));
  assertEquals(QUALITY_RUN_FULL_SUITE_INPUT_SCHEMA_V1.additionalProperties, false);
  assertEquals(QUALITY_SUBMIT_REVIEW_INPUT_SCHEMA_V1.additionalProperties, false);
  const review: QualityReviewSubmissionV1 = {
    client_command_id: "quality-1",
    expected_revision: 7,
    full_suite_validation_id: 8,
    verdict: "accept",
    rationale: artifact(10, "a", 20),
    risks: artifact(11, "b", 20),
    additional_probes: artifact(12, "c", 20),
  };
  validateXshQualityReviewV1(review);
  assertThrows(
    () => validateXshQualityReviewV1({ ...review, full_suite_validation_id: 0 }),
    TypeError,
    "validation ID",
  );
  validateXshQualityFullSuiteV1({
    client_command_id: "quality-full-1",
    expected_revision: 7,
    validation_profile: "full",
  });
  assertThrows(
    () =>
      validateXshQualityFullSuiteV1({
        client_command_id: "quality-focused-1",
        expected_revision: 7,
        validation_profile: "focused",
      }),
    TypeError,
    "must be full",
  );
});

Deno.test("XSH Engineering uses the closed candidate validator", () => {
  const candidate: CandidateSubmissionV1 = {
    client_command_id: "engineering-1",
    expected_revision: 4,
    engineering_report: artifact(20, "a", 20),
    commit_subject: "Fix the public behavior",
    commit_body: "",
    regression_test_identity: "tests/integration.rs::public_behavior",
    risks: artifact(21, "b", 20),
  };
  validateXshCandidateSubmissionV1(candidate);
  assertThrows(
    () => validateXshCandidateSubmissionV1({ ...candidate, commit_subject: "bad\nsubject" }),
    TypeError,
    "one line",
  );
});
