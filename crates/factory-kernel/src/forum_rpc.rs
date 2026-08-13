//! Actor-socket adapter for the permanent Forum authority.
//!
//! JSON selects a closed Forum operation and supplies its bounded data, but
//! never an author. The inherited [`ActorConnectionBinding`] remains the sole
//! source of session and office attribution. Domain rejections are returned as
//! ordinary typed error responses so a rejected post does not tear down the
//! actor's liveness channel.

use factory_protocol::{
    AggregateRevision, ArtifactId, ForumAttachmentInput, ForumAttachmentLabel,
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
};
use miniserde::json;

use crate::{
    forum_store::{ForumStore, ForumStoreError},
    local_transport::{ActorConnectionBinding, BoundActorFrame, LocalTransportError},
};

/// Dispatches one already-bound Forum frame. Callers must route only the seven
/// `forum.*` operations here; all identity comes from `frame.binding()`.
pub(crate) async fn dispatch_actor_forum(
    store: &ForumStore,
    frame: &BoundActorFrame,
) -> Result<Vec<u8>, LocalTransportError> {
    let request_id = frame.envelope().request_id.clone();
    let operation = frame.envelope().operation.clone();
    let result = dispatch(store, *frame.binding(), frame).await;
    Ok(match result {
        Ok(bytes) => bytes,
        Err(error) => json::to_string(&factory_protocol::ErrorResponse {
            protocol_version: PROTOCOL_VERSION_V1,
            request_id,
            operation,
            error_code: forum_error_code(&error).to_owned(),
            message: error.to_string(),
        })
        .into_bytes(),
    })
}

