function source(path: string): string {
  return Deno.readTextFileSync(new URL(`../${path}`, import.meta.url));
}

const retiredMutationNames = /forum_(?:create_topic|create_thread|post)/u;

Deno.test("public Forum routes and actor adapters expose reads only", () => {
  const publicSources = [
    "crates/factory-kernel/src/forum_rpc.rs",
    "crates/factory-kernel/src/operator_forum_rpc.rs",
    "crates/factory-kernel/src/local_transport.rs",
    "crates/factory-kernel/src/session_runtime.rs",
    "crates/factory-kernel/src/process.rs",
    "packages/factory-sdk/forum.ts",
    "packages/factory-pi-host/forum-tools.ts",
    "packages/factory-pi-host/framed-actor.ts",
    "applications/xsh/mod.ts",
  ];
  for (const path of publicSources) {
    if (retiredMutationNames.test(source(path))) {
      throw new Error(`${path} still exposes an unanchored Forum mutation`);
    }
  }
});

Deno.test("legacy Forum storage remains explicitly available only for compatibility", () => {
  const rpc = source("crates/factory-kernel/src/forum_rpc.rs");
  const operatorRpc = source("crates/factory-kernel/src/operator_forum_rpc.rs");
  if (!rpc.includes("read-only Forum frame")) {
    throw new Error("actor Forum route is not marked legacy");
  }
  if (!operatorRpc.includes("Read-only adapter for legacy Forum navigation")) {
    throw new Error("operator Forum route is not marked legacy");
  }
});
