import {
  compileApplicationV1,
  type OfficeV1,
  renderTemplateV1,
  type TemplateArtifactV1,
} from "@factory/sdk";
import { xshApplicationV1 } from "./mod.ts";

const decoder = new TextDecoder("utf-8", { fatal: true });

// These prompts are injected into ordinary XSH work. They must describe the
// assigned investigation, implementation, or review without exposing the
// surrounding control system as a metaphor the worker has to interpret.
const forbiddenInstitutionalVocabulary =
  /\b(?:architect|campaign|compan(?:y|ies)|control[- ]plane|cto|daemon|department|director|employee|factory|grand\s+architect|institution(?:s|al|ally)?|kernel|manager|office|organization(?:s|al|ally)?|sponsor(?:ed|ship)?|ticket\s+buffer)\b/iu;

const sourceRoot = decodeURIComponent(new URL("./", import.meta.url).pathname);

// Keep the application declaration deliberately closed.  This is a source
// contract test: the Rust admission path verifies the declared BLAKE3 values
// against these exact files when it adopts an application revision.  The Deno
// test verifies that all and only the seven worker-visible inputs are declared
// here, without reimplementing either the renderer or BLAKE3.
const expectedTemplateDigests = {
  "templates/engineering-assignment.md":
    "3160178e4d7c5981d60522f174afa6b43cf275ff863a4b30bc497709a64122b5",
  "templates/engineering-system.md":
    "f4f856900042bc84f6862b8605297064150dddb7054c49a3abf8a26fa99c7071",
  "templates/mission.md": "238e6ad15801eba875197f4a96aed1345efab91df5728b35864d9ab7c2769bbb",
  "templates/product-assignment.md":
    "45af90aff658aaa330ee42e8fc54f7d2507eb2050d663facc1ffb13a1f7a5122",
  "templates/product-system.md": "0e74d9887ab8815ed0dfdc7e25bd962cccbb7ff8e5211a2fc1cbf141fe603083",
  "templates/quality-assignment.md":
    "be05a0a0d56c8dca558d51b955324a52b0f02f5fab0c419dce74cc73f467965e",
  "templates/quality-system.md": "de14293ac66d6496c73649f2fe9feea886e8a31caf81c6a98eb920b10e442a29",
} as const;

const expectedOfficeTemplates: Readonly<Record<OfficeV1, readonly [string, string]>> = {
  product_research: ["templates/product-system.md", "templates/product-assignment.md"],
  engineering: ["templates/engineering-system.md", "templates/engineering-assignment.md"],
  quality: ["templates/quality-system.md", "templates/quality-assignment.md"],
};

Deno.test("XSH worker templates are neutral and compile deterministically", async () => {
  const first = await compileApplicationV1(xshApplicationV1, sourceRoot);
  const second = await compileApplicationV1(xshApplicationV1, sourceRoot);

  assertBytesEqual(first.canonical_bytes, second.canonical_bytes, "canonical bundle");
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

  for (const profile of xshApplicationV1.office_profiles) {
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
        profile.office,
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
    ...xshApplicationV1.office_profiles.flatMap((profile) => [
      profile.system_template,
      profile.assignment_template,
    ]),
  ];
  const expectedPaths = Object.keys(expectedTemplateDigests).sort();
  const declaredPaths = declared.map((template) => template.source_path).sort();
  const compiledPaths = compiled.templates.map((template) => template.source_path).sort();
  assertExactStrings(expectedPaths, declaredPaths, "declared template paths");
  assertExactStrings(expectedPaths, compiledPaths, "compiled template paths");

  for (const template of declared) {
    const expectedDigest = expectedTemplateDigests[
      template.source_path as keyof typeof expectedTemplateDigests
    ];
    if (expectedDigest === undefined || template.digest !== expectedDigest) {
      throw new Error(`unexpected declared digest for ${template.source_path}`);
    }
  }

  for (
    const [office, expected] of Object.entries(expectedOfficeTemplates) as Array<
      [OfficeV1, readonly [string, string]]
    >
  ) {
    const profile = xshApplicationV1.office_profiles.find((candidate) =>
      candidate.office === office
    );
    if (profile === undefined) throw new Error(`missing ${office} office profile`);
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
  artifact: TemplateArtifactV1,
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
  artifact: TemplateArtifactV1,
  values: Readonly<Record<string, string>>,
  office: OfficeV1 | undefined,
): void {
  const rendered = decoder.decode(renderTemplateV1(source, artifact, values, office));
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
