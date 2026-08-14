//! Authenticated local-operator application control routing.
//!
//! This adapter is deliberately distinct from actor protocol routing and from
//! every product-specific application module. The operator transport mints its capability
//! after binding the mode-`0600` socket, then supplies only the narrow kernel
//! store and CAS custody objects.  Application source is read only by
//! `KernelStore::admit_compiled_application`; neither factoryctl nor this
//! router evaluates product-specific code.

use std::{path::PathBuf, sync::Arc};

use factory_protocol::{
    AggregateRevision, ApplicationRevisionReceiptResponse, ApplicationShowResponse,
    ConflictResponse, ContractError, ErrorResponse, ExpectedRevision, FrameError,
    OP_OPERATOR_ACTIVATE_APPLICATION, OP_OPERATOR_REGISTER_APPLICATION,
    OP_OPERATOR_SHOW_APPLICATION, OperatorApplicationActivateRequest,
    OperatorApplicationRegisterRequest, OperatorApplicationShowRequest, PROTOCOL_VERSION_V1,
    decode_operation_request, decode_routing_envelope,
};
use miniserde::json;
use thiserror::Error;

use crate::{
    cas::CasStore,
    storage::{
        ActivateApplicationRevision, AdmitCompiledApplication, ApplicationActivationReceipt,
        ApplicationRevisionReceipt, ApplicationRevisionView, KernelStore, StoreError,
    },
};

/// Capability minted only at the authenticated operator listener.  Its
/// private constructor means actor sockets cannot obtain application control
/// by placing an `operator.application.*` string in a frame.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct OperatorApplicationCapability {
    _private: (),
}

impl OperatorApplicationCapability {
    pub(crate) const fn from_operator_transport() -> Self {
        Self { _private: () }
    }
}

/// Operator-only application command adapter.  It owns no database URL or
/// raw pool.  CAS is held only to let the daemon, not its client, re-read and
/// adopt the source-root bundle and all declared template bytes.
#[derive(Clone, Debug)]
pub(crate) struct ApplicationOperatorRpc {
    store: KernelStore,
    cas: Arc<CasStore>,
}

impl ApplicationOperatorRpc {
    pub(crate) fn from_operator_transport(
        _capability: OperatorApplicationCapability,
        store: KernelStore,
        cas: Arc<CasStore>,
    ) -> Self {
        Self { store, cas }
    }

    /// Dispatches only the three generic application operations.  Each error
    /// below is a typed response after a valid frame; malformed frames remain
    /// a transport rejection and cannot reach storage or CAS authority.
    pub(crate) async fn dispatch(
        &self,
        frame: &[u8],
    ) -> Result<Vec<u8>, ApplicationOperatorRpcError> {
        let envelope = decode_routing_envelope(frame, factory_protocol::REQUEST_FRAME_MAX_BYTES)?;
        let request_id = envelope.request_id.clone();
        let operation = envelope.operation.clone();
        let outcome = match operation.as_str() {
            OP_OPERATOR_SHOW_APPLICATION => self.dispatch_show(frame).await,
            OP_OPERATOR_REGISTER_APPLICATION => self.dispatch_register(frame).await,
            OP_OPERATOR_ACTIVATE_APPLICATION => self.dispatch_activate(frame).await,
            _ => return Err(ApplicationOperatorRpcError::OperationNotApplication { operation }),
        };
        Ok(match outcome {
            Ok(response) => response,
            Err(rejection) => rejection.response(request_id, envelope.operation),
        })
    }

    async fn dispatch_show(&self, frame: &[u8]) -> Result<Vec<u8>, ApplicationOperationRejection> {
        let request: OperatorApplicationShowRequest = decode_operation_request(
            frame,
            factory_protocol::REQUEST_FRAME_MAX_BYTES,
            OP_OPERATOR_SHOW_APPLICATION,
        )
        .map_err(ApplicationOperationRejection::Frame)?;
        let application_key = request
            .application_key()
            .map_err(ApplicationOperationRejection::Contract)?;
        let application_revision_id = request
            .application_revision_id()
            .map_err(ApplicationOperationRejection::Contract)?;
        let view = self
            .store
            .active_application_view(&application_key, application_revision_id)
            .await
            .map_err(ApplicationOperationRejection::Store)?;
        Ok(show_response(request.request_id, view))
    }

