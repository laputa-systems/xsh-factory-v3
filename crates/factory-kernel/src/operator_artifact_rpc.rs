//! Operator-only adoption of one bounded local source file into CAS.
//!
//! This is the narrow provenance bridge for human-authored rationale and
//! similar operator evidence. The client names an absolute source root and a
//! canonical relative file path, while daemon-owned CAS re-opens, canonicalizes
//! and seals the regular file before ordinary artifact registration. No bytes,
//! database handle, or arbitrary daemon write capability cross the socket.

use std::{path::PathBuf, sync::Arc};

use factory_protocol::{
    AggregateRevision, ContractError, ErrorResponse, ExpectedRevision, FrameError,
    OP_OPERATOR_SEAL_ARTIFACT, OperatorArtifactSealReceiptResponse, OperatorArtifactSealRequest,
    PROTOCOL_VERSION_V2, decode_operation_request, decode_routing_envelope,
};
use miniserde::json;
use thiserror::Error;

use crate::{
    cas::CasStore,
    storage::{KernelStore, RegisterArtifact, StoreError},
};

/// Capability minted only at the operator listener after the mode-`0600`
/// socket has been bound. Actor routes have no constructor for this type.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct OperatorArtifactCapability {
    _private: (),
}

impl OperatorArtifactCapability {
    pub(crate) const fn from_operator_transport() -> Self {
        Self { _private: () }
    }
}

/// Thin adapter over physical custody and the ordinary immutable artifact
/// registration command. It stores neither operator paths nor source bytes.
#[derive(Clone, Debug)]
pub(crate) struct OperatorArtifactRpc {
    store: KernelStore,
    cas: Arc<CasStore>,
}

impl OperatorArtifactRpc {
    pub(crate) fn from_operator_transport(
        _capability: OperatorArtifactCapability,
        store: KernelStore,
        cas: Arc<CasStore>,
    ) -> Self {
        Self { store, cas }
    }

    pub(crate) async fn dispatch(&self, frame: &[u8]) -> Result<Vec<u8>, OperatorArtifactRpcError> {
        let envelope = decode_routing_envelope(frame, factory_protocol::REQUEST_FRAME_MAX_BYTES)?;
        let request_id = envelope.request_id.clone();
        let operation = envelope.operation.clone();
        let outcome = match operation.as_str() {
            OP_OPERATOR_SEAL_ARTIFACT => self.dispatch_seal(frame).await,
            _ => return Err(OperatorArtifactRpcError::OperationNotArtifact { operation }),
        };
        Ok(match outcome {
            Ok(response) => response,
            Err(rejection) => rejection.response(request_id, envelope.operation),
        })
    }

    async fn dispatch_seal(&self, frame: &[u8]) -> Result<Vec<u8>, OperatorArtifactRejection> {
        let request: OperatorArtifactSealRequest = decode_operation_request(
            frame,
            factory_protocol::REQUEST_FRAME_MAX_BYTES,
            OP_OPERATOR_SEAL_ARTIFACT,
        )
        .map_err(OperatorArtifactRejection::Frame)?;
        let source_root = request
            .source_root()
            .map_err(OperatorArtifactRejection::Contract)?;
        let source_relative_path = request
            .source_relative_path()
            .map_err(OperatorArtifactRejection::Contract)?;
        let sealed = self
            .cas
            .adopt(
                PathBuf::from(source_root.as_str()),
                source_relative_path.as_str(),
            )
            .map_err(StoreError::from)
            .map_err(OperatorArtifactRejection::Store)?;
        let expected_kernel_build_revision =
            expected_revision(request.expected_kernel_build_revision);
        let kernel_build = self
            .store
            .kernel_build_at_revision(expected_kernel_build_revision)
            .await
            .map_err(OperatorArtifactRejection::Store)?;
        let request_id = request.request_id.clone();
        let receipt = self
            .store
            .register_artifact(
                self.cas.as_ref(),
                &RegisterArtifact {
                    principal: request.principal,
                    command_id: request.client_command_id,
                    expected_kernel_build_revision,
                    kernel_build_id: kernel_build.kernel_build_id,
                    sealed,
                },
            )
            .await
            .map_err(OperatorArtifactRejection::Store)?;
        Ok(json::to_string(&OperatorArtifactSealReceiptResponse {
            protocol_version: PROTOCOL_VERSION_V2,
            request_id,
            operation: OP_OPERATOR_SEAL_ARTIFACT.to_owned(),
            audit_id: receipt.audit_log_id,
            aggregate_revision: kernel_build.aggregate_revision.get(),
            artifact_id: receipt.artifact_id.get(),
            digest: sealed.digest().to_hex(),
            byte_length: sealed.byte_length(),
            was_idempotent_retry: receipt.was_idempotent_retry,
            was_reused: receipt.was_reused,
        })
        .into_bytes())
    }
}

