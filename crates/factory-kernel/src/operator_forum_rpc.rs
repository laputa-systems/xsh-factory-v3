//! Read-only adapter for legacy Forum navigation on the operator socket.
//!
//! New durable discussion facts must be anchored through the institutional
//! publication boundary. This route intentionally retains only bounded reads;
//! the direct Forum store remains available for historical compatibility tests
//! and migration work, not as a public mutation surface.

use factory_protocol::{
    AssignmentRole, ErrorResponse, ForumAttachmentViewV1, ForumAuthor, ForumListThreadsRequestV1,
    ForumListTopicsRequestV1, ForumPageLimit, ForumPostId, ForumPostKind, ForumPostViewV1,
    ForumPostsResponseV1, ForumReadThreadRequestV1, ForumSearchCursor, ForumSearchHitV1,
    ForumSearchInput, ForumSearchQuery, ForumSearchRequestV1, ForumSearchResponseV1, ForumThreadId,
    ForumThreadPage, ForumThreadViewV1, ForumThreadsResponseV1, ForumTopicId, ForumTopicViewV1,
    ForumTopicsResponseV1, PROTOCOL_VERSION_V1, REQUEST_FRAME_MAX_BYTES, decode_operation_request,
    decode_routing_envelope,
};
use miniserde::json;
use thiserror::Error;

use crate::forum_store::{ForumStore, ForumStoreError};

/// Socket-only Forum router. It owns no pool and has no actor binding.
#[derive(Clone, Debug)]
pub(crate) struct OperatorForumRpc {
    store: ForumStore,
}

impl OperatorForumRpc {
    pub(crate) fn from_operator_transport(store: ForumStore) -> Self {
        Self { store }
    }

    pub(crate) async fn dispatch(&self, frame: &[u8]) -> Result<Vec<u8>, OperatorForumRpcError> {
        let envelope = decode_routing_envelope(frame, REQUEST_FRAME_MAX_BYTES)?;
        let request_id = envelope.request_id.clone();
        let operation = envelope.operation.clone();
        let result = self.dispatch_operation(frame).await;
        Ok(match result {
            Ok(response) => response,
            Err(error) => json::to_string(&ErrorResponse {
                protocol_version: PROTOCOL_VERSION_V1,
                request_id,
                operation,
                error_code: forum_error_code(&error).to_owned(),
                message: error.to_string(),
            })
            .into_bytes(),
        })
    }

