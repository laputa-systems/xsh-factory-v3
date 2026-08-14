import { assert, assertEquals, assertRejects, assertThrows } from "@std/assert";
import { fromFileUrl } from "@std/path";
import { xshApplicationV1 } from "../applications/xsh/mod.ts";
import {
  canonicalizeApplicationSourceV1,
  compileApplicationWithTemplatesV1,
  renderTemplateV1,
  validateTemplateForOfficeV1,
} from "../packages/factory-sdk/compiler.ts";
import {
  canonicalJson,
  decodeFrame,
  decodeJsonFrame,
  encodeFrame,
  encodeJsonFrame,
  FrameProtocolError,
  type FrameTransport,
  isKnownOperation,
  LocalProtocolClient,
  OPERATION,
  RESPONSE_FRAME_MAX_BYTES,
  validateInstitutionalSearchInputV1,
  validateInstitutionalReference,
  validateProtocolResponse,
} from "../packages/factory-sdk/protocol.ts";
import { decodeAssignmentPacketV1 } from "../packages/factory-pi-host/entrypoint.ts";

Deno.test("TypeScript request frame matches the Rust golden payload", async () => {
  const frame = encodeJsonFrame({
    protocol_version: 1,
    request_id: "req-1",
    operation: OPERATION.artifactSealWorkspaceFile,
    client_command_id: "cmd-1",
    expected_revision: 7,
    workspace_relative_path: "reports/result.json",
    byte_limit: 4096,
  });
  const payload = new TextDecoder().decode(decodeFrame(frame));
  const golden = JSON.parse(
    await Deno.readTextFile(
      new URL("./protocol-fixtures/artifact-seal-request.json", import.meta.url),
    ),
  );
  assertEquals(payload, JSON.stringify(golden));
  assertEquals(
    decodeJsonFrame<{ operation: string }>(frame, OPERATION.artifactSealWorkspaceFile).operation,
    OPERATION.artifactSealWorkspaceFile,
  );
});

Deno.test("every closed operation has request, success, conflict, and error goldens", async () => {
  const fixture = JSON.parse(
    await Deno.readTextFile(new URL("./protocol-fixtures/operation-goldens.json", import.meta.url)),
  ) as {
    operations: readonly string[];
    requests: Record<string, Record<string, unknown>>;
    success: Record<string, Record<string, unknown>>;
    conflict: Record<string, Record<string, unknown>>;
    error: Record<string, Record<string, unknown>>;
  };
  assertEquals(fixture.operations.length, Object.values(OPERATION).length);
  for (const operation of fixture.operations) {
    const request = fixture.requests[operation];
    assertEquals(request.operation, operation);
    assertEquals(
      decodeJsonFrame<Record<string, unknown>>(
        encodeJsonFrame(request),
        operation,
      ).operation,
      operation,
    );
    const shape = operation === OPERATION.factorydStatus
      ? "daemon_status"
      : operation === OPERATION.artifactSealWorkspaceFile
      ? "artifact"
      : operation === OPERATION.workspaceRead
      ? "workspace_read"
      : operation === OPERATION.sessionSealArtifact
      ? "artifact"
      : operation === OPERATION.sessionVerifyPacket
      ? "packet_verification"
      : operation === OPERATION.artifactRead
      ? "artifact_read"
      : operation === OPERATION.operatorApplicationShow
      ? "application_show"
      : operation === OPERATION.operatorApplicationRegister ||
          operation === OPERATION.operatorApplicationActivate
      ? "application_revision"
      : operation === OPERATION.operatorArtifactSeal
      ? "operator_artifact"
      : operation === OPERATION.operatorCampaignStart ||
          operation === OPERATION.operatorCampaignCancel
      ? "campaign_receipt"
      : operation === OPERATION.operatorCampaignStatus
      ? "campaign_status"
      : operation === OPERATION.operatorTicketList
      ? "ticket_list"
      : operation === OPERATION.operatorTicketShow
      ? "ticket_show"
      : operation === OPERATION.operatorCandidateShow
      ? "candidate_show"
      : operation === OPERATION.operatorAuditShow
      ? "audit_show"
      : operation === OPERATION.operatorInstitutionalSearch
      ? "institutional_search"
      : operation === OPERATION.operatorInstitutionalShow
      ? "institutional_show"
      : operation.startsWith("forum.") &&
          (operation === OPERATION.forumListTopics ||
            operation === OPERATION.forumListThreads ||
            operation === OPERATION.forumSearch ||
            operation === OPERATION.forumReadThread)
      ? "page"
      : "receipt";
    validateProtocolResponse(
      fixture.success[operation],
      operation,
      request.request_id as string,
      shape,
    );
    validateProtocolResponse(
      fixture.conflict[operation],
      operation,
      request.request_id as string,
      shape,
    );
    validateProtocolResponse(
      fixture.error[operation],
      operation,
      request.request_id as string,
      shape,
    );
    for (
      const response of [
        fixture.success[operation],
        fixture.conflict[operation],
        fixture.error[operation],
      ]
    ) {
      assertEquals(
        decodeJsonFrame<Record<string, unknown>>(
          encodeJsonFrame(response, 4 * 1024 * 1024),
          operation,
          4 * 1024 * 1024,
        ).operation,
        operation,
      );
    }
  }
});

