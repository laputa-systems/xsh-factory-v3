/**
 * Typed Forum adapter for an already-connected local protocol transport.
 *
 * The adapter owns Forum shape and quota checks but never opens a socket,
 * chooses an author, writes PostgreSQL, or treats a post as workflow state.
 * The daemon binds actor/session/office identity to the transport descriptor.
 */

import {
  decodeJsonFrame,
  encodeJsonFrame,
  FrameProtocolError,
  type FrameTransport,
  OPERATION,
  PROTOCOL_VERSION_V1,
  ProtocolCommandError,
  type ProtocolResponseShape,
  REQUEST_FRAME_MAX_BYTES,
  RESPONSE_FRAME_MAX_BYTES,
  validateProtocolResponse,
} from "./protocol.ts";

export const FORUM_QUOTAS = {
  searchQueryMaxBytes: 4 * 1024,
  searchCursorMaxBytes: 512,
  snippetMaxBytes: 1024,
  pageMax: 20,
} as const;

export type ForumPostKind =
  | "Note"
  | "Question"
  | "Finding"
  | "Proposal"
  | "Challenge"
  | "Correction"
  | "DecisionLink";

export type ForumAuthorOffice = "product_research" | "engineering" | "quality";

export interface ForumSearchCursor {
  readonly rank_bits: number;
  readonly post_id: number;
}

export interface ForumSearchInput {
  readonly query: string;
  readonly topic_id?: number | null;
  readonly thread_id?: number | null;
  readonly author_office?: ForumAuthorOffice | null;
  readonly post_kind?: ForumPostKind | null;
  readonly created_after_micros?: number | null;
  readonly created_before_micros?: number | null;
  readonly limit: number;
  readonly cursor?: ForumSearchCursor | null;
}

export interface ForumThreadPageInput {
  readonly thread_id: number;
  readonly after_post_id?: number | null;
  readonly limit: number;
}

export interface ForumListInput {
  readonly after_id?: number | null;
  readonly limit: number;
}

export interface ForumTopic {
  readonly id: number;
  readonly name: string;
  readonly description: string;
  readonly author_office?: ForumAuthorOffice;
  readonly created_at_micros: number;
}

export interface ForumThread {
  readonly id: number;
  readonly topic_id: number;
  readonly title: string;
  readonly author_office?: ForumAuthorOffice;
  readonly created_at_micros: number;
}

export interface ForumAttachment {
  readonly artifact_id: number;
  readonly label: string;
}

export interface ForumPost {
  readonly id: number;
  readonly thread_id: number;
  readonly kind: ForumPostKind;
  readonly body: string;
  readonly reply_to: number | null;
  readonly supersedes: number | null;
  readonly attachments: readonly ForumAttachment[];
  readonly author_office?: ForumAuthorOffice;
  readonly created_at_micros: number;
}

export interface ForumSearchHit {
  readonly topic_id: number;
  readonly thread_id: number;
  readonly post_id: number;
  readonly kind: ForumPostKind;
  readonly author_office?: ForumAuthorOffice;
  readonly rank_bits: number;
  readonly snippet: string;
  readonly topic_name: string;
  readonly thread_title: string;
}

export interface ForumPage<T> {
  readonly items: readonly T[];
  readonly next_cursor: string | null;
}

interface ForumErrorResponse {
  readonly protocol_version: number;
  readonly request_id: string;
  readonly operation: string;
  readonly error_code: string;
  readonly message: string;
  readonly current_revision?: number;
}

interface ForumPageWire {
  readonly items: readonly Record<string, unknown>[];
  readonly next_cursor: string;
}

export interface ForumAdapterOptions {
  readonly requestId?: () => string;
}

/**
 * SDK methods for legacy Forum navigation. Reads only send one bounded request
 * and never manufacture a write receipt. New discussion writes belong to the
 * anchored institutional publication API, not this compatibility adapter.
 */
export class ForumAdapter {
  readonly #transport: FrameTransport;
  readonly #requestId: () => string;

  constructor(transport: FrameTransport, options: ForumAdapterOptions = {}) {
    this.#transport = transport;
    let nextRequestId = 0;
    this.#requestId = options.requestId ?? (() => `forum-request-${++nextRequestId}`);
  }