    async fn dispatch_operation(&self, frame: &[u8]) -> Result<Vec<u8>, OperatorForumRpcError> {
        let operation = decode_routing_envelope(frame, REQUEST_FRAME_MAX_BYTES)?.operation;
        Ok(match operation.as_str() {
            factory_protocol::OP_FORUM_LIST_TOPICS => {
                let request: ForumListTopicsRequestV1 =
                    decode(frame, factory_protocol::OP_FORUM_LIST_TOPICS)?;
                let limit = ForumPageLimit::new(request.limit)?;
                let items = self
                    .store
                    .list_topics(optional_id(&request.cursor, ForumTopicId::new)?, limit)
                    .await?;
                json::to_string(&ForumTopicsResponseV1 {
                    protocol_version: PROTOCOL_VERSION_V1,
                    request_id: request.request_id,
                    operation,
                    next_cursor: page_cursor(
                        items.len(),
                        limit,
                        items.last().map(|item| item.topic_id.get()),
                    ),
                    items: items.into_iter().map(topic_wire).collect(),
                })
                .into_bytes()
            }
            factory_protocol::OP_FORUM_LIST_THREADS => {
                let request: ForumListThreadsRequestV1 =
                    decode(frame, factory_protocol::OP_FORUM_LIST_THREADS)?;
                let limit = ForumPageLimit::new(request.limit)?;
                let topic_id = ForumTopicId::new(request.topic_id)?;
                let items = self
                    .store
                    .list_threads(
                        topic_id,
                        optional_id(&request.cursor, ForumThreadId::new)?,
                        limit,
                    )
                    .await?;
                json::to_string(&ForumThreadsResponseV1 {
                    protocol_version: PROTOCOL_VERSION_V1,
                    request_id: request.request_id,
                    operation,
                    next_cursor: page_cursor(
                        items.len(),
                        limit,
                        items.last().map(|item| item.thread_id.get()),
                    ),
                    items: items.into_iter().map(thread_wire).collect(),
                })
                .into_bytes()
            }
            factory_protocol::OP_FORUM_READ_THREAD => {
                let request: ForumReadThreadRequestV1 =
                    decode(frame, factory_protocol::OP_FORUM_READ_THREAD)?;
                let limit = ForumPageLimit::new(request.limit)?;
                let items = self
                    .store
                    .read_thread(ForumThreadPage::new(
                        ForumThreadId::new(request.thread_id)?,
                        optional_positive_id(request.after_post_id, ForumPostId::new)?,
                        limit,
                    ))
                    .await?;
                json::to_string(&ForumPostsResponseV1 {
                    protocol_version: PROTOCOL_VERSION_V1,
                    request_id: request.request_id,
                    operation,
                    next_cursor: page_cursor(
                        items.len(),
                        limit,
                        items.last().map(|item| item.post_id.get()),
                    ),
                    items: items.into_iter().map(post_wire).collect(),
                })
                .into_bytes()
            }
            factory_protocol::OP_FORUM_SEARCH => {
                let request: ForumSearchRequestV1 =
                    decode(frame, factory_protocol::OP_FORUM_SEARCH)?;
                let limit = ForumPageLimit::new(request.limit)?;
                let input = ForumSearchInput {
                    query: ForumSearchQuery::new(request.query)?,
                    topic_id: request.topic_id.map(ForumTopicId::new).transpose()?,
                    thread_id: request.thread_id.map(ForumThreadId::new).transpose()?,
                    author_office: request.author_office.map(office).transpose()?,
                    post_kind: request.post_kind.map(post_kind).transpose()?,
                    created_after_micros: request.created_after_micros,
                    created_before_micros: request.created_before_micros,
                    limit,
                    cursor: if request.cursor.is_empty() {
                        None
                    } else {
                        Some(ForumSearchCursor::decode(&request.cursor)?)
                    },
                };
                let items = self.store.search(&input).await?;
                let next_cursor = if items.len() == usize::from(limit.get()) {
                    items
                        .last()
                        .map(|item| ForumSearchCursor::new(item.rank_bits, item.post_id).encode())
                        .unwrap_or_default()
                } else {
                    String::new()
                };
                json::to_string(&ForumSearchResponseV1 {
                    protocol_version: PROTOCOL_VERSION_V1,
                    request_id: request.request_id,
                    operation,
                    items: items.into_iter().map(search_wire).collect(),
                    next_cursor,
                })
                .into_bytes()
            }
            _ => return Err(OperatorForumRpcError::OperationNotForum { operation }),
        })
    }
}