Deno.test("AssignmentPacketV1 golden is canonical and maps numeric wire identities", async () => {
  const text = (await Deno.readTextFile(
    new URL("./protocol-fixtures/assignment-packet-v1.json", import.meta.url),
  )).trimEnd();
  assertEquals(text, canonicalJson(JSON.parse(text)));
  const packet = decodeAssignmentPacketV1(new TextEncoder().encode(text));
  assertEquals(packet.assignment_id, "22");
  assertEquals(packet.application_revision_id, "33");
  assertEquals(packet.aggregate_revision, "7");
  assertEquals(packet.required_reads[0].canonical_path, "AGENTS.md");
  assertEquals(packet.model.price_cache_read_micro_usd_per_million_tokens, 300);
  assertEquals(packet.runtime.credential_source, {
    kind: "environment",
    name: "FACTORY_PROVIDER_KEY",
  });
  assertThrows(
    () => decodeAssignmentPacketV1(new TextEncoder().encode(` ${text}`)),
    Error,
    "assignment packet",
  );

  const initialRevision = JSON.parse(text);
  initialRevision.aggregate_revision = 0;
  assertEquals(
    decodeAssignmentPacketV1(
      new TextEncoder().encode(canonicalJson(initialRevision)),
    ).aggregate_revision,
    "0",
  );
});

Deno.test("TypeScript frame boundary rejects malformed and oversized input", () => {
  assertThrows(() => decodeFrame(new Uint8Array([0, 0, 0])), FrameProtocolError, "length");
  assertThrows(
    () => decodeFrame(new Uint8Array([0, 0, 0, 4, 65])),
    FrameProtocolError,
    "truncated",
  );
  assertThrows(
    () => decodeFrame(new Uint8Array([0, 0, 0, 1, 65, 66])),
    FrameProtocolError,
    "trailing",
  );
  assertThrows(() => encodeFrame(new Uint8Array(9), 8), FrameProtocolError, "exceeding");
  assertThrows(
    () => decodeJsonFrame(new Uint8Array([0, 0, 0, 1, 0xff]), "fixture"),
    FrameProtocolError,
    "UTF-8",
  );
  assert(isKnownOperation(OPERATION.workComplete));
  assert(!isKnownOperation("not.an.operation"));
});

Deno.test("institutional navigation requires one closed kind and a matching cursor", () => {
  const search = {
    query: "typed records",
    kind: "rfc",
    project_id: 7,
    owner_office_id: 3,
    limit: 20,
    cursor: { kind: "rfc", id: 9 },
  };
  validateInstitutionalSearchInputV1(search);
  validateInstitutionalReference(search.cursor);
  assertThrows(
    () => validateInstitutionalSearchInputV1({ ...search, kind: "publication" }),
    TypeError,
    "kind",
  );
  assertThrows(
    () => validateInstitutionalSearchInputV1({ ...search, cursor: { kind: "experiment", id: 9 } }),
    TypeError,
    "cursor",
  );
  assertThrows(
    () => validateInstitutionalSearchInputV1({ ...search, unexpected: true }),
    TypeError,
    "closed",
  );
});

