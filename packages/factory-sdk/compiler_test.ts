import { assertThrows } from "@std/assert";
import type { OfficeV1, TemplateArtifactV1 } from "./application.ts";
import { blake3Hex } from "./blake3.ts";
import { ApplicationCompileError, validateTemplateForOfficeV1 } from "./compiler.ts";

function template(placeholder: string): TemplateArtifactV1 {
  return {
    source_path: "template.md",
    digest: "a".repeat(64),
    placeholders: [placeholder],
    rendered_byte_limit: 1024,
  };
}

Deno.test("application compiler BLAKE3 matches the canonical digest vectors", () => {
  if (
    blake3Hex(new Uint8Array()) !==
      "af1349b9f5f9a1a6a0404dea36dcc9499bcb25c9adc112b7cc9a93cae41f3262"
  ) {
    throw new Error("empty BLAKE3 digest changed");
  }
  if (
    blake3Hex(new TextEncoder().encode("abc")) !==
      "6437b3ac38465133ffb63b75273a8db548c558465d79db03fd359c6cd5bd9d85"
  ) {
    throw new Error("short BLAKE3 digest changed");
  }
  const bytes = new Uint8Array(3_000);
  for (let index = 0; index < bytes.length; index += 1) bytes[index] = index % 251;
  if (blake3Hex(bytes) !== "5fade288bf27444bee55ba2babb98c3c922c1e84c2e445e7d1f6da24756f5060") {
    throw new Error("multi-chunk BLAKE3 digest changed");
  }
});

Deno.test("assignment templates structurally hide control-plane identities", () => {
  const rejected: readonly [OfficeV1, string][] = [
    ["product_research", "CAMPAIGN_ID"],
    ["engineering", "APPLICATION_REVISION_ID"],
    ["quality", "OFFICE"],
    ["product_research", "SESSION_ID"],
  ];
  for (const [office, placeholder] of rejected) {
    assertThrows(
      () => validateTemplateForOfficeV1(`\${${placeholder}}`, template(placeholder), office),
      ApplicationCompileError,
      `cannot use placeholder ${placeholder}`,
    );
  }
});

Deno.test("Engineering templates may name their fixed checkpoint contract", () => {
  for (const placeholder of ["REGRESSION_COMMAND", "REGRESSION_EXPECTED_FAILURE"]) {
    validateTemplateForOfficeV1(`\${${placeholder}}`, template(placeholder), "engineering");
  }
});
