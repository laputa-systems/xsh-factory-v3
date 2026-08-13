/**
 * Pi custom-tool descriptors for the Forum.
 *
 * The host only adapts a tool call to the typed SDK adapter.  It does not
 * expose SQL, author identity, lifecycle commands, or an unbounded arbitrary
 * operation.  The bound SDK adapter remains the authority boundary.
 */

import type {
  ForumAdapter,
  ForumCreateThreadInput,
  ForumCreateTopicInput,
  ForumListInput,
  ForumPostInput,
  ForumSearchInput,
  ForumThreadPageInput,
} from "../factory-sdk/forum.ts";

export type ForumToolName =
  | "forum_search"
  | "forum_list_topics"
  | "forum_list_threads"
  | "forum_read_thread"
  | "forum_create_topic"
  | "forum_create_thread"
  | "forum_post";

export interface ForumToolDefinition {
  readonly name: ForumToolName;
  readonly description: string;
  /** JSON-schema-shaped data kept closed and provider-neutral. */
  readonly input_schema: Readonly<Record<string, unknown>>;
  readonly invoke: (input: unknown) => Promise<unknown>;
}

/** Actor-visible search intentionally omits the durable author-office filter. */
type ActorForumSearchInput = Omit<ForumSearchInput, "author_office">;

/** Returns one bounded, typed custom-tool definition for each Forum method. */
export function createForumTools(adapter: ForumAdapter): readonly ForumToolDefinition[] {
  return [
    {
      name: "forum_search",
      description: "Search assigned discussion history with bounded filters and continuation.",
      input_schema: {
        type: "object",
        required: ["query", "limit"],
        additionalProperties: false,
        properties: {
          query: { type: "string", maxLength: 4096 },
          topic_id: { type: ["integer", "null"], minimum: 1 },
          thread_id: { type: ["integer", "null"], minimum: 1 },
          post_kind: {
            enum: [
              "Note",
              "Question",
              "Finding",
              "Proposal",
              "Challenge",
              "Correction",
              "DecisionLink",
              null,
            ],
          },
          created_after_micros: { type: ["integer", "null"], minimum: 0 },
          created_before_micros: { type: ["integer", "null"], minimum: 0 },
          limit: { type: "integer", minimum: 1, maximum: 20 },
          cursor: { type: ["object", "null"] },
        },
      },
      invoke: async (input) => {
        const { author_office: _discarded, ...actorInput } = input as ForumSearchInput;
        return stripAuthorOffice(await adapter.search(actorInput as ActorForumSearchInput));
      },
    },
    {
      name: "forum_list_topics",
      description: "List assigned discussion topics by recent activity.",
      input_schema: listSchema(),
      invoke: async (input) => stripAuthorOffice(await adapter.listTopics(input as ForumListInput)),
    },
    {
      name: "forum_list_threads",
      description: "List threads in an assigned topic by recent activity.",
      input_schema: {
        type: "object",
        required: ["topic_id", "limit"],
        additionalProperties: false,
        properties: {
          topic_id: { type: "integer", minimum: 1 },
          ...listSchema().properties as object,
        },
      },
      invoke: async (input) => {
        const value = input as { topic_id: number } & ForumListInput;
        return stripAuthorOffice(await adapter.listThreads(value.topic_id, value));
      },
    },
    {
      name: "forum_read_thread",
      description: "Read a bounded chronological page of immutable discussion posts.",
      input_schema: {
        type: "object",
        required: ["thread_id", "limit"],
        additionalProperties: false,
        properties: {
          thread_id: { type: "integer", minimum: 1 },
          after_post_id: { type: ["integer", "null"], minimum: 1 },
          limit: { type: "integer", minimum: 1, maximum: 20 },
        },
      },
      invoke: async (input) =>
        stripAuthorOffice(await adapter.readThread(input as ForumThreadPageInput)),
    },
    {
      name: "forum_create_topic",
      description: "Create one persistent discussion topic.",
      input_schema: {
        type: "object",
        required: ["client_command_id", "expected_revision", "name", "description"],
        additionalProperties: false,
        properties: {
          client_command_id: { type: "string", maxLength: 160 },
          expected_revision: { type: "integer", minimum: 0 },
          name: { type: "string", maxLength: 160 },
          description: { type: "string", maxLength: 4096 },
        },
      },
      invoke: async (input) => {
        await adapter.createTopic(input as ForumCreateTopicInput);
        return { accepted: true };
      },
    },
    {
      name: "forum_create_thread",
      description: "Create one persistent discussion thread beneath an existing topic.",
      input_schema: {
        type: "object",
        required: ["client_command_id", "expected_revision", "topic_id", "title"],
        additionalProperties: false,
        properties: {
          client_command_id: { type: "string", maxLength: 160 },
          expected_revision: { type: "integer", minimum: 0 },
          topic_id: { type: "integer", minimum: 1 },
          title: { type: "string", maxLength: 240 },
        },
      },
      invoke: async (input) => {
        await adapter.createThread(input as ForumCreateThreadInput);
        return { accepted: true };
      },
    },
    {
      name: "forum_post",
      description: "Append an immutable discussion post, reply, correction, or supersession.",
      input_schema: {
        type: "object",
        required: ["client_command_id", "expected_revision", "thread_id", "kind", "body"],
        additionalProperties: false,
        properties: {
          client_command_id: { type: "string", maxLength: 160 },
          expected_revision: { type: "integer", minimum: 0 },
          thread_id: { type: "integer", minimum: 1 },
          kind: {
            enum: [
              "Note",
              "Question",
              "Finding",
              "Proposal",
              "Challenge",
              "Correction",
              "DecisionLink",
            ],
          },
          body: { type: "string", maxLength: 16384 },
          reply_to: { type: ["integer", "null"], minimum: 1 },
          supersedes: { type: ["integer", "null"], minimum: 1 },
          attachments: { type: "array", maxItems: 8 },
        },
      },
      invoke: async (input) => {
        await adapter.post(input as ForumPostInput);
        return { accepted: true };
      },
    },
  ];
}

/**
 * Durable storage retains author attribution for audit and operator surfaces.
 * This actor adapter deliberately discards that organizational metadata from
 * every Forum result before it can enter a model transcript.
 */
function stripAuthorOffice(value: unknown): unknown {
  if (Array.isArray(value)) return value.map(stripAuthorOffice);
  if (value === null || typeof value !== "object") return value;
  const record = value as Record<string, unknown>;
  return Object.fromEntries(
    Object.entries(record)
      .filter(([key]) => key !== "author_office" && key !== "author_kind")
      .map(([key, item]) => [key, stripAuthorOffice(item)]),
  );
}

function listSchema(): Readonly<Record<string, unknown>> {
  return {
    type: "object",
    required: ["limit"],
    additionalProperties: false,
    properties: {
      after_id: { type: ["integer", "null"], minimum: 1 },
      limit: { type: "integer", minimum: 1, maximum: 20 },
    },
  };
}