#[derive(Debug, Error)]
pub(crate) enum OperatorArtifactRpcError {
    #[error(transparent)]
    Frame(#[from] FrameError),

    #[error("operation {operation:?} is not an operator artifact operation")]
    OperationNotArtifact { operation: String },
}

#[derive(Debug)]
enum OperatorArtifactRejection {
    Frame(FrameError),
    Contract(ContractError),
    Store(StoreError),
}

impl OperatorArtifactRejection {
    fn response(self, request_id: String, operation: String) -> Vec<u8> {
        let (error_code, message) = match self {
            Self::Frame(error) => ("invalid_operator_artifact_request", error.to_string()),
            Self::Contract(error) => ("invalid_operator_artifact_request", error.to_string()),
            Self::Store(StoreError::RevisionConflict { current, .. }) => {
                return json::to_string(&factory_protocol::ConflictResponse {
                    protocol_version: PROTOCOL_VERSION_V2,
                    request_id,
                    operation,
                    error_code: "revision_conflict".to_owned(),
                    current_revision: current.get(),
                    message: "the observed kernel-build revision is stale".to_owned(),
                })
                .into_bytes();
            }
            Self::Store(error) => (
                operator_artifact_store_error_code(&error),
                error.to_string(),
            ),
        };
        json::to_string(&ErrorResponse {
            protocol_version: PROTOCOL_VERSION_V2,
            request_id,
            operation,
            error_code: error_code.to_owned(),
            message,
        })
        .into_bytes()
    }
}

fn expected_revision(value: u64) -> ExpectedRevision {
    ExpectedRevision::new(AggregateRevision::from_persisted(value))
}

fn operator_artifact_store_error_code(error: &StoreError) -> &'static str {
    match error {
        StoreError::IdempotencyConflict { .. } => "idempotency_conflict",
        StoreError::NoCurrentKernelBuild => "no_current_kernel_build",
        StoreError::UnknownKernelBuildRevision { .. } => "unknown_kernel_build_revision",
        StoreError::InvalidProcessCommand { .. } | StoreError::InvalidCommandComponent { .. } => {
            "invalid_operator_artifact_request"
        }
        _ => "operator_artifact_rejected",
    }
}

#[cfg(test)]
mod tests {
    use factory_protocol::{
        OperatorArtifactSealRequest, PROTOCOL_VERSION_V2, REQUEST_FRAME_MAX_BYTES,
        encode_json_frame,
    };

    use super::*;

    #[test]
    fn operator_artifact_wire_requires_an_absolute_root_and_safe_relative_path() {
        let frame = encode_json_frame(
            &OperatorArtifactSealRequest {
                protocol_version: PROTOCOL_VERSION_V2,
                request_id: "operator-artifact-1".to_owned(),
                operation: OP_OPERATOR_SEAL_ARTIFACT.to_owned(),
                client_command_id: "operator-artifact-seal-1".to_owned(),
                expected_kernel_build_revision: 1,
                source_root: "/operator/evidence".to_owned(),
                source_relative_path: "rationale.md".to_owned(),
                principal: "grand-architect".to_owned(),
            },
            REQUEST_FRAME_MAX_BYTES,
        )
        .expect("frame");
        let request: OperatorArtifactSealRequest =
            decode_operation_request(&frame, REQUEST_FRAME_MAX_BYTES, OP_OPERATOR_SEAL_ARTIFACT)
                .expect("typed request");
        assert!(request.source_root().is_ok());
        assert!(request.source_relative_path().is_ok());
    }
}