async fn dispatch(
    store: &ForumStore,
    binding: ActorConnectionBinding,
    frame: &BoundActorFrame,
) -> Result<Vec<u8>, ForumRpcError> {
    Ok(match frame.envelope().operation.as_str() {
        factory_protocol::OP_FORUM_LIST_TOPICS => {
            let request: ForumListTopicsRequestV1 = decode(frame)?;
            let limit = ForumPageLimit::new(request.limit)?;
            let items = store
                .list_topics(
                    parse_optional_id(&request.cursor, ForumTopicId::new)?,
                    limit,
                )
                .await?;
            let next_cursor = page_cursor(
                items.len(),
                limit,
                items.last().map(|item| item.topic_id.get()),
            );
            json::to_string(&ForumTopicsResponseV1 {
                protocol_version: PROTOCOL_VERSION_V1,
                request_id: request.request_id,
                operation: factory_protocol::OP_FORUM_LIST_TOPICS.to_owned(),
                items: items.into_iter().map(topic_wire).collect(),
                next_cursor,
            })
            .into_bytes()
        }
        factory_protocol::OP_FORUM_LIST_THREADS => {
            let request: ForumListThreadsRequestV1 = decode(frame)?;
            let limit = ForumPageLimit::new(request.limit)?;
            let topic_id = ForumTopicId::new(request.topic_id)?;
            let items = store
                .list_threads(
                    topic_id,
                    parse_optional_id(&request.cursor, ForumThreadId::new)?,
                    limit,
                )
                .await?;
            let next_cursor = page_cursor(
                items.len(),
                limit,
                items.last().map(|item| item.thread_id.get()),
            );
            json::to_string(&ForumThreadsResponseV1 {
                protocol_version: PROTOCOL_VERSION_V1,
                request_id: request.request_id,
                operation: factory_protocol::OP_FORUM_LIST_THREADS.to_owned(),
                items: items.into_iter().map(thread_wire).collect(),
                next_cursor,
            })
            .into_bytes()
        }
        factory_protocol::OP_FORUM_READ_THREAD => {
            let request: ForumReadThreadRequestV1 = decode(frame)?;
            let limit = ForumPageLimit::new(request.limit)?;
            let items = store
                .read_thread(ForumThreadPage::new(
                    ForumThreadId::new(request.thread_id)?,
                    optional_positive_id(request.after_post_id, ForumPostId::new)?,
                    limit,
                ))
                .await?;
            let next_cursor = page_cursor(
                items.len(),
                limit,
                items.last().map(|item| item.post_id.get()),
            );
            json::to_string(&ForumPostsResponseV1 {
                protocol_version: PROTOCOL_VERSION_V1,
                request_id: request.request_id,
                operation: factory_protocol::OP_FORUM_READ_THREAD.to_owned(),
                items: items.into_iter().map(post_wire).collect(),
                next_cursor,
            })
            .into_bytes()
        }
        factory_protocol::OP_FORUM_SEARCH => {
            let request: ForumSearchRequestV1 = decode(frame)?;
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
            let items = store.search(&input).await?;
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
                operation: factory_protocol::OP_FORUM_SEARCH.to_owned(),
                items: items.into_iter().map(search_wire).collect(),
                next_cursor,
            })
            .into_bytes()
        }
        factory_protocol::OP_FORUM_CREATE_TOPIC => {
            let request: ForumCreateTopicRequestV1 = decode(frame)?;
            let receipt = store
                .create_topic(
                    binding,
                    &ForumCreateTopicCommand {
                        identity: mutation_identity(
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
            receipt_bytes(
                request.request_id,
                factory_protocol::OP_FORUM_CREATE_TOPIC,
                receipt.audit_log_id.get(),
                receipt.resulting_revision,
            )
        }
        factory_protocol::OP_FORUM_CREATE_THREAD => {
            let request: ForumCreateThreadRequestV1 = decode(frame)?;
            let receipt = store
                .create_thread(
                    binding,
                    &ForumCreateThreadCommand {
                        identity: mutation_identity(
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
            receipt_bytes(
                request.request_id,
                factory_protocol::OP_FORUM_CREATE_THREAD,
                receipt.audit_log_id.get(),
                receipt.resulting_revision,
            )
        }
        factory_protocol::OP_FORUM_POST => {
            let request: ForumPostRequestV1 = decode(frame)?;
            let receipt = store
                .append_post(
                    binding,
                    &ForumPostCommand {
                        identity: mutation_identity(
                            request.client_command_id,
                            request.expected_revision,
                        )?,
                        thread_id: ForumThreadId::new(request.thread_id)?,
                        input: ForumPostInput {
                            kind: post_kind(request.kind)?,
                            body: ForumPostBody::new(request.body)?,
                            reply_to: request.reply_to.map(ForumPostId::new).transpose()?,
                            supersedes: request.supersedes.map(ForumPostId::new).transpose()?,
                            attachments: request
                                .attachments
                                .into_iter()
                                .map(|item| {
                                    Ok(ForumAttachmentInput {
                                        artifact_id: ArtifactId::new(item.artifact_id)?,
                                        label: ForumAttachmentLabel::new(item.label)?,
                                    })
                                })
                                .collect::<Result<Vec<_>, ForumRpcError>>()?,
                        },
                    },
                )
                .await?;
            receipt_bytes(
                request.request_id,
                factory_protocol::OP_FORUM_POST,
                receipt.audit_log_id.get(),
                receipt.resulting_revision,
            )
        }
        _ => return Err(ForumRpcError::WrongOperation),
    })
}

fn decode<T: miniserde::Deserialize>(frame: &BoundActorFrame) -> Result<T, ForumRpcError> {
    factory_protocol::decode_operation_request(
        frame.frame(),
        REQUEST_FRAME_MAX_BYTES,
        forum_operation(frame.envelope().operation.as_str())?,
    )
    .map_err(ForumRpcError::from)
}

fn forum_operation(value: &str) -> Result<&'static str, ForumRpcError> {
    Ok(match value {
        factory_protocol::OP_FORUM_LIST_TOPICS => factory_protocol::OP_FORUM_LIST_TOPICS,
        factory_protocol::OP_FORUM_LIST_THREADS => factory_protocol::OP_FORUM_LIST_THREADS,
        factory_protocol::OP_FORUM_SEARCH => factory_protocol::OP_FORUM_SEARCH,
        factory_protocol::OP_FORUM_READ_THREAD => factory_protocol::OP_FORUM_READ_THREAD,
        factory_protocol::OP_FORUM_CREATE_TOPIC => factory_protocol::OP_FORUM_CREATE_TOPIC,
        factory_protocol::OP_FORUM_CREATE_THREAD => factory_protocol::OP_FORUM_CREATE_THREAD,
        factory_protocol::OP_FORUM_POST => factory_protocol::OP_FORUM_POST,
        _ => return Err(ForumRpcError::WrongOperation),
    })
}

fn mutation_identity(
    command_id: String,
    revision: u64,
) -> Result<ForumMutationIdentity, ForumRpcError> {
    Ok(ForumMutationIdentity::new(
        command_id,
        AggregateRevision::from_persisted(revision),
    )?)
}

fn receipt_bytes(
    request_id: String,
    operation: &str,
    audit_id: i64,
    revision: AggregateRevision,
) -> Vec<u8> {
    json::to_string(&OperationReceiptResponse {
        protocol_version: PROTOCOL_VERSION_V1,
        request_id,
        operation: operation.to_owned(),
        audit_id,
        aggregate_revision: revision.get(),
    })
    .into_bytes()
}

fn page_cursor(length: usize, limit: ForumPageLimit, last: Option<i64>) -> String {
    if length == usize::from(limit.get()) {
        last.map(|value| value.to_string()).unwrap_or_default()
    } else {
        String::new()
    }
}

fn parse_optional_id<T>(
    value: &str,
    constructor: impl FnOnce(i64) -> Result<T, factory_protocol::ContractError>,
) -> Result<Option<T>, ForumRpcError> {
    if value.is_empty() {
        return Ok(None);
    }
    let parsed = value
        .parse::<i64>()
        .map_err(|_| ForumRpcError::InvalidCursor)?;
    Ok(Some(constructor(parsed)?))
}

fn optional_positive_id<T>(
    value: i64,
    constructor: impl FnOnce(i64) -> Result<T, factory_protocol::ContractError>,
) -> Result<Option<T>, ForumRpcError> {
    if value == 0 {
        Ok(None)
    } else {
        Ok(Some(constructor(value)?))
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

fn author_wire(author: ForumAuthor) -> (u8, Option<i64>, Option<u8>) {
    match author {
        ForumAuthor::Actor { session_id, office } => {
            (0, Some(session_id.get()), Some(office_code(office)))
        }
        ForumAuthor::GrandArchitect => (1, None, None),
    }
}

const fn office_code(value: Office) -> u8 {
    match value {
        Office::ProductResearch => 0,
        Office::Engineering => 1,
        Office::Quality => 2,
    }
}

fn office(value: u8) -> Result<Office, ForumRpcError> {
    match value {
        0 => Ok(Office::ProductResearch),
        1 => Ok(Office::Engineering),
        2 => Ok(Office::Quality),
        _ => Err(ForumRpcError::InvalidOffice),
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

fn post_kind(value: u8) -> Result<ForumPostKind, ForumRpcError> {
    match value {
        0 => Ok(ForumPostKind::Note),
        1 => Ok(ForumPostKind::Question),
        2 => Ok(ForumPostKind::Finding),
        3 => Ok(ForumPostKind::Proposal),
        4 => Ok(ForumPostKind::Challenge),
        5 => Ok(ForumPostKind::Correction),
        6 => Ok(ForumPostKind::DecisionLink),
        _ => Err(ForumRpcError::InvalidPostKind),
    }
}

fn forum_error_code(error: &ForumRpcError) -> &'static str {
    match error {
        ForumRpcError::Store(ForumStoreError::RevisionConflict { .. }) => "revision_conflict",
        ForumRpcError::Store(ForumStoreError::IdempotencyConflict) => "idempotency_conflict",
        ForumRpcError::Store(_) => "forum_rejected",
        ForumRpcError::Frame(_) => "invalid_frame",
        ForumRpcError::Contract(_)
        | ForumRpcError::InvalidCursor
        | ForumRpcError::InvalidOffice
        | ForumRpcError::InvalidPostKind
        | ForumRpcError::WrongOperation => "invalid_forum_request",
    }
}

#[derive(Debug, thiserror::Error)]
enum ForumRpcError {
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
    #[error("operation is not a Forum operation")]
    WrongOperation,
}
