//! Grand Architect adapter for the permanent Forum authority.
//!
//! The same closed Forum DTOs serve actor and operator paths, but this route
//! mints `OperatorForumCapability` after authenticating the mode-0600 socket.
//! Thus the persisted author is always `GrandArchitect`; no JSON field can
//! claim a session, office, or principal.

use factory_protocol::{
    AggregateRevision, ArtifactId, ErrorResponse, ForumAttachmentInput, ForumAttachmentLabel,
    ForumAttachmentViewV1, ForumAuthor, ForumCreateThreadCommand, ForumCreateThreadInput,
    ForumCreateThreadRequestV1, ForumCreateTopicCommand, ForumCreateTopicInput,
    ForumCreateTopicRequestV1, ForumListThreadsRequestV1, ForumListTopicsRequestV1,
    ForumMutationIdentity, ForumPageLimit, ForumPostBody, ForumPostCommand, ForumPostId,
    ForumPostInput, ForumPostKind, ForumPostRequestV1, ForumPostViewV1, ForumPostsResponseV1,
    ForumReadThreadRequestV1, ForumSearchCursor, ForumSearchHitV1, ForumSearchInput,
    ForumSearchQuery, ForumSearchRequestV1, ForumSearchResponseV1, ForumThreadId, ForumThreadPage,
    ForumThreadTitle, ForumThreadViewV1, ForumThreadsResponseV1, ForumTopicDescription,
    ForumTopicId, ForumTopicName, ForumTopicViewV1, ForumTopicsResponseV1, Office,
    OperationReceiptResponse, PROTOCOL_VERSION_V1, REQUEST_FRAME_MAX_BYTES,
    decode_operation_request, decode_routing_envelope,
};
use miniserde::json;
use thiserror::Error;

use crate::forum_store::{ForumAuthority, ForumStore, ForumStoreError, OperatorForumCapability};

/// Socket-only Forum router. It owns no pool and has no actor binding.
#[derive(Clone, Debug)]
pub(crate) struct OperatorForumRpc {
    store: ForumStore,
    capability: OperatorForumCapability,
}

impl OperatorForumRpc {
    pub(crate) fn from_operator_transport(store: ForumStore) -> Self {
        Self {
            store,
            capability: ForumStore::operator_capability(),
        }
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
            factory_protocol::OP_FORUM_CREATE_TOPIC => {
                let request: ForumCreateTopicRequestV1 =
                    decode(frame, factory_protocol::OP_FORUM_CREATE_TOPIC)?;
                let accepted = self
                    .store
                    .create_topic_with_authority(
                        ForumAuthority::GrandArchitect(self.capability),
                        &ForumCreateTopicCommand {
                            identity: identity(
                                request.client_command_id,
                                request.expected_revision,
                            )?,
                            input: ForumCreateTopicInput {
                                name: ForumTopicName::new(request.name)?,
                                description: ForumTopicDescription::new(request.description)?,
                            },
                        },
                    )
                    .await?;
                receipt(
                    request.request_id,
                    operation,
                    accepted.audit_log_id.get(),
                    accepted.resulting_revision,
                )
            }
            factory_protocol::OP_FORUM_CREATE_THREAD => {
                let request: ForumCreateThreadRequestV1 =
                    decode(frame, factory_protocol::OP_FORUM_CREATE_THREAD)?;
                let accepted = self
                    .store
                    .create_thread_with_authority(
                        ForumAuthority::GrandArchitect(self.capability),
                        &ForumCreateThreadCommand {
                            identity: identity(
                                request.client_command_id,
                                request.expected_revision,
                            )?,
                            input: ForumCreateThreadInput {
                                topic_id: ForumTopicId::new(request.topic_id)?,
                                title: ForumThreadTitle::new(request.title)?,
                            },
                        },
                    )
                    .await?;
                receipt(
                    request.request_id,
                    operation,
                    accepted.audit_log_id.get(),
                    accepted.resulting_revision,
                )
            }
            factory_protocol::OP_FORUM_POST => {
                let request: ForumPostRequestV1 = decode(frame, factory_protocol::OP_FORUM_POST)?;
                let attachments = request
                    .attachments
                    .into_iter()
                    .map(|item| {
                        Ok(ForumAttachmentInput {
                            artifact_id: ArtifactId::new(item.artifact_id)?,
                            label: ForumAttachmentLabel::new(item.label)?,
                        })
                    })
                    .collect::<Result<Vec<_>, OperatorForumRpcError>>()?;
                let accepted = self
                    .store
                    .append_post_with_authority(
                        ForumAuthority::GrandArchitect(self.capability),
                        &ForumPostCommand {
                            identity: identity(
                                request.client_command_id,
                                request.expected_revision,
                            )?,
                            thread_id: ForumThreadId::new(request.thread_id)?,
                            input: ForumPostInput {
                                kind: post_kind(request.kind)?,
                                body: ForumPostBody::new(request.body)?,
                                reply_to: request.reply_to.map(ForumPostId::new).transpose()?,
                                supersedes: request.supersedes.map(ForumPostId::new).transpose()?,
                                attachments,
                            },
                        },
                    )
                    .await?;
                receipt(
                    request.request_id,
                    operation,
                    accepted.audit_log_id.get(),
                    accepted.resulting_revision,
                )
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

fn decode<T: miniserde::Deserialize>(
    frame: &[u8],
    operation: &'static str,
) -> Result<T, OperatorForumRpcError> {
    Ok(decode_operation_request(
        frame,
        REQUEST_FRAME_MAX_BYTES,
        operation,
    )?)
}
fn identity(
    command_id: String,
    revision: u64,
) -> Result<ForumMutationIdentity, OperatorForumRpcError> {
    Ok(ForumMutationIdentity::new(
        command_id,
        AggregateRevision::from_persisted(revision),
    )?)
}
fn receipt(
    request_id: String,
    operation: String,
    audit_id: i64,
    aggregate_revision: AggregateRevision,
) -> Vec<u8> {
    json::to_string(&OperationReceiptResponse {
        protocol_version: PROTOCOL_VERSION_V1,
        request_id,
        operation,
        audit_id,
        aggregate_revision: aggregate_revision.get(),
    })
    .into_bytes()
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
fn office(value: u8) -> Result<Office, OperatorForumRpcError> {
    match value {
        0 => Ok(Office::ProductResearch),
        1 => Ok(Office::Engineering),
        2 => Ok(Office::Quality),
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
const fn office_code(value: Office) -> u8 {
    match value {
        Office::ProductResearch => 0,
        Office::Engineering => 1,
        Office::Quality => 2,
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
        ForumAuthor::Actor { session_id, office } => {
            (0, Some(session_id.get()), Some(office_code(office)))
        }
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