    async fn dispatch_register(
        &self,
        frame: &[u8],
    ) -> Result<Vec<u8>, ApplicationOperationRejection> {
        let request: OperatorApplicationRegisterRequest = decode_operation_request(
            frame,
            factory_protocol::REQUEST_FRAME_MAX_BYTES,
            OP_OPERATOR_REGISTER_APPLICATION,
        )
        .map_err(ApplicationOperationRejection::Frame)?;
        let kernel_build_id = request
            .kernel_build_id()
            .map_err(ApplicationOperationRejection::Contract)?;
        let source_root = request
            .source_root()
            .map_err(ApplicationOperationRejection::Contract)?;
        let bundle_relative_path = request
            .bundle_relative_path()
            .map_err(ApplicationOperationRejection::Contract)?;
        let request_id = request.request_id.clone();
        let receipt = self
            .store
            .admit_compiled_application(
                self.cas.as_ref(),
                &AdmitCompiledApplication {
                    principal: request.principal,
                    command_id: request.client_command_id,
                    expected_revision: expected_revision(request.expected_revision),
                    expected_kernel_build_revision: expected_revision(
                        request.expected_kernel_build_revision,
                    ),
                    kernel_build_id,
                    source_root: PathBuf::from(source_root.as_str()),
                    bundle_relative_path: PathBuf::from(bundle_relative_path.as_str()),
                },
            )
            .await
            .map_err(ApplicationOperationRejection::Store)?;
        Ok(admission_response(request_id, receipt))
    }

    async fn dispatch_activate(
        &self,
        frame: &[u8],
    ) -> Result<Vec<u8>, ApplicationOperationRejection> {
        let request: OperatorApplicationActivateRequest = decode_operation_request(
            frame,
            factory_protocol::REQUEST_FRAME_MAX_BYTES,
            OP_OPERATOR_ACTIVATE_APPLICATION,
        )
        .map_err(ApplicationOperationRejection::Frame)?;
        let request_id = request.request_id.clone();
        let principal = request
            .principal()
            .map_err(ApplicationOperationRejection::Contract)?;
        let application_key = request
            .application_key()
            .map_err(ApplicationOperationRejection::Contract)?;
        let application_revision_id = request
            .application_revision_id()
            .map_err(ApplicationOperationRejection::Contract)?;
        let rationale = request
            .rationale()
            .map_err(ApplicationOperationRejection::Contract)?;
        let command = ActivateApplicationRevision {
            principal,
            command_id: request.client_command_id,
            expected_revision: expected_revision(request.expected_revision),
            application_key,
            application_revision_id,
            rationale,
        };
        let receipt = self
            .store
            .activate_application_revision(&command)
            .await
            .map_err(ApplicationOperationRejection::Store)?;
        Ok(activation_response(request_id, receipt))
    }
}

