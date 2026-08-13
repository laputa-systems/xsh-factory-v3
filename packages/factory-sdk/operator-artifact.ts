/**
 * Operator-only adoption of one local evidence file into daemon-owned CAS.
 *
 * This adapter is for an already-authenticated local operator transport. It
 * sends an absolute source root plus one canonical relative filename, never
 * inline rationale bytes or a database capability.
 */

import type {
  LocalProtocolClient,
  OperatorArtifactSealCall,
  OperatorArtifactSealReceiptResponse,
} from "./protocol.ts";
import { exactObject } from "./candidate.ts";

export const OPERATOR_ARTIFACT_LIMITS_V1 = {
  principalByteLimit: 160,
} as const;

export class OperatorArtifactAdapterV1 {
  readonly #operatorClient: LocalProtocolClient;

  /** The client must be connected to factoryd's local operator socket. */
  constructor(operatorClient: LocalProtocolClient) {
    this.#operatorClient = operatorClient;
  }

  async seal(input: OperatorArtifactSealCall): Promise<OperatorArtifactSealReceiptResponse> {
    validateOperatorArtifactSealV1(input);
    return await this.#operatorClient.operatorArtifactSeal(input);
  }
}

export function validateOperatorArtifactSealV1(input: OperatorArtifactSealCall): void {
  exactObject(input, "operator artifact seal", [
    "client_command_id",
    "expected_kernel_build_revision",
    "source_root",
    "source_relative_path",
    "principal",
  ]);
  boundedText(input.client_command_id, "client command ID", 160);
  nonnegative(input.expected_kernel_build_revision, "expected kernel build revision");
  absolutePath(input.source_root, "source root");
  safeRelativePath(input.source_relative_path, "source relative path");
  boundedText(input.principal, "principal", OPERATOR_ARTIFACT_LIMITS_V1.principalByteLimit);
}

function boundedText(value: string, field: string, byteLimit: number): void {
  if (typeof value !== "string" || value.length === 0 || value.includes("\0")) {
    fail(`${field} must be nonempty UTF-8 without NUL`);
  }
  if (new TextEncoder().encode(value).byteLength > byteLimit) {
    fail(`${field} exceeds ${byteLimit} bytes`);
  }
}

function absolutePath(value: string, field: string): void {
  if (
    typeof value !== "string" || value.length === 0 || value.includes("\0") ||
    !value.startsWith("/")
  ) {
    fail(`${field} must be a nonempty absolute NUL-free path`);
  }
}

function safeRelativePath(value: string, field: string): void {
  if (
    typeof value !== "string" || value.length === 0 || value.includes("\0") ||
    value.startsWith("/") || value.includes("\\") ||
    value.split("/").some((part) => part === "" || part === "." || part === "..")
  ) {
    fail(`${field} is not a canonical safe relative path`);
  }
}

function nonnegative(value: number, field: string): void {
  if (!Number.isSafeInteger(value) || value < 0) fail(`${field} is invalid`);
}

function fail(message: string): never {
  throw new TypeError(`invalid operator artifact seal: ${message}`);
}
