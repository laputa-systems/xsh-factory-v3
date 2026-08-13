import { assertEquals, assertThrows } from "@std/assert";
import { absolutePath, parseArguments, sameDatabaseTarget } from "./backup_restore_check.ts";

Deno.test("backup restore argument parser accepts only unique flag/value pairs", () => {
  assertEquals(
    parseArguments(["--source-runtime-root", "/source", "--restore-runtime-root", "/restore"]),
    { "source-runtime-root": "/source", "restore-runtime-root": "/restore" },
  );
  assertThrows(() =>
    parseArguments(["--source-runtime-root", "/source", "--source-runtime-root", "/other"])
  );
  assertThrows(() => parseArguments(["--source-runtime-root"]));
  assertThrows(() => parseArguments(["source-runtime-root", "/source"]));
});

Deno.test("backup restore path guard requires normalized absolute distinct roots", () => {
  assertEquals(absolutePath("/restore///"), "/restore");
  assertThrows(() => absolutePath("relative/restore"));
});

Deno.test("backup restore target guard compares PostgreSQL host port and name", () => {
  assertEquals(
    sameDatabaseTarget(
      "postgresql://josh@localhost/factory_restore_v3_1",
      "postgres://other@LOCALHOST:5432/factory_restore_v3_1",
    ),
    true,
  );
  assertEquals(
    sameDatabaseTarget(
      "postgresql://josh@%2Ftmp/factory_restore_v3_1",
      "postgresql://josh@%2Ftmp/factory_restore_v3_2",
    ),
    false,
  );
  assertEquals(
    sameDatabaseTarget(
      "postgresql://josh@localhost:5433/factory_restore_v3_1",
      "postgresql://josh@localhost/factory_restore_v3_1",
    ),
    false,
  );
});
