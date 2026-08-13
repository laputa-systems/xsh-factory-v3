use thiserror::Error;

/// Rejections that are useful at a protocol or domain boundary.
///
/// Later physical-boundary errors wrap these rather than replacing a precise
/// invariant with an unstructured string.
#[derive(Debug, Error, Clone, PartialEq, Eq)]
pub enum ContractError {
    #[error("{kind} must be greater than zero")]
    NonPositiveIdentifier { kind: &'static str },

    #[error("{field} is invalid: {reason}")]
    InvalidValue {
        field: &'static str,
        reason: &'static str,
    },

    #[error("{field} exceeds its maximum of {maximum} bytes")]
    ByteLimitExceeded { field: &'static str, maximum: usize },

    #[error("{field} must be a safe relative path: {reason}")]
    UnsafeRelativePath {
        field: &'static str,
        reason: &'static str,
    },

    #[error("aggregate revision overflow")]
    RevisionOverflow,

    #[error("application bundle invariant {invariant} failed: {evidence}")]
    BundleInvariant {
        invariant: &'static str,
        evidence: &'static str,
    },
}