#[derive(Debug, Error)]
pub(crate) enum OperatorForumRpcError {
    #[error(transparent)]
    Frame(#[from] factory_protocol::FrameError),
    #[error(transparent)]
    Contract(#[from] factory_protocol::ContractError),
    #[error(transparent)]
    Store(#[from] ForumStoreError),
    #[error("Forum cursor is invalid")]
    InvalidCursor,
    #[error("Forum office is invalid")]
    InvalidOffice,
    #[error("Forum post kind is invalid")]
    InvalidPostKind,
    #[error("operation {operation:?} is not a Forum operation")]
    OperationNotForum { operation: String },
}

fn decode<T: miniserde::Deserialize + miniserde::Serialize>(
    frame: &[u8],
    operation: &'static str,
) -> Result<T, OperatorForumRpcError> {
    Ok(decode_operation_request(
        frame,
        REQUEST_FRAME_MAX_BYTES,
        operation,
    )?)
}
fn page_cursor(length: usize, limit: ForumPageLimit, last: Option<i64>) -> String {
    if length == usize::from(limit.get()) {
        last.map(|id| id.to_string()).unwrap_or_default()
    } else {
        String::new()
    }
}
fn optional_id<T>(
    value: &str,
    constructor: impl FnOnce(i64) -> Result<T, factory_protocol::ContractError>,
) -> Result<Option<T>, OperatorForumRpcError> {
    if value.is_empty() {
        return Ok(None);
    }
    Ok(Some(constructor(
        value
            .parse()
            .map_err(|_| OperatorForumRpcError::InvalidCursor)?,
    )?))
}
fn optional_positive_id<T>(
    value: i64,
    constructor: impl FnOnce(i64) -> Result<T, factory_protocol::ContractError>,
) -> Result<Option<T>, OperatorForumRpcError> {
    if value == 0 {
        Ok(None)
    } else {
        Ok(Some(constructor(value)?))
    }
}
fn office(value: u8) -> Result<AssignmentRole, OperatorForumRpcError> {
    match value {
        0 => Ok(AssignmentRole::ProductResearch),
        1 => Ok(AssignmentRole::Engineering),
        2 => Ok(AssignmentRole::Quality),
        _ => Err(OperatorForumRpcError::InvalidOffice),
    }
}
fn post_kind(value: u8) -> Result<ForumPostKind, OperatorForumRpcError> {
    match value {
        0 => Ok(ForumPostKind::Note),
        1 => Ok(ForumPostKind::Question),
        2 => Ok(ForumPostKind::Finding),
        3 => Ok(ForumPostKind::Proposal),
        4 => Ok(ForumPostKind::Challenge),
        5 => Ok(ForumPostKind::Correction),
        6 => Ok(ForumPostKind::DecisionLink),
        _ => Err(OperatorForumRpcError::InvalidPostKind),
    }
}
const fn office_code(value: AssignmentRole) -> u8 {
    match value {
        AssignmentRole::ProductResearch => 0,
        AssignmentRole::Engineering => 1,
        AssignmentRole::Quality => 2,
    }
}
const fn post_kind_code(value: ForumPostKind) -> u8 {
    match value {
        ForumPostKind::Note => 0,
        ForumPostKind::Question => 1,
        ForumPostKind::Finding => 2,
        ForumPostKind::Proposal => 3,
        ForumPostKind::Challenge => 4,
        ForumPostKind::Correction => 5,
        ForumPostKind::DecisionLink => 6,
    }
}
fn author_wire(author: ForumAuthor) -> (u8, Option<i64>, Option<u8>) {
    match author {
        ForumAuthor::Actor {
            session_id,
            assignment_role,
        } => (
            0,
            Some(session_id.get()),
            Some(office_code(assignment_role)),
        ),
        ForumAuthor::GrandArchitect => (1, None, None),
    }
}
fn topic_wire(item: crate::forum_store::ForumTopicView) -> ForumTopicViewV1 {
    let (author_kind, author_session_id, author_office) = author_wire(item.author);
    ForumTopicViewV1 {
        id: item.topic_id.get(),
        name: item.name,
        description: item.description,
        author_kind,
        author_session_id,
        author_office,
        created_at_micros: item.created_at_micros,
    }
}
fn thread_wire(item: crate::forum_store::ForumThreadView) -> ForumThreadViewV1 {
    let (author_kind, author_session_id, author_office) = author_wire(item.author);
    ForumThreadViewV1 {
        id: item.thread_id.get(),
        topic_id: item.topic_id.get(),
        title: item.title,
        author_kind,
        author_session_id,
        author_office,
        created_at_micros: item.created_at_micros,
    }
}
fn post_wire(item: crate::forum_store::ForumPostView) -> ForumPostViewV1 {
    let (author_kind, author_session_id, author_office) = author_wire(item.author);
    ForumPostViewV1 {
        id: item.post_id.get(),
        thread_id: item.thread_id.get(),
        kind: post_kind_code(item.kind),
        body: item.body,
        author_kind,
        author_session_id,
        author_office,
        reply_to: item.reply_to.map(ForumPostId::get),
        supersedes: item.supersedes.map(ForumPostId::get),
        attachments: item
            .attachments
            .into_iter()
            .map(|attachment| ForumAttachmentViewV1 {
                artifact_id: attachment.artifact_id.get(),
                label: attachment.label,
            })
            .collect(),
        created_at_micros: item.created_at_micros,
    }
}
fn search_wire(item: crate::forum_store::ForumSearchHit) -> ForumSearchHitV1 {
    ForumSearchHitV1 {
        topic_id: item.topic_id.get(),
        thread_id: item.thread_id.get(),
        post_id: item.post_id.get(),
        kind: post_kind_code(item.kind),
        author_office: item.author_office.map(office_code),
        rank_bits: item.rank_bits,
        snippet: item.snippet,
        topic_name: item.topic_name,
        thread_title: item.thread_title,
    }
}
fn forum_error_code(error: &OperatorForumRpcError) -> &'static str {
    match error {
        OperatorForumRpcError::Store(ForumStoreError::RevisionConflict { .. }) => {
            "revision_conflict"
        }
        OperatorForumRpcError::Store(ForumStoreError::IdempotencyConflict) => {
            "idempotency_conflict"
        }
        OperatorForumRpcError::Store(_) => "forum_rejected",
        OperatorForumRpcError::Frame(_)
        | OperatorForumRpcError::Contract(_)
        | OperatorForumRpcError::InvalidCursor
        | OperatorForumRpcError::InvalidOffice
        | OperatorForumRpcError::InvalidPostKind => "invalid_forum_request",
        OperatorForumRpcError::OperationNotForum { .. } => "invalid_forum_operation",
    }
}

#[cfg(test)]
mod tests {
    use crate::storage::KernelStore;

    use super::*;
    #[test]
    fn operator_forum_authority_cannot_be_selected_by_an_office_code() {
        assert!(office(3).is_err());
        assert!(post_kind(7).is_err());
    }

    #[test]
    #[ignore = "requires FACTORY_TEST_DATABASE_URL for a disposable PostgreSQL 18 database"]
    fn forum_browse_and_search_create_no_receipt() {
        smol::block_on(async {
            let store = KernelStore::connect(&test_database_url())
                .await
                .expect("connect");
            store.migrate_and_verify().await.expect("migration");
            let router = OperatorForumRpc::from_operator_transport(store.forum_store());
            let before = audit_count(&store).await;
            for frame in [
                frame(&ForumListTopicsRequestV1 {
                    protocol_version: PROTOCOL_VERSION_V1,
                    request_id: "forum-topics".to_owned(),
                    operation: factory_protocol::OP_FORUM_LIST_TOPICS.to_owned(),
                    cursor: String::new(),
                    limit: 20,
                }),
                frame(&ForumSearchRequestV1 {
                    protocol_version: PROTOCOL_VERSION_V1,
                    request_id: "forum-search".to_owned(),
                    operation: factory_protocol::OP_FORUM_SEARCH.to_owned(),
                    query: "one".to_owned(),
                    topic_id: None,
                    thread_id: None,
                    author_office: None,
                    post_kind: None,
                    created_after_micros: None,
                    created_before_micros: None,
                    cursor: String::new(),
                    limit: 20,
                }),
            ] {
                router.dispatch(&frame).await.expect("forum response");
            }
            assert_eq!(audit_count(&store).await, before);
            store.close().await;
        });
    }

    fn frame<T: miniserde::Serialize>(value: &T) -> Vec<u8> {
        factory_protocol::encode_json_frame(value, REQUEST_FRAME_MAX_BYTES).expect("frame")
    }
    async fn audit_count(store: &KernelStore) -> i64 {
        sqlx::query_scalar!("SELECT count(*)::BIGINT AS \"count!\" FROM factory.audit_log")
            .fetch_one(&store.pool_for_authority())
            .await
            .expect("audit count")
    }
    fn test_database_url() -> String {
        let database_url =
            std::env::var("FACTORY_TEST_DATABASE_URL").expect("FACTORY_TEST_DATABASE_URL");
        let name = database_url
            .rsplit('/')
            .next()
            .and_then(|value| value.split('?').next())
            .expect("database name");
        assert!(name.strip_prefix("factory_test_v3_").is_some_and(
            |suffix| !suffix.is_empty() && suffix.bytes().all(|byte| byte.is_ascii_digit())
        ));
        database_url
    }
}
