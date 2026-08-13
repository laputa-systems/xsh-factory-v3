import {
  decodeJsonFrame,
  encodeJsonFrame,
  type FrameTransport,
  RESPONSE_FRAME_MAX_BYTES,
} from "./protocol.ts";
import {
  decodeForumSearchCursor,
  ForumAdapter,
  type ForumSearchInput,
  validateSearchInput,
} from "./forum.ts";

class FakeForumTransport implements FrameTransport {
  readonly requests: Record<string, unknown>[] = [];

  exchange(frame: Uint8Array): Promise<Uint8Array> {
    const request = decodeJsonFrame<Record<string, unknown>>(
      frame,
      "fake forum transport",
      1 << 20,
    );
    this.requests.push(request);
    return Promise.resolve(encodeJsonFrame(
      {
        protocol_version: 1,
        request_id: request.request_id,
        operation: request.operation,
        items: [],
        next_cursor: "",
      },
      RESPONSE_FRAME_MAX_BYTES,
    ));
  }
}

Deno.test("ForumAdapter sends one bounded search request and preserves cursor", async () => {
  const transport = new FakeForumTransport();
  const adapter = new ForumAdapter(transport, { requestId: () => "request-1" });
  const input: ForumSearchInput = {
    query: '"exact phrase" term',
    limit: 2,
    cursor: { rank_bits: 0x3f80_0000, post_id: 9 },
  };
  const result = await adapter.search(input);
  if (result.items.length !== 0 || result.next_cursor !== null) throw new Error("unexpected page");
  if (transport.requests.length !== 1) throw new Error("search wrote more than one request");
  if (transport.requests[0].operation !== "forum.search") throw new Error("wrong operation");
  if (transport.requests[0].cursor !== "3f800000.9") throw new Error("cursor was not stable");
});

Deno.test("Forum search rejects unbounded or ambiguous input before transport", () => {
  for (
    const input of [
      { query: "", limit: 1 },
      { query: "too many", limit: 21 },
      { query: '"unclosed', limit: 1 },
      { query: "backwards", limit: 1, created_after_micros: 2, created_before_micros: 1 },
    ]
  ) {
    let rejected = false;
    try {
      validateSearchInput(input);
    } catch {
      rejected = true;
    }
    if (!rejected) throw new Error("invalid Forum search input was accepted");
  }
});

Deno.test("Forum cursor rejects NaN rank bits", () => {
  let rejected = false;
  try {
    decodeForumSearchCursor("7fc00000.42");
  } catch {
    rejected = true;
  }
  if (!rejected) throw new Error("NaN cursor rank was accepted");
});

Deno.test("Forum default request IDs are monotonic and connection-local", async () => {
  const transport = new FakeForumTransport();
  const adapter = new ForumAdapter(transport);
  await adapter.search({ query: "one", limit: 1 });
  await adapter.search({ query: "two", limit: 1 });
  if (transport.requests[0].request_id !== "forum-request-1") {
    throw new Error("first request ID is not monotonic");
  }
  if (transport.requests[1].request_id !== "forum-request-2") {
    throw new Error("second request ID is not monotonic");
  }
});
