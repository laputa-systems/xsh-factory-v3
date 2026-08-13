import { assert, assertEquals, assertThrows } from "@std/assert";
import type { TicketPolicyV1 } from "./application.ts";
import {
  EXACT_OBSERVATION_COMPARISON_V1,
  type FrameTransport,
  LocalProtocolClient,
  OPERATION,
  PRODUCT_SUBMIT_TICKET_INPUT_SCHEMA_V1,
  type ProductTicketProposalV1,
} from "./mod.ts";
import { encodeJsonFrame } from "./protocol.ts";
import {
  ProductAdapterV1,
  validateDuplicateSearchInputV1,
  validateProductTicketProposalV1,
} from "./product.ts";

const policy: TicketPolicyV1 = {
  low_water: 2,
  target: 3,
  maximum: 5,
  proposal_maximum: 3,
  ticket_bounds: {
    narrative_byte_limit: 64,
    acceptance_criteria_limit: 2,
    contract_read_limit: 2,
  },
};

const digest = (character: string): string => character.repeat(64);
const artifact = (artifact_id: number, character: string, byte_length = 1) => ({
  artifact_id,
  digest: digest(character),
  byte_length,
});

function proposal(): ProductTicketProposalV1 {
  return {
    title: "observable public defect",
    mission_value: "users receive the documented result",
    scope: "public command output",
    contract_owner: "docs/contract.md",
    risk: "compatibility",
    narrative: artifact(1, "a", 64),
    evidence: artifact(2, "b", 16),
    acceptance_criteria: ["the documented output is returned"],
    contract_reads: [{ path: "docs/contract.md", reason: "defines the public behavior" }],
    duplicate_search: { query: "observable public defect", limit: 20 },
    reproducer_profile: "reproducer",
    reproducer: {
      comparison_rule_version: EXACT_OBSERVATION_COMPARISON_V1,
      command: artifact(3, "c", 16),
      stdin: artifact(10, "a", 32),
      expected_observation: {
        exit_status: 0,
        stdout: artifact(4, "d"),
        stderr: artifact(5, "e", 0),
      },
      first_observation: {
        exit_status: 1,
        stdout: artifact(6, "f"),
        stderr: artifact(7, "0", 0),
      },
      second_observation: {
        exit_status: 1,
        stdout: artifact(8, "f"),
        stderr: artifact(9, "0", 0),
      },
    },
  };
}

Deno.test("Product proposal rejects extras and application-bound fields", () => {
  validateProductTicketProposalV1(proposal(), policy);

  const withExtra = { ...proposal(), sponsor: true } as ProductTicketProposalV1;
  assertThrows(
    () => validateProductTicketProposalV1(withExtra, policy),
    TypeError,
    "unknown",
  );

  const oversized = { ...proposal(), narrative: artifact(1, "a", 65) };
  assertThrows(
    () => validateProductTicketProposalV1(oversized, policy),
    TypeError,
    "byte limit",
  );
});

Deno.test("Product proposal requires an exact two-run failing observation", () => {
  const source = proposal();
  const divergent = {
    ...source,
    reproducer: {
      ...source.reproducer,
      second_observation: {
        ...source.reproducer.second_observation,
        exit_status: 2,
      },
    },
  };
  assertThrows(
    () => validateProductTicketProposalV1(divergent, policy),
    TypeError,
    "do not match",
  );

  const expectedSource = proposal();
  const alreadyExpected = {
    ...expectedSource,
    reproducer: {
      ...expectedSource.reproducer,
      first_observation: expectedSource.reproducer.expected_observation,
      second_observation: expectedSource.reproducer.expected_observation,
    },
  };
  assertThrows(
    () => validateProductTicketProposalV1(alreadyExpected, policy),
    TypeError,
    "expected",
  );
});

Deno.test("Product duplicate-search input is closed and bounded", () => {
  validateDuplicateSearchInputV1({ query: "observable public defect", limit: 20 });
  assertThrows(
    () => validateDuplicateSearchInputV1({ query: "observable public defect", limit: 21 }),
    TypeError,
    "between 1 and 20",
  );
  assertThrows(
    () =>
      validateDuplicateSearchInputV1({
        query: "observable public defect",
        limit: 1,
        cursor: "x",
      } as unknown as { readonly query: string; readonly limit: number }),
    TypeError,
    "unknown",
  );
});

Deno.test("Product adapter submits a repeatable proposal without Architect authority", async () => {
  class Transport implements FrameTransport {
    request: Record<string, unknown> | undefined;

    exchange(frame: Uint8Array): Promise<Uint8Array> {
      this.request = JSON.parse(new TextDecoder().decode(frame.slice(4))) as Record<
        string,
        unknown
      >;
      return Promise.resolve(encodeJsonFrame({
        protocol_version: 1,
        request_id: this.request.request_id,
        operation: this.request.operation,
        audit_id: 91,
        aggregate_revision: 8,
      }, 4 * 1024 * 1024));
    }
  }

  const transport = new Transport();
  const adapter = new ProductAdapterV1(new LocalProtocolClient(transport), policy);
  await adapter.submitTicket({
    client_command_id: "product-proposal-1",
    expected_revision: 7,
    proposal: proposal(),
  });
  assertEquals(transport.request?.operation, OPERATION.productSubmitTicket);
  assertEquals(transport.request?.client_command_id, "product-proposal-1");
  assert(!("sponsor" in (transport.request ?? {})));
  await adapter.submitTicket({
    client_command_id: "product-proposal-2",
    expected_revision: 8,
    proposal: proposal(),
  });
  assertEquals(transport.request?.client_command_id, "product-proposal-2");
});

Deno.test("Product Pi custom-tool schema is the closed proposal surface", () => {
  assertEquals(PRODUCT_SUBMIT_TICKET_INPUT_SCHEMA_V1.additionalProperties, false);
  assert(PRODUCT_SUBMIT_TICKET_INPUT_SCHEMA_V1.required.includes("reproducer"));
  assert(PRODUCT_SUBMIT_TICKET_INPUT_SCHEMA_V1.required.includes("duplicate_search"));
  assert(
    !(PRODUCT_SUBMIT_TICKET_INPUT_SCHEMA_V1.required as readonly string[]).includes("sponsor"),
  );
});