  async listTopics(input: ForumListInput): Promise<ForumPage<ForumTopic>> {
    validateListInput(input, "forum list topics");
    return await this.#read<ForumTopic>(OPERATION.forumListTopics, {
      cursor: input.after_id == null ? "" : String(positiveInteger(input.after_id, "after_id")),
      limit: input.limit,
    });
  }

  async listThreads(
    topicId: number,
    input: ForumListInput,
  ): Promise<ForumPage<ForumThread>> {
    positiveInteger(topicId, "topic_id");
    validateListInput(input, "forum list threads");
    return await this.#read<ForumThread>(OPERATION.forumListThreads, {
      topic_id: topicId,
      cursor: input.after_id == null ? "" : String(positiveInteger(input.after_id, "after_id")),
      limit: input.limit,
    });
  }

  async readThread(input: ForumThreadPageInput): Promise<ForumPage<ForumPost>> {
    positiveInteger(input.thread_id, "thread_id");
    validatePageLimit(input.limit, "forum read thread");
    if (input.after_post_id != null) positiveInteger(input.after_post_id, "after_post_id");
    return await this.#read<ForumPost>(OPERATION.forumReadThread, {
      thread_id: input.thread_id,
      after_post_id: input.after_post_id ?? 0,
      limit: input.limit,
    });
  }

  async search(input: ForumSearchInput): Promise<ForumPage<ForumSearchHit>> {
    validateSearchInput(input);
    return await this.#read<ForumSearchHit>(OPERATION.forumSearch, {
      query: input.query,
      topic_id: input.topic_id ?? null,
      thread_id: input.thread_id ?? null,
      author_office: input.author_office == null ? null : encodeOffice(input.author_office),
      post_kind: input.post_kind == null ? null : encodePostKind(input.post_kind),
      created_after_micros: input.created_after_micros ?? null,
      created_before_micros: input.created_before_micros ?? null,
      cursor: input.cursor == null ? "" : encodeForumSearchCursor(input.cursor),
      limit: input.limit,
    });
  }

  async #read<Item>(operation: string, payload: unknown): Promise<ForumPage<Item>> {
    const response = await this.#exchange(operation, payload, "page") as ForumPageWire;
    const items = response.items.map((item) => decodeForumItem(operation, item) as Item);
    return { items, next_cursor: response.next_cursor === "" ? null : response.next_cursor };
  }

  async #exchange<T>(
    operation: string,
    payload: T,
    shape: ProtocolResponseShape,
  ): Promise<unknown> {
    const requestId = this.#requestId();
    const request = {
      protocol_version: PROTOCOL_VERSION_V1,
      request_id: requestId,
      operation,
      ...payload as Record<string, unknown>,
    };
    const responseFrame = await this.#transport.exchange(
      encodeJsonFrame(request, REQUEST_FRAME_MAX_BYTES),
    );
    const response = decodeJsonFrame<ForumErrorResponse | ForumPageWire>(
      responseFrame,
      operation,
      RESPONSE_FRAME_MAX_BYTES,
    );
    validateProtocolResponse(response, operation, requestId, shape);
    if ("error_code" in response) {
      throw new ProtocolCommandError(response as ForumErrorResponse & { current_revision: number });
    }
    return response;
  }
}

const POST_KINDS: readonly ForumPostKind[] = [
  "Note",
  "Question",
  "Finding",
  "Proposal",
  "Challenge",
  "Correction",
  "DecisionLink",
];

const OFFICE_TO_WIRE: Readonly<Record<ForumAuthorOffice, number>> = {
  product_research: 0,
  engineering: 1,
  quality: 2,
};
const WIRE_TO_OFFICE: readonly ForumAuthorOffice[] = [
  "product_research",
  "engineering",
  "quality",
];

function encodeOffice(value: ForumAuthorOffice): number {
  return OFFICE_TO_WIRE[value];
}

function encodePostKind(value: ForumPostKind): number {
  const index = POST_KINDS.indexOf(value);
  if (index < 0) throw new TypeError("forum post kind is invalid");
  return index;
}

