import { xshApplicationV1 } from "../applications/xsh/mod.ts";
import { compileApplicationBytesV1 } from "../packages/factory-sdk/compiler.ts";

const bytes = compileApplicationBytesV1(xshApplicationV1);
let hex = "";
for (const byte of bytes) hex += byte.toString(16).padStart(2, "0");
console.log(hex);
