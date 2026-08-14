use std::fmt;

use crate::{ContentDigest, ContractError};

macro_rules! numeric_identifier {
    ($name:ident, $label:literal) => {
        #[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
        #[repr(transparent)]
        pub struct $name(i64);

        impl $name {
            pub fn new(value: i64) -> Result<Self, ContractError> {
                if value <= 0 {
                    return Err(ContractError::NonPositiveIdentifier { kind: $label });
                }
                Ok(Self(value))
            }

            #[must_use]
            pub const fn get(self) -> i64 {
                self.0
            }
        }

        impl fmt::Display for $name {
            fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
                self.0.fmt(formatter)
            }
        }
    };
}

numeric_identifier!(ApplicationId, "application ID");
numeric_identifier!(ApplicationRevisionId, "application revision ID");
numeric_identifier!(RepositoryId, "repository ID");
numeric_identifier!(CampaignId, "campaign ID");
numeric_identifier!(OfficeId, "office ID");
numeric_identifier!(TicketId, "ticket ID");
numeric_identifier!(TicketRevisionId, "ticket revision ID");
numeric_identifier!(TicketAttemptId, "ticket attempt ID");
numeric_identifier!(AssignmentId, "assignment ID");
numeric_identifier!(SessionId, "session ID");
numeric_identifier!(ArtifactId, "artifact ID");
numeric_identifier!(CandidateId, "candidate ID");
numeric_identifier!(ValidationId, "validation ID");
numeric_identifier!(ReviewId, "review ID");
numeric_identifier!(ArchitectDecisionId, "architect decision ID");
numeric_identifier!(DeliveryId, "delivery ID");
numeric_identifier!(ForumTopicId, "forum topic ID");
numeric_identifier!(ForumThreadId, "forum thread ID");
numeric_identifier!(ForumPostId, "forum post ID");
numeric_identifier!(AuditLogId, "audit log ID");

/// A kernel build is content-identified; it is not a random durable ID.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
#[repr(transparent)]
pub struct KernelBuildId(ContentDigest);

impl KernelBuildId {
    #[must_use]
    pub const fn new(digest: ContentDigest) -> Self {
        Self(digest)
    }

    #[must_use]
    pub const fn digest(self) -> ContentDigest {
        self.0
    }
}

impl fmt::Display for KernelBuildId {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.0.fmt(formatter)
    }
}
