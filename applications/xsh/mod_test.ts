import {
  type AssignmentRoleV1,
  compileApplicationV1,
  renderTemplateV1,
  type TemplateDeclarationV1,
} from "@factory/sdk";
import { xshApplicationV1 } from "./mod.ts";

const decoder = new TextDecoder("utf-8", { fatal: true });

// These prompts are injected into ordinary XSH work. They must describe the
// assigned investigation, implementation, or review without exposing the
// surrounding control system as a metaphor the worker has to interpret.
const forbiddenInstitutionalVocabulary =
  /\b(?:architect|campaign|compan(?:y|ies)|control[- ]plane|cto|daemon|department|director|employee|factory|grand\s+architect|institution(?:s|al|ally)?|kernel|manager|office|organization(?:s|al|ally)?|sponsor(?:ed|ship)?|ticket\s+buffer)\b/iu;

const sourceRoot = decodeURIComponent(new URL("./", import.meta.url).pathname);
const reproducerCommandProfile =
  '{"argv":["run","--quiet","--locked","--bin","xsh","--","/dev/stdin"],"environment":[],"executable":{"approved_tool":"cargo"},"expected_exit_status":3,"name":"reproducer","stderr_byte_limit":4194304,"stdout_byte_limit":4194304,"timeout_millis":300000,"working_directory":"."}';

// Keep the application declaration deliberately closed. The Rust admission
// path, not this duplicate TypeScript ledger, verifies each declared BLAKE3
// identity against the exact template bytes it adopts. This test owns the
// independent question: all and only the seven worker-visible inputs exist.
const expectedTemplatePaths = [
  "templates/engineering-assignment.md",
  "templates/engineering-system.md",
  "templates/mission.md",
  "templates/product-assignment.md",
  "templates/product-system.md",
  "templates/quality-assignment.md",
  "templates/quality-system.md",
] as const;

const expectedOfficeTemplates: Readonly<Record<AssignmentRoleV1, readonly [string, string]>> = {
  product_research: ["templates/product-system.md", "templates/product-assignment.md"],
  engineering: ["templates/engineering-system.md", "templates/engineering-assignment.md"],
  quality: ["templates/quality-system.md", "templates/quality-assignment.md"],
};