function wireInteger(item: Record<string, unknown>, field: string, operation: string): number {
  const value = item[field];
  if (typeof value !== "number" || !Number.isSafeInteger(value) || value < 0) {
    throw new FrameProtocolError("invalid_json", `${operation} item requires ${field} integer`);
  }
  return value;
}

function wireString(item: Record<string, unknown>, field: string, operation: string): string {
  const value = item[field];
  if (typeof value !== "string") {
    throw new FrameProtocolError("invalid_json", `${operation} item requires ${field} string`);
  }
  return value;
}

function wireOptionalInteger(
  item: Record<string, unknown>,
  field: string,
  operation: string,
): number | null {
  const value = item[field];
  if (value === null || value === undefined) return null;
  return wireInteger(item, field, operation);
}

function decodeOffice(value: unknown, operation: string): ForumAuthorOffice {
  if (
    typeof value !== "number" || !Number.isInteger(value) || WIRE_TO_OFFICE[value] === undefined
  ) {
    throw new FrameProtocolError(
      "invalid_json",
      `${operation} item has an invalid office discriminant`,
    );
  }
  return WIRE_TO_OFFICE[value];
}

function decodeAuthorOffice(
  item: Record<string, unknown>,
  operation: string,
): ForumAuthorOffice | undefined {
  const kind = wireInteger(item, "author_kind", operation);
  if (kind === 1) return undefined;
  if (kind !== 0) {
    throw new FrameProtocolError("invalid_json", `${operation} item has an invalid author kind`);
  }
  return decodeOffice(item.author_office, operation);
}

function decodePostKind(value: unknown, operation: string): ForumPostKind {
  if (typeof value !== "number" || !Number.isInteger(value) || POST_KINDS[value] === undefined) {
    throw new FrameProtocolError("invalid_json", `${operation} item has an invalid post kind`);
  }
  return POST_KINDS[value];
}

function decodeAttachments(
  value: unknown,
  operation: string,
): readonly ForumAttachment[] {
  if (!Array.isArray(value)) {
    throw new FrameProtocolError("invalid_json", `${operation} item attachments must be an array`);
  }
  return value.map((entry) => {
    if (entry === null || typeof entry !== "object" || Array.isArray(entry)) {
      throw new FrameProtocolError("invalid_json", `${operation} attachment must be an object`);
    }
    const item = entry as Record<string, unknown>;
    return {
      artifact_id: wireInteger(item, "artifact_id", operation),
      label: wireString(item, "label", operation),
    };
  });
}

function decodeForumItem(
  operation: string,
  item: Record<string, unknown>,
): ForumTopic | ForumThread | ForumPost | ForumSearchHit {
  if (operation === OPERATION.forumListTopics) {
    return {
      id: wireInteger(item, "id", operation),
      name: wireString(item, "name", operation),
      description: wireString(item, "description", operation),
      author_office: decodeAuthorOffice(item, operation),
      created_at_micros: wireInteger(item, "created_at_micros", operation),
    };
  }
  if (operation === OPERATION.forumListThreads) {
    return {
      id: wireInteger(item, "id", operation),
      topic_id: wireInteger(item, "topic_id", operation),
      title: wireString(item, "title", operation),
      author_office: decodeAuthorOffice(item, operation),
      created_at_micros: wireInteger(item, "created_at_micros", operation),
    };
  }
  if (operation === OPERATION.forumReadThread) {
    return {
      id: wireInteger(item, "id", operation),
      thread_id: wireInteger(item, "thread_id", operation),
      kind: decodePostKind(item.kind, operation),
      body: wireString(item, "body", operation),
      reply_to: wireOptionalInteger(item, "reply_to", operation),
      supersedes: wireOptionalInteger(item, "supersedes", operation),
      attachments: decodeAttachments(item.attachments, operation),
      author_office: decodeAuthorOffice(item, operation),
      created_at_micros: wireInteger(item, "created_at_micros", operation),
    };
  }
  return {
    topic_id: wireInteger(item, "topic_id", operation),
    thread_id: wireInteger(item, "thread_id", operation),
    post_id: wireInteger(item, "post_id", operation),
    kind: decodePostKind(item.kind, operation),
    author_office: item.author_office == null
      ? undefined
      : decodeOffice(item.author_office, operation),
    rank_bits: wireInteger(item, "rank_bits", operation),
    snippet: wireString(item, "snippet", operation),
    topic_name: wireString(item, "topic_name", operation),
    thread_title: wireString(item, "thread_title", operation),
  };
}

