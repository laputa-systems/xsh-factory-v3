use crate::ContractError;

/// A slash-separated path stored beneath a kernel-owned runtime root.
///
/// The representation rejects platform-dependent separators, empty segments,
/// and traversal so a later physical filesystem boundary has one canonical
/// relative form to resolve.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[repr(transparent)]
pub struct RuntimeRelativePath(String);

/// A slash-separated path relative to the assigned product repository.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[repr(transparent)]
pub struct RepositoryRelativePath(String);

/// A slash-separated path relative to the application source package.
///
/// Template source paths use this type rather than [`RepositoryRelativePath`]:
/// application policy is compiled from the factory repository, while required
/// reads and product command directories resolve in the product checkout.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[repr(transparent)]
pub struct ApplicationRelativePath(String);

/// An explicit local host path used only while compiling a repository binding.
/// It cannot be persisted as a runtime-relative CAS path.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[repr(transparent)]
pub struct AbsoluteHostPath(String);

impl RuntimeRelativePath {
    pub fn parse(value: impl Into<String>) -> Result<Self, ContractError> {
        validate_safe_relative("runtime-relative path", &value.into(), false).map(Self)
    }

    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl RepositoryRelativePath {
    pub fn parse(value: impl Into<String>) -> Result<Self, ContractError> {
        validate_safe_relative("repository-relative path", &value.into(), true).map(Self)
    }

    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl ApplicationRelativePath {
    pub fn parse(value: impl Into<String>) -> Result<Self, ContractError> {
        validate_safe_relative("application-relative path", &value.into(), false).map(Self)
    }

    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl AbsoluteHostPath {
    pub fn parse(value: impl Into<String>) -> Result<Self, ContractError> {
        let value = value.into();
        if value.is_empty() || value.contains('\0') {
            return Err(ContractError::InvalidValue {
                field: "absolute host path",
                reason: "must be non-empty UTF-8 without NUL",
            });
        }
        if !value.starts_with('/') {
            return Err(ContractError::InvalidValue {
                field: "absolute host path",
                reason: "must start with a slash",
            });
        }
        Ok(Self(value))
    }

    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

fn validate_safe_relative(
    field: &'static str,
    value: &str,
    allow_root: bool,
) -> Result<String, ContractError> {
    if value.is_empty() {
        return Err(ContractError::UnsafeRelativePath {
            field,
            reason: "path is empty",
        });
    }
    if value.contains('\0') {
        return Err(ContractError::UnsafeRelativePath {
            field,
            reason: "path contains NUL",
        });
    }
    if value.starts_with('/') {
        return Err(ContractError::UnsafeRelativePath {
            field,
            reason: "path is absolute",
        });
    }
    if value.contains('\\') {
        return Err(ContractError::UnsafeRelativePath {
            field,
            reason: "backslash is not a canonical separator",
        });
    }
    if value.split('/').any(|segment| segment.is_empty()) {
        return Err(ContractError::UnsafeRelativePath {
            field,
            reason: "path contains an empty segment",
        });
    }
    if value == "." && allow_root {
        return Ok(value.to_owned());
    }
    if value
        .split('/')
        .any(|segment| segment == "." || segment == "..")
    {
        return Err(ContractError::UnsafeRelativePath {
            field,
            reason: "path contains traversal",
        });
    }
    Ok(value.to_owned())
}
