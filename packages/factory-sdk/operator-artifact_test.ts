import { assertThrows } from "@std/assert";

import { validateOperatorArtifactSealV1 } from "./operator-artifact.ts";

const valid = {
  client_command_id: "seal-rationale-1",
  expected_kernel_build_revision: 4,
  source_root: "/workspace/operator-evidence",
  source_relative_path: "rationale.md",
  principal: "grand-architect",
};

Deno.test("operator artifact seal accepts one rooted regular-file name", () => {
  validateOperatorArtifactSealV1(valid);
});

Deno.test("operator artifact seal rejects bytes and unsafe source paths", () => {
  assertThrows(
    () => validateOperatorArtifactSealV1({ ...valid, source_relative_path: "../rationale.md" }),
    TypeError,
    "safe relative",
  );
  assertThrows(
    () => validateOperatorArtifactSealV1({ ...valid, source_root: "relative" }),
    TypeError,
  );
  assertThrows(
    () => validateOperatorArtifactSealV1({ ...valid, contents: "rationale" } as typeof valid),
    TypeError,
    "unknown or missing",
  );
});
