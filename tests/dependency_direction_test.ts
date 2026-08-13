import { assert, assertThrows } from "@std/assert";
import { fromFileUrl, join } from "@std/path";
import { type ApplicationBundleV1, defineApplicationV1 } from "@factory/sdk";

Deno.test("application source depends only on the public SDK and has no database boundary", () => {
  const applicationRoot = fromFileUrl(new URL("../applications/xsh/", import.meta.url));
  const sources = collectTypeScriptSources(applicationRoot);

  for (const source of sources) {
    const contents = Deno.readTextFileSync(source);
    assert(!/from\s+["'](?!@factory\/sdk["'])/.test(contents), `${source} has a non-SDK import`);
    assert(
      !/(postgres|postgresql|database_url|sqlx|node:pg)/i.test(contents),
      `${source} names a database boundary`,
    );
    assert(
      !/Deno\.(connect|open|writeFile|command)/.test(contents),
      `${source} has a kernel-owned effect`,
    );
  }
});

Deno.test("defineApplicationV1 rejects unrecognized fields instead of carrying metadata", () => {
  const input = fixture();
  assert(Object.isFrozen(defineApplicationV1(input)));

  const withUnknown = {
    ...fixture(),
    unexpected: "not admitted",
  } as unknown as ApplicationBundleV1;
  assertThrows(() => defineApplicationV1(withUnknown), TypeError, "unknown");
});

function collectTypeScriptSources(root: string): readonly string[] {
  const sources: string[] = [];
  for (const entry of Deno.readDirSync(root)) {
    const child = join(root, entry.name);
    if (entry.isDirectory) {
      sources.push(...collectTypeScriptSources(child));
    }
    if (entry.isFile && entry.name.endsWith(".ts")) sources.push(child);
  }
  return sources;
}

function fixture(): ApplicationBundleV1 {
  const digest = "0000000000000000000000000000000000000000000000000000000000000000";
  const template = (source_path: string) => ({
    source_path,
    digest,
    placeholders: ["ASSIGNMENT_ID"],
    rendered_byte_limit: 1,
  });
  const command = (name: string) => ({
    name,
    executable: { approved_tool: "cargo" as const },
    argv: [],
    working_directory: "workspace",
    environment: [],
    timeout_millis: 1,
    stdout_byte_limit: 1,
    stderr_byte_limit: 1,
    expected_exit_status: 0,
  });
  const model = {
    provider: "provider",
    model_id: "model",
    thinking_level: "high" as const,
    capability_flags: [] as const,
    context_token_limit: 1,
    output_token_limit: 1,
    price_input_micro_usd_per_million_tokens: 0,
    price_output_micro_usd_per_million_tokens: 0,
    price_cache_read_micro_usd_per_million_tokens: 0,
    price_cache_write_micro_usd_per_million_tokens: 0,
  };
  const limits = { turn_limit: 1, wall_limit_millis: 1, output_byte_limit: 1 };
  return {
    format_version: 1,
    application_key: "example",
    predecessor_bundle: null,
    repository: {
      repository_key: "product",
      canonical_local_path: "/workspace/product",
      default_branch: "main",
      delivery_mode: "local_fast_forward_only",
    },
    mission_template: template("templates/mission.md"),
    office_profiles: [
      {
        office: "product_research",
        system_template: template("templates/system.md"),
        assignment_template: template("templates/assignment.md"),
        tools: ["workspace_read", "product_submit_ticket"],
        model,
        limits,
      },
      {
        office: "engineering",
        system_template: template("templates/system.md"),
        assignment_template: template("templates/assignment.md"),
        tools: ["workspace_read", "candidate_submit"],
        model,
        limits,
      },
      {
        office: "quality",
        system_template: template("templates/system.md"),
        assignment_template: template("templates/assignment.md"),
        tools: ["workspace_read", "quality_submit_review"],
        model,
        limits,
      },
    ],
    ticket_policy: {
      low_water: 1,
      target: 2,
      maximum: 3,
      proposal_maximum: 1,
      ticket_bounds: {
        narrative_byte_limit: 1,
        acceptance_criteria_limit: 1,
        contract_read_limit: 1,
      },
    },
    required_reads: [{ path: "AGENTS.md", reason: "contract" }],
    reproducer_profiles: [command("reproducer")],
    validation_profiles: { focused: [command("focused")], full: [command("full")] },
    git_policy: {
      forbidden_paths: [".git"],
      delivery_mode: "local_fast_forward_only",
      provenance_trailers_required: true,
    },
    commit_message_policy: { subject_byte_limit: 1, body_byte_limit: 1 },
  };
}
