use crate::ContractError;

/// Monotonically increasing optimistic-concurrency revision of one aggregate.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[repr(transparent)]
pub struct AggregateRevision(u64);

impl AggregateRevision {
    #[must_use]
    pub const fn initial() -> Self {
        Self(0)
    }

    #[must_use]
    pub const fn get(self) -> u64 {
        self.0
    }

    /// Reconstitutes a revision previously validated as a nonnegative SQL
    /// `BIGINT`. Every `u64` is a legal aggregate revision; mutation still
    /// advances only through [`Self::next`].
    #[must_use]
    pub const fn from_persisted(value: u64) -> Self {
        Self(value)
    }

    pub fn next(self) -> Result<Self, ContractError> {
        self.0
            .checked_add(1)
            .map(Self)
            .ok_or(ContractError::RevisionOverflow)
    }
}

/// A caller's explicit observation of an aggregate revision.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[repr(transparent)]
pub struct ExpectedRevision(AggregateRevision);

impl ExpectedRevision {
    #[must_use]
    pub const fn new(value: AggregateRevision) -> Self {
        Self(value)
    }

    #[must_use]
    pub const fn get(self) -> AggregateRevision {
        self.0
    }
}
