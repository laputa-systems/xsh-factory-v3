import { defineApplicationV1 } from "./application.ts";
import type {
  ApplicationDefinitionV1,
  ApplicationSourceDefinitionV1,
  OfficeV1,
  TemplateArtifactV1,
  TemplateDeclarationV1,
} from "./application.ts";
import { blake3Hex } from "./blake3.ts";
import { canonicalJson } from "./protocol.ts";

/** A canonical bundle and its separately materialized Markdown inputs. */
export interface CompiledApplicationV1 {
  readonly format_version: 1;
  readonly bundle: ApplicationDefinitionV1;
  readonly canonical_bytes: Uint8Array;
  readonly templates: readonly CompiledTemplateV1[];
}

export interface CompiledTemplateV1 {
  readonly source_path: string;
  readonly bytes: Uint8Array;
  readonly placeholders: readonly string[];
}

export interface CompileApplicationOptionsV1 {
  /** Application package root. Relative template paths resolve beneath it. */
  readonly source_root?: string;
  /** @deprecated template materialization is mandatory for concrete compilation. */
  readonly read_templates?: boolean;
}

export class ApplicationCompileError extends Error {
  constructor(message: string) {
    super(message);
    this.name = "ApplicationCompileError";
  }
}

/**
 * Compiles closed application data to deterministic bytes. This function is
 * intentionally pure: the same frozen declaration can be compiled in two
 * clean Deno processes without reading environment variables or making a
 * provider call. The Rust admission path re-hashes each declared template
 * while adopting it into CAS.
 */
export function canonicalizeApplicationV1(definition: ApplicationDefinitionV1): Uint8Array {
  const bytes = new TextEncoder().encode(canonicalJson(definition));
  // Do not return a view over a caller-owned object: callers use these bytes as
  // the immutable application-revision identity input.
  return bytes.slice();
}

/** Canonicalizes source policy before the compiler binds template bytes. */
export function canonicalizeApplicationSourceV1(
  definition: ApplicationSourceDefinitionV1,
): Uint8Array {
  const bytes = new TextEncoder().encode(canonicalJson(definition));
  return bytes.slice();
}

/** Explicit compatibility name for callers that only need canonical bytes. */
export const compileApplicationBytesV1 = canonicalizeApplicationV1;

/** Concrete compiler entry: template reads and placeholder validation are mandatory. */
export function compileApplicationV1(
  definition: ApplicationSourceDefinitionV1,
  sourceRoot: string,
): Promise<CompiledApplicationV1> {
  return compileApplicationWithTemplatesV1(definition, { source_root: sourceRoot });
}

/**
 * Reads declared templates and checks the closed placeholder language. The
 * returned canonical bundle still contains only paths/digests; Markdown bytes
 * are separate CAS inputs and never become an opaque bundle field.
 */
export async function compileApplicationWithTemplatesV1(
  definition: ApplicationSourceDefinitionV1,
  options: CompileApplicationOptionsV1 = {},
): Promise<CompiledApplicationV1> {
  if (options.source_root === undefined) {
    throw new ApplicationCompileError(
      "source_root is required: concrete application compilation always reads and validates templates",
    );
  }
  const templates: CompiledTemplateV1[] = [];
  const templateDigests = new Map<string, string>();
  const artifacts = templateArtifacts(definition);
  for (const [artifact, office] of artifacts) {
    const path = resolveTemplatePath(options.source_root, artifact.source_path);
    let bytes: Uint8Array;
    try {
      bytes = await Deno.readFile(path);
    } catch (error) {
      throw new ApplicationCompileError(
        `cannot read ${artifact.source_path}: ${
          error instanceof Error ? error.message : String(error)
        }`,
      );
    }
    const source = new TextDecoder("utf-8", { fatal: true }).decode(bytes);
    validateTemplateForOfficeV1(source, artifact, office);
    const digest = blake3Hex(bytes);
    const previousDigest = templateDigests.get(artifact.source_path);
    if (previousDigest !== undefined && previousDigest !== digest) {
      throw new ApplicationCompileError(
        `template path ${artifact.source_path} resolved to different bytes`,
      );
    }
    templateDigests.set(artifact.source_path, digest);
    templates.push({
      source_path: artifact.source_path,
      bytes: bytes.slice(),
      placeholders: [...artifact.placeholders],
    });
  }
  const bundle = materializeApplicationDefinition(definition, templateDigests);
  return {
    format_version: 1,
    bundle,
    canonical_bytes: canonicalizeApplicationV1(bundle),
    templates,
  };
}

/**
 * Validates the deliberately tiny `${PLACEHOLDER}` syntax and its declared
 * set. It rejects unknown, malformed, and declared-but-absent placeholders;
 * no conditional, loop, include, default, or environment syntax is parsed.
 */
