import { assert, assertEquals } from "@std/assert";
import { ModelRuntime } from "@factory/pi-headless";
import { xshApplicationV1 } from "../applications/xsh/mod.ts";
import {
  createEphemeralCredentialStore,
  verifyModelDescriptor,
} from "../packages/factory-pi-host/sdk-factory.ts";
import type { PiAssignmentPacket } from "../packages/factory-pi-host/types.ts";

/**
 * This is an offline catalog compatibility judge, not a model invocation. It
 * uses the same local Pi-headless runtime and exact descriptor verifier as the paid host while
 * supplying an empty process-local credential store and disabling catalog
 * network refresh.
 */
Deno.test("XSH office model profiles exactly match Pi's frozen offline catalog", async () => {
  const runtime = await ModelRuntime.create({
    credentials: createEphemeralCredentialStore(),
    modelsPath: null,
    allowModelNetwork: false,
    refreshOnCreate: false,
  });

  for (const office of xshApplicationV1.assignment_role_profiles) {
    const model = runtime.getModel(office.model.provider, office.model.model_id);
    assert(
      model !== undefined,
      `${office.assignment_role} model is absent from Pi's offline catalog`,
    );
    verifyModelDescriptor(
      model,
      { model: office.model } as unknown as PiAssignmentPacket,
    );
  }

  const engineering = xshApplicationV1.assignment_role_profiles.find((office) =>
    office.assignment_role === "engineering"
  );
  assert(engineering !== undefined);
  assertEquals(engineering.model.model_id, "openai/gpt-5.6-luna");
  assertEquals(engineering.model.thinking_level, "xhigh");
});
