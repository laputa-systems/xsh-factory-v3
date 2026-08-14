//! Actor-socket adapter for immutable institutional publications.
//!
//! The only provenance inputs are the anchor and sealed artifacts named by
//! the actor. The authoring office, session, application revision, and
//! principal come from the daemon-bound connection and are proved again by
//! [`PublicationStore`].

use factory_protocol::{
    ArtifactId, ErrorResponse, PROTOCOL_VERSION_V2, PublicationAttachmentWireV2,
    PublicationCreateRequest, PublicationId, PublicationReceiptResponse, REQUEST_FRAME_MAX_BYTES,
};
use miniserde::json;

use crate::{
    local_transport::{BoundActorFrame, LocalTransportError},
    publication_store::{
        CreatePublication, PublicationAttachmentInput, PublicationStore, PublicationStoreError,
    },
};

/// Dispatches the one actor-authorized publication command. Semantic
/// rejections are returned as a normal typed tool response so the actor can
/// correct an anchor or artifact reference without losing its custody-bound
/// transport connection.
pub(crate) async fn dispatch_actor_publication(
    store: &PublicationStore,
    frame: &BoundActorFrame,
) -> Result<Vec<u8>, LocalTransportError> {
    let request_id = frame.envelope().request_id.clone();
    let operation = frame.envelope().operation.clone();
    let result = dispatch(store, frame).await;
    Ok(match result {
        Ok(response) => response,
        Err(error) => json::to_string(&ErrorResponse {
            protocol_version: PROTOCOL_VERSION_V2,
            request_id,
            operation,
            error_code: publication_error_code(&error).to_owned(),
            message: error.to_string(),
        })
        .into_bytes(),
    })
}

async fn dispatch(
    store: &PublicationStore,
    frame: &BoundActorFrame,
) -> Result<Vec<u8>, PublicationRpcError> {
    let request: PublicationCreateRequest = factory_protocol::decode_operation_request(
        frame.frame(),
        REQUEST_FRAME_MAX_BYTES,
        factory_protocol::OP_PUBLICATION_CREATE,
    )?;
    request.validate()?;
    let receipt = store
        .create_from_actor(*frame.binding(), &command_from_request(&request)?)
        .await?;
    Ok(json::to_string(&PublicationReceiptResponse {
        protocol_version: PROTOCOL_VERSION_V2,
        request_id: request.request_id,
        operation: factory_protocol::OP_PUBLICATION_CREATE.to_owned(),
        audit_id: receipt.audit_log_id.get(),
        aggregate_revision: receipt.resulting_revision.get(),
        publication_id: receipt.publication_id.get(),
        was_idempotent_retry: receipt.was_idempotent_retry,
    })
    .into_bytes())
}

pub(crate) fn command_from_request(
    request: &PublicationCreateRequest,
) -> Result<CreatePublication, factory_protocol::ContractError> {
    Ok(CreatePublication {
        client_command_id: request.client_command_id.clone(),
        anchor: request.anchor_reference()?,
        kind: request.publication_kind()?,
        summary: request.summary.clone(),
        body_artifact_id: ArtifactId::new(request.body_artifact_id)?,
        attachments: request
            .attachments
            .iter()
            .map(attachment)
            .collect::<Result<_, _>>()?,
        reply_to: request.reply_to.map(PublicationId::new).transpose()?,
        supersedes: request.supersedes.map(PublicationId::new).transpose()?,
    })
}

fn attachment(
    value: &PublicationAttachmentWireV2,
) -> Result<PublicationAttachmentInput, factory_protocol::ContractError> {
    Ok(PublicationAttachmentInput {
        artifact_id: ArtifactId::new(value.artifact_id)?,
        label: value.label.clone(),
    })
}

#[derive(Debug, thiserror::Error)]
enum PublicationRpcError {
    #[error(transparent)]
    Frame(#[from] factory_protocol::FrameError),
    #[error(transparent)]
    Contract(#[from] factory_protocol::ContractError),
    #[error(transparent)]
    Store(#[from] PublicationStoreError),
}

fn publication_error_code(error: &PublicationRpcError) -> &'static str {
    match error {
        PublicationRpcError::Store(PublicationStoreError::IdempotencyConflict { .. }) => {
            "idempotency_conflict"
        }
        PublicationRpcError::Frame(_) | PublicationRpcError::Contract(_) => {
            "invalid_publication_request"
        }
        PublicationRpcError::Store(_) => "publication_rejected",
    }
}
