/**
 * Socket-only operator navigation and campaign control.
 *
 * This adapter closes the programmatic gap between `factoryctl` and the SDK.
 * It accepts a caller-supplied framed transport, so it cannot open PostgreSQL,
 * HTTP, or a Unix socket by itself. Every method remains one closed daemon
 * operation; ticket/candidate/audit navigation has no generic query escape.
 */

import type {
  AuditShowResponse,
  CampaignReceiptResponse,
  CampaignStatusResponse,
  CandidateShowResponse,
  FactorydStatusCall,
  FactorydStatusResponse,
  InstitutionalSearchResponse,
  InstitutionalShowResponse,
  LocalProtocolClient,
  OperatorAuditShowCall,
  OperatorCampaignCancelCall,
  OperatorCampaignStartCall,
  OperatorCampaignStatusCall,
  OperatorCandidateShowCall,
  OperatorInstitutionalSearchCall,
  OperatorInstitutionalShowCall,
  OperatorTicketListCall,
  OperatorTicketShowCall,
  TicketListResponse,
  TicketShowResponse,
} from "./protocol.ts";
import { validateInstitutionalReference, validateInstitutionalSearchInputV1 } from "./protocol.ts";
import { exactObject, validateCommandIdentityV1 } from "./candidate.ts";

export const OPERATOR_LIMITS_V1 = {
  commandIdMaxBytes: 160,
  principalMaxBytes: 240,
} as const;

const TICKET_STATES = [
  "proposed",
  "sponsored",
  "in_flight",
  "delivered",
  "blocked",
  "resolved",
  "superseded",
  "rejected",
] as const;

/** All operator methods require the daemon's authenticated local transport. */
export class OperatorAdapterV1 {
  readonly #client: LocalProtocolClient;

  constructor(client: LocalProtocolClient) {
    this.#client = client;
  }

  async daemonStatus(input: FactorydStatusCall = {}): Promise<FactorydStatusResponse> {
    validateDaemonStatusV1(input);
    return await this.#client.factorydStatus(input);
  }

  async startCampaign(input: OperatorCampaignStartCall): Promise<CampaignReceiptResponse> {
    validateCampaignStartV1(input);
    return await this.#client.operatorCampaignStart(input);
  }

  async campaignStatus(input: OperatorCampaignStatusCall): Promise<CampaignStatusResponse> {
    validateCampaignStatusV1(input);
    return await this.#client.operatorCampaignStatus(input);
  }

  async cancelCampaign(input: OperatorCampaignCancelCall): Promise<CampaignReceiptResponse> {
    validateCampaignCancelV1(input);
    return await this.#client.operatorCampaignCancel(input);
  }

  async listTickets(input: OperatorTicketListCall): Promise<TicketListResponse> {
    validateTicketListV1(input);
    return await this.#client.operatorTicketList(input);
  }

  async showTicket(input: OperatorTicketShowCall): Promise<TicketShowResponse> {
    validateTicketShowV1(input);
    return await this.#client.operatorTicketShow(input);
  }

  async showCandidate(input: OperatorCandidateShowCall): Promise<CandidateShowResponse> {
    validateCandidateShowV1(input);
    return await this.#client.operatorCandidateShow(input);
  }

  async showAudit(input: OperatorAuditShowCall): Promise<AuditShowResponse> {
    validateAuditShowV1(input);
    return await this.#client.operatorAuditShow(input);
  }

  async searchInstitutional(
    input: OperatorInstitutionalSearchCall,
  ): Promise<InstitutionalSearchResponse> {
    validateInstitutionalSearchV1(input);
    return await this.#client.operatorInstitutionalSearch(input);
  }

  async showInstitutional(
    input: OperatorInstitutionalShowCall,
  ): Promise<InstitutionalShowResponse> {
    validateInstitutionalShowV1(input);
    return await this.#client.operatorInstitutionalShow(input);
  }
}