Deno.test("dropped response retry preserves command identity and payload exactly", async () => {
  class DroppedResponseTransport implements FrameTransport {
    readonly requests: Record<string, unknown>[] = [];
    calls = 0;

    exchange(frame: Uint8Array): Promise<Uint8Array> {
      const request = decodeJsonFrame<Record<string, unknown>>(
        frame,
        OPERATION.artifactSealWorkspaceFile,
      );
      this.requests.push(request);
      this.calls += 1;
      if (this.calls === 1) return Promise.reject(new Error("response dropped"));
      return Promise.resolve(encodeJsonFrame({
        protocol_version: 1,
        request_id: request.request_id,
        operation: request.operation,
        artifact_id: 8,
        digest: "0000000000000000000000000000000000000000000000000000000000000000",
        byte_length: 4,
        aggregate_revision: 8,
      }, RESPONSE_FRAME_MAX_BYTES));
    }
  }
  const transport = new DroppedResponseTransport();
  const client = new LocalProtocolClient(transport);
  const call = {
    client_command_id: "stable-command",
    expected_revision: 7,
    workspace_relative_path: "reports/result.json",
    byte_limit: 4096,
  };
  await assertRejects(() => client.artifactSealWorkspaceFile(call), Error, "dropped");
  await client.artifactSealWorkspaceFile(call);
  assertEquals(transport.requests.length, 2);
  const first = { ...transport.requests[0], request_id: undefined };
  const second = { ...transport.requests[1], request_id: undefined };
  assertEquals(first, second);
  assertEquals(transport.requests[0].request_id, "request-1");
  assertEquals(transport.requests[1].request_id, "request-2");
});

Deno.test("XSH application source canonicalization is byte-identical across two runs", () => {
  const first = canonicalizeApplicationSourceV1(xshApplicationV1);
  const second = canonicalizeApplicationSourceV1(xshApplicationV1);
  assertEquals(first, second);
  assert(new TextDecoder().decode(first).startsWith('{"application_key":"xsh"'));
});

Deno.test("XSH application compilation is byte-identical in separate Deno processes", async () => {
  const childPath = fromFileUrl(new URL("./protocol_compile_child.ts", import.meta.url));
  const run = async (): Promise<string> => {
    const command = new Deno.Command(Deno.execPath(), {
      args: ["run", "--no-prompt", "--frozen", "--cached-only", childPath],
      stdout: "piped",
      stderr: "piped",
    });
    const output = await command.output();
    assert(output.success, new TextDecoder().decode(output.stderr));
    return new TextDecoder().decode(output.stdout).trim();
  };
  const [first, second] = await Promise.all([run(), run()]);
  assertEquals(first, second);
});

Deno.test("template compiler reads declared templates and rejects undeclared syntax", async () => {
  const sourceRoot = fromFileUrl(new URL("../applications/xsh/", import.meta.url));
  const compiled = await compileApplicationWithTemplatesV1(xshApplicationV1, {
    source_root: sourceRoot,
    read_templates: true,
  });
  assertEquals(compiled.templates.length, 7);

  const artifact = xshApplicationV1.assignment_role_profiles[1].system_template;
  assertEquals(
    new TextDecoder().decode(renderTemplateV1(
      "assignment ${ASSIGNMENT_ID}; mission ${MISSION}",
      artifact,
      { ASSIGNMENT_ID: "a-1", MISSION: "make the public behavior predictable" },
      "engineering",
    )),
    "assignment a-1; mission make the public behavior predictable",
  );
  assertThrows(
    () => validateTemplateForOfficeV1("${UNKNOWN}", artifact, "engineering"),
    Error,
    "undeclared",
  );
  assertThrows(
    () => validateTemplateForOfficeV1("plain", artifact, "engineering"),
    Error,
    "missing",
  );
  for (
    const [office, placeholder] of [
      ["engineering", "SESSION_ID"],
      ["product_research", "TICKET_ID"],
      ["product_research", "TICKET_REVISION_ID"],
      ["engineering", "CANDIDATE_ID"],
    ] as const
  ) {
    const unavailableArtifact = { ...artifact, placeholders: [placeholder] };
    assertThrows(
      () => validateTemplateForOfficeV1(`\${${placeholder}}`, unavailableArtifact, office),
      Error,
      `cannot use placeholder ${placeholder}`,
    );
  }
});

Deno.test("template compiler never exposes a database or identity field", async () => {
  const sourceRoot = fromFileUrl(new URL("../applications/xsh/", import.meta.url));
  const result = await compileApplicationWithTemplatesV1(xshApplicationV1, {
    source_root: sourceRoot,
  });
  const bundle = JSON.parse(new TextDecoder().decode(result.canonical_bytes)) as Record<
    string,
    unknown
  >;
  assert(!("database_url" in bundle));
  assert(!("session_id" in bundle));
  assert(!("callback" in bundle));
});
