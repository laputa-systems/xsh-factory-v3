//! Local-operator adapter for an immutable institutional publication.
//!
//! This is intentionally separate from the actor route: only the daemon's
//! mode-`0600` operator socket can select a durable office without an actor
//! assignment, and it never supplies a session provenance value.

use factory_protocol::{
    ErrorResponse, OperatorPublicationCreateRequest, PROTOCOL_VERSION_V1,
    PublicationReceiptResponse, REQUEST_FRAME_MAX_BYTES, decode_operation_request,
    decode_routing_envelope,
};
use miniserde::json;
use thiserror::Error;

use crate::{
    publication_rpc::command_from_request,
    publication_store::{PublicationStore, PublicationStoreError},
};

/// Capability minted solely while configuring the daemon's local operator
/// listener. Actors and applications have no constructor for this value.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct OperatorPublicationCapability {
    _private: (),
}

impl OperatorPublicationCapability {
    pub(crate) const fn from_operator_transport() -> Self {
        Self { _private: () }
    }
}

#[derive(Clone, Debug)]
pub(crate) struct OperatorPublicationRpc {
    store: PublicationStore,
}

impl OperatorPublicationRpc {
    pub(crate) fn from_operator_transport(
        _capability: OperatorPublicationCapability,
        store: PublicationStore,
    ) -> Self {
        Self { store }
    }

    pub(crate) async fn dispatch(
        &self,
        frame: &[u8],
    ) -> Result<Vec<u8>, OperatorPublicationRpcError> {
        let envelope = decode_routing_envelope(frame, REQUEST_FRAME_MAX_BYTES)?;
        let request_id = envelope.request_id.clone();
        let operation = envelope.operation.clone();
        let response = self.dispatch_create(frame).await;
        Ok(match response {
            Ok(response) => response,
            Err(error) => json::to_string(&ErrorResponse {
                protocol_version: PROTOCOL_VERSION_V1,
                request_id,
                operation,
                error_code: publication_error_code(&error).to_owned(),
                message: error.to_string(),
            })
            .into_bytes(),
        })
    }

    async fn dispatch_create(&self, frame: &[u8]) -> Result<Vec<u8>, OperatorPublicationRpcError> {
        let request: OperatorPublicationCreateRequest = decode_operation_request(
            frame,
            REQUEST_FRAME_MAX_BYTES,
            factory_protocol::OP_OPERATOR_PUBLICATION_CREATE,
        )?;
        let command = request.publication_command()?;
        let receipt = self
            .store
            .create_from_operator(
                request.application_revision_id()?,
                request.authoring_office_id()?,
                &command_from_request(&command)?,
            )
            .await?;
        Ok(json::to_string(&PublicationReceiptResponse {
            protocol_version: PROTOCOL_VERSION_V1,
            request_id: request.request_id,
            operation: factory_protocol::OP_OPERATOR_PUBLICATION_CREATE.to_owned(),
            audit_id: receipt.audit_log_id.get(),
            aggregate_revision: receipt.resulting_revision.get(),
            publication_id: receipt.publication_id.get(),
            was_idempotent_retry: receipt.was_idempotent_retry,
        })
        .into_bytes())
    }
}

#[derive(Debug, Error)]
pub(crate) enum OperatorPublicationRpcError {
    #[error(transparent)]
    Frame(#[from] factory_protocol::FrameError),
    #[error(transparent)]
    Contract(#[from] factory_protocol::ContractError),
    #[error(transparent)]
    Store(#[from] PublicationStoreError),
}

fn publication_error_code(error: &OperatorPublicationRpcError) -> &'static str {
    match error {
        OperatorPublicationRpcError::Store(PublicationStoreError::IdempotencyConflict {
            ..
        }) => "idempotency_conflict",
        OperatorPublicationRpcError::Frame(_) | OperatorPublicationRpcError::Contract(_) => {
            "invalid_operator_publication_request"
        }
        OperatorPublicationRpcError::Store(_) => "operator_publication_rejected",
    }
}
