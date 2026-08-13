import { assertThrows } from "@std/assert";
import type { OfficeV1, TemplateArtifactV1 } from "./application.ts";
import { ApplicationCompileError, validateTemplateForOfficeV1 } from "./compiler.ts";

function template(placeholder: string): TemplateArtifactV1 {
  return {
    source_path: "template.md",
    digest: "a".repeat(64),
    placeholders: [placeholder],
    rendered_byte_limit: 1024,
  };
}

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