export function validateDaemonStatusV1(input: FactorydStatusCall): void {
  exactObject(input, "daemon status", []);
}

export function validateCampaignStartV1(input: OperatorCampaignStartCall): void {
  exactObject(input, "campaign start", [
    "client_command_id",
    "expected_application_revision",
    "application_revision_id",
    "aggregate_budget_micro_usd",
    "deadline_unix_millis",
    "delivery_target",
    "principal",
  ]);
  commandId(input.client_command_id);
  nonnegative(input.expected_application_revision, "expected application revision");
  positive(input.application_revision_id, "application revision ID");
  positive(input.aggregate_budget_micro_usd, "aggregate budget");
  positive(input.deadline_unix_millis, "deadline");
  positive(input.delivery_target, "delivery target");
  principal(input.principal);
}

export function validateCampaignStatusV1(input: OperatorCampaignStatusCall): void {
  exactObject(input, "campaign status", ["campaign_id"]);
  positive(input.campaign_id, "campaign ID");
}

export function validateCampaignCancelV1(input: OperatorCampaignCancelCall): void {
  exactObject(input, "campaign cancellation", [
    "client_command_id",
    "expected_revision",
    "campaign_id",
    "principal",
  ]);
  validateCommandIdentityV1(input.client_command_id, input.expected_revision);
  positive(input.campaign_id, "campaign ID");
  principal(input.principal);
}

export function validateTicketListV1(input: OperatorTicketListCall): void {
  exactObject(input, "ticket list", ["state"]);
  if (input.state !== null && !TICKET_STATES.includes(input.state)) {
    throw new TypeError("invalid operator ticket list: state is not closed");
  }
}

export function validateTicketShowV1(input: OperatorTicketShowCall): void {
  exactObject(input, "ticket show", ["ticket_id"]);
  positive(input.ticket_id, "ticket ID");
}

export function validateCandidateShowV1(input: OperatorCandidateShowCall): void {
  exactObject(input, "candidate show", ["candidate_id"]);
  positive(input.candidate_id, "candidate ID");
}

export function validateAuditShowV1(input: OperatorAuditShowCall): void {
  exactObject(input, "audit show", ["selector"]);
  if (
    typeof input.selector !== "string" ||
    !/^(ticket|candidate|campaign|application-revision|audit):[1-9][0-9]*$/.test(input.selector)
  ) {
    throw new TypeError("invalid operator audit show: selector must be a closed positive subject");
  }
}

export function validateInstitutionalSearchV1(input: OperatorInstitutionalSearchCall): void {
  validateInstitutionalSearchInputV1(input);
}

export function validateInstitutionalShowV1(input: OperatorInstitutionalShowCall): void {
  exactObject(input, "institutional show", ["reference"]);
  validateInstitutionalReference(input.reference, "institutional show reference");
}

function commandId(value: string): void {
  boundedText(value, "client command ID", OPERATOR_LIMITS_V1.commandIdMaxBytes);
}

function principal(value: string): void {
  boundedText(value, "principal", OPERATOR_LIMITS_V1.principalMaxBytes);
}

function boundedText(value: string, field: string, byteLimit: number): void {
  if (typeof value !== "string" || value.length === 0 || value.includes("\0")) {
    throw new TypeError(`invalid operator operation: ${field} must be nonempty UTF-8 without NUL`);
  }
  if (new TextEncoder().encode(value).byteLength > byteLimit) {
    throw new TypeError(`invalid operator operation: ${field} exceeds ${byteLimit} bytes`);
  }
}

function positive(value: number, field: string): void {
  if (!Number.isSafeInteger(value) || value < 1) {
    throw new TypeError(`invalid operator operation: ${field} is invalid`);
  }
}

function nonnegative(value: number, field: string): void {
  if (!Number.isSafeInteger(value) || value < 0) {
    throw new TypeError(`invalid operator operation: ${field} is invalid`);
  }
}
