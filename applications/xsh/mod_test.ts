import {
  type AssignmentRoleV1,
  compileApplicationV1,
  renderTemplateV1,
  type TemplateDeclarationV1,
} from "@factory/sdk";
import { xshApplicationV1 } from "./mod.ts";

const decoder = new TextDecoder("utf-8", { fatal: true });

// Worker prompts describe XSH work, never the surrounding institution.
const forbiddenInstitutionalVocabulary =
  /\b(?:architect|campaign|compan(?:y|ies)|control[- ]plane|cto|daemon|department|director|employee|factory|grand\s+architect|institution(?:s|al|ally)?|kernel|manager|office|organization(?:s|al|ally)?|sponsor(?:ed|ship)?|ticket\s+buffer)\b/iu;

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

Deno.test("XSH worker templates compile deterministically and expose a bounded product portfolio", async () => {
  const first = await compileApplicationV1(xshApplicationV1, sourceRoot());
  const second = await compileApplicationV1(xshApplicationV1, sourceRoot());

  assertBytesEqual(first.canonical_bytes, second.canonical_bytes, "canonical bundle");
  if (
    first.bundle.predecessor_bundle !==
      "851719fbde9a1a2b10cf469946a75ff14350980fde1efc2e5472637b823dd1ac"
  ) {
    throw new Error("the product portfolio revision must pin the admitted predecessor bundle");
  }
  assertExactTemplateDeclaration(first);

  const mission = templateText(first, "templates/mission.md");
  assertNeutral("templates/mission.md", mission);
  assertRenderedNeutral(
    "templates/mission.md",
    mission,
    xshApplicationV1.mission_template,
    {},
    undefined,
  );

  const profiles = xshApplicationV1.reproducer_profiles;
  assertExactStrings(
    ["sha256_crypt_vector", "sha512_crypt_vector"],
    profiles.map((profile) => profile.name),
    "product opportunity profile names",
  );
  for (const profile of profiles) {
    if (profile.expected_exit_status !== 0) {
      throw new Error(`${profile.name} must model the expected passing vector`);
    }
    if (!profile.argv.includes("--ignored")) {
      throw new Error(`${profile.name} must explicitly exercise its known ignored vector`);
    }
  }

  const productSystem = templateText(first, "templates/product-system.md");
  for (
    const required of [
      "sha256_drepper_vector",
      "sha512_drepper_vector",
      "sha256_crypt_vector",
      "sha512_crypt_vector",
      "Submit each independently failing vector",
      "Do not submit a vector that passes",
    ]
  ) {
    assertContains(productSystem, required, "Product portfolio prompt");
  }
  if (productSystem.includes("par-map worker index failure")) {
    throw new Error("Product must not carry the delivered par-map defect into the next portfolio");
  }
  assertExactRequiredReadInstructions("templates/product-system.md", productSystem);

  const engineeringSystem = templateText(first, "templates/engineering-system.md");
  for (
    const required of [
      "exact assigned behavior-defect contract",
      "Keep every shell source-inspection response under 8 KiB",
      "one focused ticket-relevant native check",
      "Do not run `cargo test --locked --test integration`",
      "Bounded flaky-test remediation",
    ]
  ) {
    assertContains(engineeringSystem, required, "Engineering");
  }
  for (
    const staleInstruction of [
      "Par-map failure propagation",
      "quality-only network download flake",
      "eval_indexed_par_map_item",
    ]
  ) {
    if (engineeringSystem.includes(staleInstruction)) {
      throw new Error(
        `Engineering must derive work from the ticket, not retain ${staleInstruction}`,
      );
    }
  }
  const engineeringProfile = profileFor("engineering");
  if (
    engineeringProfile.limits.turn_limit !== 24 ||
    engineeringProfile.limits.wall_limit_millis !== 900_000
  ) {
    throw new Error("Engineering must retain the bounded implementation budget");
  }
  if (engineeringProfile.tools.includes("artifact_seal")) {
    throw new Error("Engineering must not own completion-evidence sealing");
  }

  const qualitySystem = templateText(first, "templates/quality-system.md");
  for (
    const required of [
      "convergence gate, not open-ended research",
      "Keep every shell source-inspection response under 8 KiB",
      "one targeted `rg -n` lookup and at most one adjacent, line-numbered range",
      "Do not run network, download, build, or additional test probes after a passing receipt",
    ]
  ) {
    assertContains(qualitySystem, required, "Quality");
  }
  const qualityProfile = profileFor("quality");
  if (
    qualityProfile.limits.turn_limit !== 16 || qualityProfile.limits.wall_limit_millis !== 600_000
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

function sourceRoot(): string {
  return decodeURIComponent(new URL("./", import.meta.url).pathname);
}

function profileFor(role: AssignmentRoleV1) {
  const profile = xshApplicationV1.assignment_role_profiles.find((candidate) =>
    candidate.assignment_role === role
  );
  if (profile === undefined) throw new Error(`missing ${role} assignment-role profile`);
  return profile;
}

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
  assertExactStrings(
    expectedPaths,
    declared.map((template) => template.source_path).sort(),
    "declared template paths",
  );
  assertExactStrings(
    expectedPaths,
    compiled.templates.map((template) => template.source_path).sort(),
    "compiled template paths",
  );
  for (
    const [office, expected] of Object.entries(expectedOfficeTemplates) as Array<
      [AssignmentRoleV1, readonly [string, string]]
    >
  ) {
    const profile = profileFor(office);
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
  assignmentRole: AssignmentRoleV1 | undefined,
): void {
  const rendered = decoder.decode(renderTemplateV1(source, artifact, values, assignmentRole));
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

function assertContains(source: string, expected: string, label: string): void {
  const normalized = source.replace(/\s+/gu, " ");
  if (!normalized.includes(expected.replace(/\s+/gu, " "))) {
    throw new Error(`${label} must include ${expected}`);
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