Deno.test("XSH worker templates are neutral and compile deterministically", async () => {
  const first = await compileApplicationV1(xshApplicationV1, sourceRoot);
  const second = await compileApplicationV1(xshApplicationV1, sourceRoot);

  assertBytesEqual(first.canonical_bytes, second.canonical_bytes, "canonical bundle");
  if (
    first.bundle.predecessor_bundle !==
      "7037b96969423c218ecae6c6e9ac875462c68bb6bd99f6dabae2a439ef29f686"
  ) {
    throw new Error("the current XSH declaration must pin its admitted predecessor bundle");
  }
  if (first.templates.length !== 7 || second.templates.length !== 7) {
    throw new Error("XSH application must compile exactly its seven worker templates");
  }
  assertExactTemplateDeclaration(first);
  for (let index = 0; index < first.templates.length; index += 1) {
    const left = first.templates[index];
    const right = second.templates[index];
    if (left.source_path !== right.source_path) {
      throw new Error("two compiles disagreed about template inventory");
    }
    assertBytesEqual(left.bytes, right.bytes, `${left.source_path} bytes`);
  }

  const mission = templateText(first, "templates/mission.md");
  assertNeutral("templates/mission.md", mission);
  assertRenderedNeutral(
    "templates/mission.md",
    mission,
    xshApplicationV1.mission_template,
    {},
    undefined,
  );

  const productSystem = templateText(first, "templates/product-system.md");
  if (!productSystem.includes(reproducerCommandProfile)) {
    throw new Error("Product must name the canonical command-profile artifact");
  }
  if (xshApplicationV1.reproducer_profiles[0]?.expected_exit_status !== 3) {
    throw new Error("XSH's direct reproducer profile must expect the public runtime failure");
  }
  if (!productSystem.includes("`cargo run --quiet --locked --bin xsh -- /dev/stdin`")) {
    throw new Error("Product must name the command represented by the admitted profile");
  }
  if (!/desired\s+direct XSH exit status `3`/u.test(productSystem)) {
    throw new Error("Product must hand off the direct structured-error expectation");
  }
  if (!productSystem.includes("Investigate only a `par-map` worker index failure")) {
    throw new Error("Product must reproduce the bounded par-map worker failure");
  }
  if (
    !/put only the supplied two-line `par-map` program\s+in the sealed stdin artifact/u.test(
      productSystem,
    )
  ) {
    throw new Error("Product's sealed reproducer must match the bounded par-map failure");
  }
  if (!productSystem.includes("Do not search the host or switch to another")) {
    throw new Error("Product must use its assigned checkout instead of discovering another one");
  }
  if (!/Do\s+not\s+write\s+an\s+outer\s+helper\s+program/u.test(productSystem)) {
    throw new Error("Product must be prohibited from constructing an implementation-style wrapper");
  }
  if (!productSystem.includes("Set `reproducer_profile` to exactly `reproducer`")) {
    throw new Error(
      "Product must name the admitted reproducer profile separately from its command",
    );
  }
  if (!/Each\s+`contract_reads`\s+path must be unique/u.test(productSystem)) {
    throw new Error(
      "Product must require one contract-read entry for each repository path",
    );
  }
  if (!/Set\s+`contract_owner`\s+to exactly `docs\/TEST-MAP\.md`/u.test(productSystem)) {
    throw new Error(
      "Product must name the exact contract-owner path required by the proposal validator",
    );
  }
  if (!productSystem.includes("most 240 UTF-8 bytes")) {
    throw new Error(
      "Product must keep ticket contract-read reasons materializable in an assignment packet",
    );
  }
  if (!productSystem.includes("all ten `artifact_seal` calls together")) {
    throw new Error(
      "Product must be told how to finish the fixed evidence set without serial tool turns",
    );
  }
  if (!productSystem.includes("Do not use Python, create observation JSON")) {
    throw new Error(
      "Product must use the fixed evidence recipe instead of constructing new evidence shapes",
    );
  }
  if (!productSystem.includes("first_observation` and `second_observation` artifact identities")) {
    throw new Error("Product must keep its closed two-run observation references identical");
  }
  const productProfile = xshApplicationV1.assignment_role_profiles.find((profile) =>
    profile.assignment_role === "product_research"
  );
  if (productProfile?.limits.turn_limit !== 12) {
    throw new Error("Product must stay within the bounded discovery allowance");
  }
  const engineeringSystem = templateText(first, "templates/engineering-system.md");
  for (
    const evidenceName of [
      "ticket_proposal",
      "ticket_narrative",
      "ticket_evidence",
      "reproducer_command",
      "reproducer_stdin",
      "reproducer_expected_stdout",
      "reproducer_expected_stderr",
      "reproducer_first_actual_stdout",
      "reproducer_first_actual_stderr",
      "reproducer_second_actual_stdout",
      "reproducer_second_actual_stderr",
    ]
  ) {
    if (!engineeringSystem.includes(`\`${evidenceName}\``)) {
      throw new Error(`Engineering must read handed-off ${evidenceName} evidence`);
    }
  }
  if (!/Do not create or seal\s+implementation-report or risk files/u.test(engineeringSystem)) {
    throw new Error("Engineering must leave completion evidence capture to the controller");
  }
  for (
    const [label, requirement] of [
      ["ten-minute remediation budget", /ten-minute remediation budget/u],
      ["two focused reruns", /no more than two focused\s+reruns/u],
      ["preserved test assertions", /never delete the test or its\s+assertions/u],
      ["Rust named ignore", /#\[ignore =/u],
    ] as const
  ) {
    if (!requirement.test(engineeringSystem)) {
      throw new Error(`Engineering must include bounded flaky-test policy: ${label}`);
    }
  }
  for (
    const [label, requirement] of [
      [
        "typed par-map worker failure",
        /must keep the original `RuntimeError` typed until the\s+coordinating evaluator/u,
      ],
      [
        "coordinator-owned structured error context",
        /`stream_item_runtime_error\("par-map", index,\s+error\)` path exactly once/u,
      ],
      [
        "both par-map execution modes",
        /Cover both the traced\/single-worker path and the\s+ordinary multi-worker path/u,
      ],
      [
        "direct reproducer proof before submission",
        /Do not submit if the exact direct command still exits 0/u,
      ],
      [
        "in-band par-map ResultErr values",
        /`LoweredValue::ResultErr` is an XSH language value: it is in-band output/u,
      ],
      [
        "exact ResultErr preservation",
        /Preserve that value unchanged as\s+`Ok\(LoweredValue::ResultErr\(value\)\)`/u,
      ],
      [
        "collect-all regression gate",
        /`tests\/xsh\/par-map-result\.xsh::test_par_map_collect_all` is the canonical guard/u,
      ],
      [
        "serial collector must not raw-propagate",
        /do not write\s+`results\.push\(result\?\)`/u,
      ],
      [
        "serial collector exact wrapper",
        /Err\(error\) => return Err\(self\.stream_item_runtime_error\("par-map", item_index, error\)\)/u,
      ],
      [
        "native runtime gate before submission",
        /`cargo test --locked --test integration runtime::coverage::xsh_native_tests -- --exact` passes/u,
      ],
    ] as const
  ) {
    if (!requirement.test(engineeringSystem)) {
      throw new Error(`Engineering must include par-map propagation guardrail: ${label}`);
    }
  }
  const engineeringProfile = xshApplicationV1.assignment_role_profiles.find((profile) =>
    profile.assignment_role === "engineering"
  );
  if (engineeringProfile?.tools.includes("artifact_seal")) {
    throw new Error("Engineering must not own report or risk artifact sealing");
  }
  if (!/Keep every shell source-inspection response under 8 KiB/u.test(engineeringSystem)) {
    throw new Error("Engineering must bound source-inspection response size");
  }
  if (
    engineeringProfile?.limits.turn_limit !== 18 ||
    engineeringProfile.limits.wall_limit_millis !== 900_000
  ) {
    throw new Error("Engineering must use the bounded implementation budget");
  }
  const qualitySystem = templateText(first, "templates/quality-system.md");
  if (!/convergence gate, not open-ended research/u.test(qualitySystem)) {
    throw new Error("Quality must treat a passed full suite as a convergence gate");
  }
  if (
    !/Do not run network,\s+download,\s+build, or additional test probes after a passing receipt/u
      .test(
        qualitySystem,
      )
  ) {
    throw new Error("Quality must not spend paid time on speculative probes after a passing suite");
  }
  const qualityProfile = xshApplicationV1.assignment_role_profiles.find((profile) =>
    profile.assignment_role === "quality"
  );
  if (
    qualityProfile?.limits.turn_limit !== 12 ||
    qualityProfile.limits.wall_limit_millis !== 600_000
  ) {
    throw new Error("Quality must use the short, bounded convergence budget");
  }

  for (const profile of xshApplicationV1.assignment_role_profiles) {
    for (const artifact of [profile.system_template, profile.assignment_template]) {
      const source = templateText(first, artifact.source_path);
      assertNeutral(artifact.source_path, source);
      if (artifact.source_path.endsWith("-system.md")) {
        assertExactRequiredReadInstructions(artifact.source_path, source);
      }
      assertRenderedNeutral(
        artifact.source_path,
        source,
        artifact,
        templateValues(artifact, mission),
        profile.assignment_role,
      );
    }
  }
});

function assertExactRequiredReadInstructions(label: string, source: string): void {
  for (const path of ["AGENTS.md", "docs/CHAPTER-01-why-xsh.md", "docs/TEST-MAP.md"]) {
    if (!source.includes(`\`${path}\``)) {
      throw new Error(`${label} does not name required workspace_read path ${path}`);
    }
  }
  if (!source.includes("`workspace_read`") || !source.includes("through `bash`")) {
    throw new Error(`${label} does not distinguish exact reads from shell inspection`);
  }
}

function assertExactTemplateDeclaration(
  compiled: Awaited<ReturnType<typeof compileApplicationV1>>,
): void {
  const declared = [
    xshApplicationV1.mission_template,
    ...xshApplicationV1.assignment_role_profiles.flatMap((profile) => [
      profile.system_template,
      profile.assignment_template,
    ]),
  ];
  const expectedPaths = [...expectedTemplatePaths].sort();
  const declaredPaths = declared.map((template) => template.source_path).sort();
  const compiledPaths = compiled.templates.map((template) => template.source_path).sort();
  assertExactStrings(expectedPaths, declaredPaths, "declared template paths");
  assertExactStrings(expectedPaths, compiledPaths, "compiled template paths");

  for (
    const [office, expected] of Object.entries(expectedOfficeTemplates) as Array<
      [AssignmentRoleV1, readonly [string, string]]
    >
  ) {
    const profile = xshApplicationV1.assignment_role_profiles.find((candidate) =>
      candidate.assignment_role === office
    );
    if (profile === undefined) throw new Error(`missing ${office} assignment-role profile`);
    assertExactStrings(
      [...expected],
      [profile.system_template.source_path, profile.assignment_template.source_path],
      `${office} template selection`,
    );
  }
}

function templateText(
  compiled: Awaited<ReturnType<typeof compileApplicationV1>>,
  sourcePath: string,
): string {
  const template = compiled.templates.find((candidate) => candidate.source_path === sourcePath);
  if (template === undefined) throw new Error(`missing compiled template ${sourcePath}`);
  return decoder.decode(template.bytes);
}

function templateValues(
  artifact: TemplateDeclarationV1,
  mission: string,
): Readonly<Record<string, string>> {
  const values: Record<string, string> = {};
  for (const placeholder of artifact.placeholders) {
    values[placeholder] = placeholder === "MISSION"
      ? mission
      : placeholder === "TARGET"
      ? "one exact XSH behavior defect and its evidence"
      : "test-assignment";
  }
  return values;
}

function assertRenderedNeutral(
  sourcePath: string,
  source: string,
  artifact: TemplateDeclarationV1,
  values: Readonly<Record<string, string>>,
  assignment_role: AssignmentRoleV1 | undefined,
): void {
  const rendered = decoder.decode(
    renderTemplateV1(source, artifact, values, assignment_role),
  );
  if (rendered.includes("${")) {
    throw new Error(`${sourcePath} left a placeholder in the worker prompt`);
  }
  assertNeutral(`${sourcePath} rendered`, rendered);
}

function assertNeutral(label: string, value: string): void {
  const match = forbiddenInstitutionalVocabulary.exec(value.replaceAll("_", " "));
  if (match !== null) {
    throw new Error(`${label} exposes institutional vocabulary ${JSON.stringify(match[0])}`);
  }
}

function assertBytesEqual(left: Uint8Array, right: Uint8Array, label: string): void {
  if (left.length !== right.length || left.some((byte, index) => byte !== right[index])) {
    throw new Error(`${label} changed across deterministic compilation`);
  }
}

function assertExactStrings(
  expected: readonly string[],
  actual: readonly string[],
  label: string,
): void {
  if (
    expected.length !== actual.length || expected.some((value, index) => value !== actual[index])
  ) {
    throw new Error(`${label} differ: expected ${expected.join(", ")}, got ${actual.join(", ")}`);
  }
}
