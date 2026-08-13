import { assertEquals } from "@std/assert";
import { createFramedToolAdapters, FramedActorClient } from "./framed-actor.ts";
import { decodeJsonFrame, encodeJsonFrame, type FrameTransport } from "../factory-sdk/protocol.ts";

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
    required: ["regression_command", "expected_failure"],
    properties: {
      regression_command: { type: "string", minLength: 1, maxLength: 160 },
      expected_failure: { type: "string", minLength: 1, maxLength: 4096 },
    },
  });
});

Deno.test("Engineering checkpoint host wrapper mints its revision and idempotency key", async () => {
  let request: Record<string, unknown> | undefined;
  const transport: FrameTransport = {
    exchange: (frame) => {
      request = decodeJsonFrame<Record<string, unknown>>(
        frame,
        "candidate.checkpoint_regression",
      );
      return Promise.resolve(
        encodeJsonFrame({
          protocol_version: 1,
          request_id: request.request_id,
          operation: "candidate.checkpoint_regression",
          regression_tree: "a".repeat(40),
          regression_patch_artifact_id: 1,
          regression_command_set_artifact_id: 2,
          regression_log_artifact_id: 3,
        }),
      );
    },
  };
  const [tool] = createFramedToolAdapters(
    new FramedActorClient(transport),
    ["candidate_checkpoint_regression"],
    { session_revision: 7, next_command_id: () => 41 },
  );
  await tool.sdk_definition.invoke({
    regression_command: "reproducer",
    expected_failure: "ticket-attempt-2-reproducer",
  });
  assertEquals(request, {
    protocol_version: 1,
    request_id: "host-request-1",
    operation: "candidate.checkpoint_regression",
    regression_command: "reproducer",
    expected_failure: "ticket-attempt-2-reproducer",
    client_command_id: "actor-candidate_checkpoint_regression-41",
    expected_revision: 7,
  });
});