export function encodeForumSearchCursor(cursor: ForumSearchCursor): string {
  positiveInteger(cursor.post_id, "cursor.post_id");
  if (
    !Number.isInteger(cursor.rank_bits) || cursor.rank_bits < 0 || cursor.rank_bits > 0xffff_ffff
  ) {
    throw new TypeError("cursor.rank_bits must be an unsigned 32-bit integer");
  }
  const rankBytes = new DataView(new ArrayBuffer(4));
  rankBytes.setUint32(0, cursor.rank_bits, false);
  const rank = rankBytes.getFloat32(0, false);
  if (!Number.isFinite(rank) || rank < 0) {
    throw new TypeError("cursor rank must be finite and nonnegative");
  }
  return `${cursor.rank_bits.toString(16).padStart(8, "0")}.${cursor.post_id}`;
}

export function decodeForumSearchCursor(value: string): ForumSearchCursor {
  boundedText(value, "search cursor", FORUM_QUOTAS.searchCursorMaxBytes, true);
  const match = /^(?<rank>[0-9a-fA-F]{8})\.(?<post>[1-9][0-9]*)$/.exec(value);
  if (match == null) throw new TypeError("search cursor is not a valid rank.post_id token");
  const cursor = {
    rank_bits: Number.parseInt(match.groups!.rank, 16),
    post_id: positiveInteger(Number(match.groups!.post), "cursor.post_id"),
  };
  encodeForumSearchCursor(cursor);
  return cursor;
}

export function validateSearchInput(input: ForumSearchInput): void {
  boundedText(input.query, "search query", FORUM_QUOTAS.searchQueryMaxBytes, true);
  if ([...input.query].filter((character) => character === '"').length % 2 !== 0) {
    throw new TypeError("search query quoted phrases must be closed");
  }
  validatePageLimit(input.limit, "forum search");
  if (input.cursor != null) encodeForumSearchCursor(input.cursor);
  if (input.topic_id != null) positiveInteger(input.topic_id, "topic_id");
  if (input.thread_id != null) positiveInteger(input.thread_id, "thread_id");
  if (input.created_after_micros != null) {
    nonnegativeInteger(input.created_after_micros, "created_after_micros");
  }
  if (input.created_before_micros != null) {
    nonnegativeInteger(input.created_before_micros, "created_before_micros");
  }
  if (
    input.created_after_micros != null &&
    input.created_before_micros != null &&
    input.created_after_micros > input.created_before_micros
  ) {
    throw new TypeError("search created_after_micros must not be later than created_before_micros");
  }
  if (input.post_kind != null && !POST_KINDS.includes(input.post_kind)) {
    throw new TypeError("search post_kind is invalid");
  }
}

function validateListInput(input: ForumListInput, operation: string): void {
  validatePageLimit(input.limit, operation);
  if (input.after_id != null) positiveInteger(input.after_id, "after_id");
}

function validatePageLimit(value: number, operation: string): void {
  if (!Number.isInteger(value) || value < 1 || value > FORUM_QUOTAS.pageMax) {
    throw new TypeError(`${operation} limit must be between 1 and ${FORUM_QUOTAS.pageMax}`);
  }
}

function positiveInteger(value: number, field: string): number {
  if (!Number.isSafeInteger(value) || value <= 0) throw new TypeError(`${field} must be positive`);
  return value;
}

function nonnegativeInteger(value: number, field: string): number {
  if (!Number.isSafeInteger(value) || value < 0) {
    throw new TypeError(`${field} must be nonnegative`);
  }
  return value;
}

function boundedText(value: string, field: string, maximum: number, nonEmpty: boolean): void {
  if (typeof value !== "string") throw new TypeError(`${field} must be a string`);
  if (nonEmpty && value.length === 0) throw new TypeError(`${field} must not be empty`);
  if (value.includes("\0")) throw new TypeError(`${field} must not contain NUL`);
  const bytes = new TextEncoder().encode(value).byteLength;
  if (bytes > maximum) throw new TypeError(`${field} exceeds its ${maximum}-byte limit`);
}
