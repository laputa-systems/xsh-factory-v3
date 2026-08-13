/// The fixed paid offices. The external Grand Architect is not an actor office.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Office {
    ProductResearch,
    Engineering,
    Quality,
}

impl Office {
    pub const ALL: [Self; 3] = [Self::ProductResearch, Self::Engineering, Self::Quality];
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum CampaignState {
    Running,
    Completed,
    Failed,
    Cancelled,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum TicketState {
    Proposed,
    Sponsored,
    InFlight,
    Delivered,
    Blocked,
    Resolved,
    Superseded,
    Rejected,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum TicketAttemptStage {
    Engineering,
    HardValidation,
    Quality,
    AwaitingArchitect,
    ReworkEngineering,
    ReworkValidation,
    ReworkQuality,
    Delivered,
    Failed,
    Cancelled,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum AssignmentState {
    Prepared,
    Running,
    Succeeded,
    Failed,
    Cancelled,
    Interrupted,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum SessionState {
    Prepared,
    Running,
    Succeeded,
    Failed,
    Cancelled,
    Interrupted,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum CandidateState {
    Submitted,
    Validated,
    Rejected,
    Accepted,
    Delivered,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum ValidationState {
    Running,
    Passed,
    Failed,
    Interrupted,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum ReviewVerdict {
    Accept,
    Reject,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum DeliveryState {
    Pending,
    Delivered,
    Failed,
}
