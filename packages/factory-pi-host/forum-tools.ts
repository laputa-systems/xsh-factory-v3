/**
 * Pi custom-tool descriptors for the Forum.
 *
 * The host only adapts a tool call to the typed SDK adapter.  It does not
 * expose SQL, author identity, lifecycle commands, or an unbounded arbitrary
 * operation.  The daemon-bound ForumAdapter remains the authority boundary.
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

/** Returns one bounded, typed custom-tool definition for each Forum method. */
export function createForumTools(adapter: ForumAdapter): readonly ForumToolDefinition[] {
  return [
    {
      name: "forum_search",
      description: "Search permanent Forum history with bounded filters and continuation.",
      input_schema: {
        type: "object",
        required: ["query", "limit"],
        additionalProperties: false,
        properties: {
          query: { type: "string", maxLength: 4096 },
          topic_id: { type: ["integer", "null"], minimum: 1 },
          thread_id: { type: ["integer", "null"], minimum: 1 },
          author_office: { enum: ["product_research", "engineering", "quality", null] },
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
      invoke: (input) => adapter.search(input as ForumSearchInput),
    },
    {
      name: "forum_list_topics",
      description: "List Forum topics by derived recent activity.",
      input_schema: listSchema(),
      invoke: (input) => adapter.listTopics(input as ForumListInput),
    },
    {
      name: "forum_list_threads",
      description: "List threads in a Forum topic by derived recent activity.",
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
        return await adapter.listThreads(value.topic_id, value);
      },
    },
    {
      name: "forum_read_thread",
      description: "Read a bounded chronological page of immutable Forum posts.",
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
      invoke: (input) => adapter.readThread(input as ForumThreadPageInput),
    },
    {
      name: "forum_create_topic",
      description: "Create one permanent Forum topic; this does not create authority.",
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
      invoke: (input) => adapter.createTopic(input as ForumCreateTopicInput),
    },
    {
      name: "forum_create_thread",
      description: "Create one permanent Forum thread beneath an existing topic.",
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
      invoke: (input) => adapter.createThread(input as ForumCreateThreadInput),
    },
    {
      name: "forum_post",
      description: "Append an immutable Forum post, reply, correction, or supersession.",
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
      invoke: (input) => adapter.post(input as ForumPostInput),
    },
  ];
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