#[derive(Debug, Error)]
pub(crate) enum ApplicationOperatorRpcError {
    #[error(transparent)]
    Frame(#[from] FrameError),

    #[error("operation {operation:?} is not an application operation")]
    OperationNotApplication { operation: String },
}

#[derive(Debug)]
enum ApplicationOperationRejection {
    Frame(FrameError),
    Contract(ContractError),
    Store(StoreError),
}

impl ApplicationOperationRejection {
    fn response(self, request_id: String, operation: String) -> Vec<u8> {
        match self {
            Self::Store(StoreError::RevisionConflict { current, .. }) => {
                json::to_string(&ConflictResponse {
                    protocol_version: PROTOCOL_VERSION_V1,
                    request_id,
                    operation,
                    error_code: "revision_conflict".to_owned(),
                    current_revision: current.get(),
                    message: "the observed application revision is stale".to_owned(),
                })
                .into_bytes()
            }
            Self::Store(error) => error_response(
                request_id,
                operation,
                application_store_error_code(&error),
                &error.to_string(),
            ),
            Self::Contract(error) => error_response(
                request_id,
                operation,
                "invalid_application_request",
                &error.to_string(),
            ),
            Self::Frame(error) => error_response(
                request_id,
                operation,
                "invalid_application_request",
                &error.to_string(),
            ),
        }
    }
}

fn expected_revision(value: u64) -> ExpectedRevision {
    ExpectedRevision::new(AggregateRevision::from_persisted(value))
}

fn show_response(request_id: String, view: ApplicationRevisionView) -> Vec<u8> {
    json::to_string(&ApplicationShowResponse {
        protocol_version: PROTOCOL_VERSION_V1,
        request_id,
        operation: OP_OPERATOR_SHOW_APPLICATION.to_owned(),
        application_key: view.application_key.as_str().to_owned(),
        application_revision_id: view.application_revision_id.get(),
        aggregate_revision: view.aggregate_revision.get(),
        bundle_artifact_id: view.bundle_artifact_id.get(),
        is_active: view.is_active,
    })
    .into_bytes()
}

fn admission_response(request_id: String, receipt: ApplicationRevisionReceipt) -> Vec<u8> {
    json::to_string(&ApplicationRevisionReceiptResponse {
        protocol_version: PROTOCOL_VERSION_V1,
        request_id,
        operation: OP_OPERATOR_REGISTER_APPLICATION.to_owned(),
        audit_id: receipt.audit_log_id,
        aggregate_revision: receipt.resulting_revision.get(),
        application_revision_id: receipt.application_revision_id.get(),
        is_active: false,
        was_idempotent_retry: receipt.was_idempotent_retry,
    })
    .into_bytes()
}

fn activation_response(request_id: String, receipt: ApplicationActivationReceipt) -> Vec<u8> {
    json::to_string(&ApplicationRevisionReceiptResponse {
        protocol_version: PROTOCOL_VERSION_V1,
        request_id,
        operation: OP_OPERATOR_ACTIVATE_APPLICATION.to_owned(),
        audit_id: receipt.audit_log_id,
        aggregate_revision: receipt.resulting_revision.get(),
        application_revision_id: receipt.application_revision_id.get(),
        is_active: receipt.is_active,
        was_idempotent_retry: receipt.was_idempotent_retry,
    })
    .into_bytes()
}

fn error_response(
    request_id: String,
    operation: String,
    error_code: &str,
    message: &str,
) -> Vec<u8> {
    json::to_string(&ErrorResponse {
        protocol_version: PROTOCOL_VERSION_V1,
        request_id,
        operation,
        error_code: error_code.to_owned(),
        message: message.to_owned(),
    })
    .into_bytes()
}

fn application_store_error_code(error: &StoreError) -> &'static str {
    match error {
        StoreError::IdempotencyConflict { .. } => "idempotency_conflict",
        StoreError::UnknownKernelBuild { .. } => "unknown_kernel_build",
        StoreError::UnknownRepositoryBinding { .. } => "unknown_repository_binding",
        StoreError::UnknownApplicationRevisionForKey { .. }
        | StoreError::UnknownApplicationRevision { .. } => "unknown_application_revision",
        StoreError::ApplicationRevisionInactive { .. } => "application_revision_inactive",
        StoreError::ApplicationActivationCampaignRunning { .. } => {
            "application_activation_campaign_running"
        }
        StoreError::ApplicationActivationRationaleMismatch => "rationale_artifact_mismatch",
        _ => "application_rejected",
    }
}

#[cfg(test)]
mod tests {
    use factory_protocol::{
        OperatorApplicationShowRequest, PROTOCOL_VERSION_V1, REQUEST_FRAME_MAX_BYTES,
        encode_json_frame,
    };

    use super::*;

    #[test]
    fn malformed_application_request_cannot_be_routed_as_a_storage_command() {
        let frame = encode_json_frame(
            &OperatorApplicationShowRequest {
                protocol_version: PROTOCOL_VERSION_V1,
                request_id: "application-show-1".to_owned(),
                operation: OP_OPERATOR_SHOW_APPLICATION.to_owned(),
                application_key: "Not-lowercase".to_owned(),
                application_revision_id: None,
            },
            REQUEST_FRAME_MAX_BYTES,
        )
        .expect("frame");
        let request: OperatorApplicationShowRequest = decode_operation_request(
            &frame,
            REQUEST_FRAME_MAX_BYTES,
            OP_OPERATOR_SHOW_APPLICATION,
        )
        .expect("wire shape");
        assert!(request.application_key().is_err());
    }
}
