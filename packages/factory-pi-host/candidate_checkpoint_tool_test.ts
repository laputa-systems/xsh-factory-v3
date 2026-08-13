import { assertEquals } from "@std/assert";
import { createFramedToolAdapters, FramedActorClient } from "./framed-actor.ts";
import type { FrameTransport } from "../factory-sdk/protocol.ts";

Deno.test("Engineering checkpoint tool exposes the closed checkpoint input schema", () => {
  const transport: FrameTransport = {
    exchange: () => Promise.reject(new Error("not invoked while inspecting schema")),
  };
  const adapters = createFramedToolAdapters(
    new FramedActorClient(transport),
    ["candidate_checkpoint_regression"],
  );
  assertEquals(adapters.length, 1);
  assertEquals(adapters[0]?.name, "candidate_checkpoint_regression");
  assertEquals(adapters[0]?.sdk_definition.input_schema, {
    type: "object",
    additionalProperties: false,
    required: ["client_command_id", "expected_revision", "regression_command", "expected_failure"],
    properties: {
      client_command_id: { type: "string", minLength: 1, maxLength: 160 },
      expected_revision: { type: "integer", minimum: 0 },
      regression_command: { type: "string", minLength: 1, maxLength: 160 },
      expected_failure: { type: "string", minLength: 1, maxLength: 4096 },
    },
  });
});
