/**
 * Installation-only cache probe.
 *
 * The type-only edge makes Deno resolve and type-check the real FD0 host graph
 * without executing its entrypoint or any dependency top-level code. Runtime
 * preflight executes this inert module with `--cached-only`.
 */
import type { PiHostMainBindings } from "./main.ts";

export type QualifiedPiHostGraph = PiHostMainBindings;
