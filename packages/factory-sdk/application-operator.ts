/**
 * Generic application control over an already-authenticated operator transport.
 *
 * This is not actor SDK surface: an actor never receives the local `0600`
 * socket. Registration contains paths, not bytes; Rust/CAS re-reads and seals
 * the bundle and every declared template beneath the supplied source root.
 */

import type {
  ApplicationRevisionReceiptResponse,
  ApplicationShowResponse,
  LocalProtocolClient,
  OperatorApplicationActivateCall,
  OperatorApplicationRegisterCall,
  OperatorApplicationShowCall,
} from "./protocol.ts";
import {
  exactObject,
  validateCommandIdentityV1,
  validateSealedArtifactReferenceV1,
} from "./candidate.ts";

export const APPLICATION_OPERATOR_LIMITS_V1 = {
  principalByteLimit: 160,
  rationaleByteLimit: 128 * 1024,
} as const;

export class ApplicationOperatorAdapterV1 {
  readonly #operatorClient: LocalProtocolClient;

  /** `operatorClient` must be bound to factoryd's operator-only Unix socket. */
  constructor(operatorClient: LocalProtocolClient) {
    this.#operatorClient = operatorClient;
  }

  async show(input: OperatorApplicationShowCall): Promise<ApplicationShowResponse> {
    validateApplicationShowV1(input);
    return await this.#operatorClient.operatorApplicationShow(input);
  }

  async register(
    input: OperatorApplicationRegisterCall,
  ): Promise<ApplicationRevisionReceiptResponse> {
    validateApplicationRegisterV1(input);
    return await this.#operatorClient.operatorApplicationRegister(input);
  }

  async activate(
    input: OperatorApplicationActivateCall,
  ): Promise<ApplicationRevisionReceiptResponse> {
    validateApplicationActivateV1(input);
    return await this.#operatorClient.operatorApplicationActivate(input);
  }
}

export function validateApplicationShowV1(input: OperatorApplicationShowCall): void {
  exactObject(input, "application show", ["application_key", "application_revision_id"]);
  applicationKey(input.application_key);
  nullablePositive(input.application_revision_id, "application revision ID");
}

export function validateApplicationRegisterV1(input: OperatorApplicationRegisterCall): void {
  exactObject(input, "application registration", [
    "client_command_id",
    "expected_revision",
    "expected_kernel_build_revision",
    "kernel_build_id",
    "source_root",
    "bundle_relative_path",
    "principal",
  ]);
  validateCommandIdentityV1(input.client_command_id, input.expected_revision);
  nonnegative(input.expected_kernel_build_revision, "expected kernel build revision");
  if (!/^[0-9a-f]{64}$/.test(input.kernel_build_id)) {
    fail("kernel build ID must be lower-case 32-byte BLAKE3 hex");
  }
  absolutePath(input.source_root, "source root");
  applicationRelativePath(input.bundle_relative_path, "bundle relative path");
  principal(input.principal);
}

export function validateApplicationActivateV1(input: OperatorApplicationActivateCall): void {
  exactObject(input, "application activation", [
    "client_command_id",
    "expected_revision",
    "application_key",
    "application_revision_id",
    "rationale",
    "principal",
  ]);
  validateCommandIdentityV1(input.client_command_id, input.expected_revision);
  applicationKey(input.application_key);
  nullablePositive(input.application_revision_id, "application revision ID");
  validateSealedArtifactReferenceV1(
    input.rationale,
    "application activation rationale",
    APPLICATION_OPERATOR_LIMITS_V1.rationaleByteLimit,
    false,
  );
  principal(input.principal);
}

function applicationKey(value: string): void {
  if (typeof value !== "string" || !/^[a-z0-9-]{1,80}$/.test(value)) {
    fail("application key must use 1 through 80 lower-case letters, digits, or hyphens");
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

function applicationRelativePath(value: string, field: string): void {
  if (
    typeof value !== "string" || value.length === 0 || value.includes("\0") ||
    value.startsWith("/") ||
    value.includes("\\") ||
    value.split("/").some((part) => part === "" || part === "." || part === "..")
  ) {
    fail(`${field} is not canonical application-relative path`);
  }
}

function principal(value: string): void {
  if (typeof value !== "string" || value.length === 0 || value.includes("\0")) {
    fail("principal must be nonempty UTF-8 without NUL");
  }
  if (
    new TextEncoder().encode(value).byteLength > APPLICATION_OPERATOR_LIMITS_V1.principalByteLimit
  ) {
    fail("principal exceeds durable audit byte limit");
  }
}

function nullablePositive(value: number | null, field: string): void {
  if (value !== null && (!Number.isSafeInteger(value) || value < 1)) fail(`${field} is invalid`);
}

function nonnegative(value: number, field: string): void {
  if (!Number.isSafeInteger(value) || value < 0) fail(`${field} is invalid`);
}

function fail(message: string): never {
  throw new TypeError(`invalid application operator request: ${message}`);
}