export function validateTemplateForOfficeV1(
  source: string,
  artifact: TemplateDeclarationV1,
  office?: OfficeV1 | "mission",
): readonly string[] {
  const declared = new Set(artifact.placeholders);
  const allowed = office === undefined ? undefined : allowedPlaceholders(office);
  const found = new Set<string>();
  let cursor = 0;
  while (cursor < source.length) {
    const start = source.indexOf("${", cursor);
    if (start < 0) break;
    const end = source.indexOf("}", start + 2);
    if (end < 0) throw new ApplicationCompileError("template contains an unterminated placeholder");
    const name = source.slice(start + 2, end);
    if (!/^[A-Z0-9_]{1,64}$/.test(name)) {
      throw new ApplicationCompileError(`template placeholder ${JSON.stringify(name)} is invalid`);
    }
    if (!declared.has(name)) {
      throw new ApplicationCompileError(`template contains undeclared placeholder ${name}`);
    }
    if (allowed !== undefined && !allowed.has(name)) {
      throw new ApplicationCompileError(`${office} template cannot use placeholder ${name}`);
    }
    found.add(name);
    cursor = end + 1;
  }
  for (const name of declared) {
    if (!found.has(name)) {
      throw new ApplicationCompileError(`template declares missing placeholder ${name}`);
    }
  }
  return [...found];
}

/** Performs one substitution pass and enforces the final UTF-8 byte ceiling. */
export function renderTemplateV1(
  source: string,
  artifact: TemplateDeclarationV1,
  values: Readonly<Record<string, string>>,
  office?: OfficeV1,
): Uint8Array {
  validateTemplateForOfficeV1(source, artifact, office);
  const declared = new Set(artifact.placeholders);
  for (const name of Object.keys(values)) {
    if (!declared.has(name)) {
      throw new ApplicationCompileError(`value supplied for unknown ${name}`);
    }
    if (values[name].includes("\0")) {
      throw new ApplicationCompileError(`value ${name} contains NUL`);
    }
  }
  const rendered = source.replace(/\$\{([A-Z0-9_]{1,64})\}/g, (_match, name: string) => {
    if (!(name in values)) throw new ApplicationCompileError(`value missing for ${name}`);
    return values[name];
  });
  const bytes = new TextEncoder().encode(rendered);
  if (bytes.byteLength > artifact.rendered_byte_limit) {
    throw new ApplicationCompileError(
      `rendered template exceeds its ${artifact.rendered_byte_limit}-byte limit`,
    );
  }
  return bytes;
}

function allowedPlaceholders(office: OfficeV1 | "mission"): ReadonlySet<string> {
  if (office === "mission") return new Set();
  const common = [
    "ASSIGNMENT_ID",
    "MISSION",
    "TARGET",
  ];
  const officeSpecific: Record<OfficeV1, readonly string[]> = {
    product_research: [],
    engineering: [
      "TICKET_ID",
      "TICKET_REVISION_ID",
      "REGRESSION_COMMAND",
      "REGRESSION_EXPECTED_FAILURE",
    ],
    quality: ["TICKET_ID", "TICKET_REVISION_ID", "CANDIDATE_ID", "VALIDATION_ID"],
  };
  return new Set([...common, ...officeSpecific[office]]);
}

function templateArtifacts(
  definition: ApplicationSourceDefinitionV1,
): readonly [TemplateDeclarationV1, OfficeV1 | "mission"][] {
  const artifacts: [TemplateDeclarationV1, OfficeV1 | "mission"][] = [
    [definition.mission_template, "mission"],
  ];
  for (const profile of definition.office_profiles) {
    artifacts.push([profile.system_template, profile.office]);
    artifacts.push([profile.assignment_template, profile.office]);
  }
  return artifacts;
}

function materializeApplicationDefinition(
  source: ApplicationSourceDefinitionV1,
  templateDigests: ReadonlyMap<string, string>,
): ApplicationDefinitionV1 {
  return defineApplicationV1({
    ...source,
    mission_template: materializeTemplate(source.mission_template, templateDigests),
    office_profiles: source.office_profiles.map((profile) => ({
      ...profile,
      system_template: materializeTemplate(profile.system_template, templateDigests),
      assignment_template: materializeTemplate(profile.assignment_template, templateDigests),
    })),
  });
}

function materializeTemplate(
  source: TemplateDeclarationV1,
  templateDigests: ReadonlyMap<string, string>,
): TemplateArtifactV1 {
  const digest = templateDigests.get(source.source_path);
  if (digest === undefined) {
    throw new ApplicationCompileError(`template ${source.source_path} was not materialized`);
  }
  return { ...source, digest };
}

function resolveTemplatePath(root: string, relative: string): string {
  if (
    relative.startsWith("/") ||
    relative.split("/").some((part) => part === ".." || part === "." || part === "")
  ) {
    throw new ApplicationCompileError(`unsafe template path ${relative}`);
  }
  return `${root.replace(/\/$/, "")}/${relative}`;
}
