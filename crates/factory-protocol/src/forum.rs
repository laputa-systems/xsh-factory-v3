//! Typed, product-neutral Forum contracts.
//!
//! The Forum is intentionally a communication record, not a workflow or
//! authority mechanism.  These values make the byte limits, immutable
//! relations, attribution, filters, and stable pagination boundary explicit
//! before a daemon or a SQL adapter accepts a command.

use std::{collections::BTreeSet, fmt, str::FromStr};

use miniserde::{Deserialize, Serialize};

use crate::{
    AggregateRevision, ArtifactId, AssignmentRole, ContractError, ForumPostId, ForumThreadId,
    ForumTopicId, SessionId,
};

/// Maximum UTF-8 byte lengths from the Forum contract.
pub const FORUM_TOPIC_NAME_MAX_BYTES: usize = 160;
pub const FORUM_TOPIC_DESCRIPTION_MAX_BYTES: usize = 4 * 1024;
pub const FORUM_THREAD_TITLE_MAX_BYTES: usize = 240;
pub const FORUM_POST_BODY_MAX_BYTES: usize = 16 * 1024;
pub const FORUM_ATTACHMENT_LABEL_MAX_BYTES: usize = 160;
pub const FORUM_SEARCH_QUERY_MAX_BYTES: usize = 4 * 1024;
pub const FORUM_SEARCH_CURSOR_MAX_BYTES: usize = 512;
pub const FORUM_SNIPPET_MAX_BYTES: usize = 1024;
pub const FORUM_PAGE_MAX: u8 = 20;
pub const FORUM_MAX_ATTACHMENTS_PER_POST: usize = 8;
pub const FORUM_COMMAND_ID_MAX_BYTES: usize = 160;

/// Count and byte quotas are kept together so a SQL/SDK adapter cannot apply
/// a text limit while silently forgetting the bounded attachment collection.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ForumQuotas {
    pub topic_name_max_bytes: usize,
    pub topic_description_max_bytes: usize,
    pub thread_title_max_bytes: usize,
    pub post_body_max_bytes: usize,
    pub attachment_label_max_bytes: usize,
    pub search_query_max_bytes: usize,
    pub search_cursor_max_bytes: usize,
    pub snippet_max_bytes: usize,
    pub page_max: u8,
    pub max_attachments_per_post: usize,
}

impl Default for ForumQuotas {
    fn default() -> Self {
        Self {
            topic_name_max_bytes: FORUM_TOPIC_NAME_MAX_BYTES,
            topic_description_max_bytes: FORUM_TOPIC_DESCRIPTION_MAX_BYTES,
            thread_title_max_bytes: FORUM_THREAD_TITLE_MAX_BYTES,
            post_body_max_bytes: FORUM_POST_BODY_MAX_BYTES,
            attachment_label_max_bytes: FORUM_ATTACHMENT_LABEL_MAX_BYTES,
            search_query_max_bytes: FORUM_SEARCH_QUERY_MAX_BYTES,
            search_cursor_max_bytes: FORUM_SEARCH_CURSOR_MAX_BYTES,
            snippet_max_bytes: FORUM_SNIPPET_MAX_BYTES,
            page_max: FORUM_PAGE_MAX,
            max_attachments_per_post: FORUM_MAX_ATTACHMENTS_PER_POST,
        }
    }
}

/// The only identity supplied by a mutating Forum command.  Author and office
/// are deliberately absent: the daemon derives them from the bound socket.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ForumMutationIdentity {
    client_command_id: String,
    expected_revision: AggregateRevision,
}

impl ForumMutationIdentity {
    pub fn new(
        client_command_id: impl Into<String>,
        expected_revision: AggregateRevision,
    ) -> Result<Self, ContractError> {
        let client_command_id = client_command_id.into();
        validate_text(
            &client_command_id,
            "forum client command ID",
            FORUM_COMMAND_ID_MAX_BYTES,
            true,
        )?;
        Ok(Self {
            client_command_id,
            expected_revision,
        })
    }

    #[must_use]
    pub fn client_command_id(&self) -> &str {
        &self.client_command_id
    }

    #[must_use]
    pub const fn expected_revision(&self) -> AggregateRevision {
        self.expected_revision
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ForumCreateTopicInput {
    pub name: ForumTopicName,
    pub description: ForumTopicDescription,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ForumCreateThreadInput {
    pub topic_id: ForumTopicId,
    pub title: ForumThreadTitle,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ForumCreateTopicCommand {
    pub identity: ForumMutationIdentity,
    pub input: ForumCreateTopicInput,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ForumCreateThreadCommand {
    pub identity: ForumMutationIdentity,
    pub input: ForumCreateThreadInput,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ForumPostCommand {
    pub identity: ForumMutationIdentity,
    pub thread_id: ForumThreadId,
    pub input: ForumPostInput,
}

/// Flat wire payloads for the Forum operations.  They intentionally carry no
/// principal, office, session, or assignment field: that authority comes from
/// the daemon's inherited actor connection.  The existing generic wire module
/// can route these operation-specific values without introducing a dynamic
/// payload map.
pub const OP_FORUM_LIST_TOPICS_V2: &str = "forum.list_topics";
pub const OP_FORUM_LIST_THREADS_V2: &str = "forum.list_threads";
pub const OP_FORUM_SEARCH_V2: &str = "forum.search";
pub const OP_FORUM_READ_THREAD_V2: &str = "forum.read_thread";
pub const OP_FORUM_CREATE_TOPIC_V2: &str = "forum.create_topic";
pub const OP_FORUM_CREATE_THREAD_V2: &str = "forum.create_thread";
pub const OP_FORUM_POST_V2: &str = "forum.post";

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ForumListTopicsRequestV2 {
    pub protocol_version: u16,
    pub request_id: String,
    pub operation: String,
    pub cursor: String,
    pub limit: u8,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ForumListThreadsRequestV2 {
    pub protocol_version: u16,
    pub request_id: String,
    pub operation: String,
    pub topic_id: i64,
    pub cursor: String,
    pub limit: u8,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ForumSearchRequestV2 {
    pub protocol_version: u16,
    pub request_id: String,
    pub operation: String,
    pub query: String,
    pub topic_id: Option<i64>,
    pub thread_id: Option<i64>,
    pub author_office: Option<u8>,
    pub post_kind: Option<u8>,
    pub created_after_micros: Option<u64>,
    pub created_before_micros: Option<u64>,
    pub cursor: String,
    pub limit: u8,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ForumReadThreadRequestV2 {
    pub protocol_version: u16,
    pub request_id: String,
    pub operation: String,
    pub thread_id: i64,
    pub after_post_id: i64,
    pub limit: u8,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ForumCreateTopicRequestV2 {
    pub protocol_version: u16,
    pub request_id: String,
    pub operation: String,
    pub client_command_id: String,
    pub expected_revision: u64,
    pub name: String,
    pub description: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ForumCreateThreadRequestV2 {
    pub protocol_version: u16,
    pub request_id: String,
    pub operation: String,
    pub client_command_id: String,
    pub expected_revision: u64,
    pub topic_id: i64,
    pub title: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ForumAttachmentWireV2 {
    pub artifact_id: i64,
    pub label: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ForumPostRequestV2 {
    pub protocol_version: u16,
    pub request_id: String,
    pub operation: String,
    pub client_command_id: String,
    pub expected_revision: u64,
    pub thread_id: i64,
    pub kind: u8,
    pub body: String,
    pub reply_to: Option<i64>,
    pub supersedes: Option<i64>,
    pub attachments: Vec<ForumAttachmentWireV2>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ForumTopicViewV2 {
    pub id: i64,
    pub name: String,
    pub description: String,
    pub author_kind: u8,
    pub author_session_id: Option<i64>,
    pub author_office: Option<u8>,
    pub created_at_micros: u64,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ForumThreadViewV2 {
    pub id: i64,
    pub topic_id: i64,
    pub title: String,
    pub author_kind: u8,
    pub author_session_id: Option<i64>,
    pub author_office: Option<u8>,
    pub created_at_micros: u64,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ForumAttachmentViewV2 {
    pub artifact_id: i64,
    pub label: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ForumPostViewV2 {
    pub id: i64,
    pub thread_id: i64,
    pub kind: u8,
    pub body: String,
    pub author_kind: u8,
    pub author_session_id: Option<i64>,
    pub author_office: Option<u8>,
    pub reply_to: Option<i64>,
    pub supersedes: Option<i64>,
    pub attachments: Vec<ForumAttachmentViewV2>,
    pub created_at_micros: u64,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ForumSearchHitV2 {
    pub topic_id: i64,
    pub thread_id: i64,
    pub post_id: i64,
    pub kind: u8,
    pub author_office: Option<u8>,
    pub rank_bits: u32,
    pub snippet: String,
    pub topic_name: String,
    pub thread_title: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ForumTopicsResponseV2 {
    pub protocol_version: u16,
    pub request_id: String,
    pub operation: String,
    pub items: Vec<ForumTopicViewV2>,
    pub next_cursor: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ForumThreadsResponseV2 {
    pub protocol_version: u16,
    pub request_id: String,
    pub operation: String,
    pub items: Vec<ForumThreadViewV2>,
    pub next_cursor: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ForumPostsResponseV2 {
    pub protocol_version: u16,
    pub request_id: String,
    pub operation: String,
    pub items: Vec<ForumPostViewV2>,
    pub next_cursor: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ForumSearchResponseV2 {
    pub protocol_version: u16,
    pub request_id: String,
    pub operation: String,
    pub items: Vec<ForumSearchHitV2>,
    pub next_cursor: String,
}

/// Post bodies and metadata are immutable once accepted.  A closed kind is
/// preferable to a free-form string because it keeps search filters and
/// storage discriminants aligned.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub enum ForumPostKind {
    Note,
    Question,
    Finding,
    Proposal,
    Challenge,
    Correction,
    DecisionLink,
}

/// The only authorship identities that can be attributed by the kernel.  An
/// actor's office is derived from its bound connection; it is never supplied
/// in a Forum request payload.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum ForumAuthor {
    Actor {
        session_id: SessionId,
        assignment_role: AssignmentRole,
    },
    GrandArchitect,
}

/// A validated non-empty topic name.
#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct ForumTopicName {
    value: String,
}

impl ForumTopicName {
    pub fn new(value: impl Into<String>) -> Result<Self, ContractError> {
        let value = value.into();
        validate_text(&value, "forum topic name", FORUM_TOPIC_NAME_MAX_BYTES, true)?;
        Ok(Self { value })
    }

    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.value
    }

    #[must_use]
    pub fn into_inner(self) -> String {
        self.value
    }
}

impl fmt::Display for ForumTopicName {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.value)
    }
}

/// A bounded topic description.  Empty descriptions are allowed so a topic
/// can be opened before its durable scope is fully written.
#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct ForumTopicDescription {
    value: String,
}

impl ForumTopicDescription {
    pub fn new(value: impl Into<String>) -> Result<Self, ContractError> {
        let value = value.into();
        validate_text(
            &value,
            "forum topic description",
            FORUM_TOPIC_DESCRIPTION_MAX_BYTES,
            false,
        )?;
        Ok(Self { value })
    }

    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.value
    }

    #[must_use]
    pub fn into_inner(self) -> String {
        self.value
    }
}

/// A validated non-empty thread title.
#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct ForumThreadTitle {
    value: String,
}

impl ForumThreadTitle {
    pub fn new(value: impl Into<String>) -> Result<Self, ContractError> {
        let value = value.into();
        validate_text(
            &value,
            "forum thread title",
            FORUM_THREAD_TITLE_MAX_BYTES,
            true,
        )?;
        Ok(Self { value })
    }

    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.value
    }

    #[must_use]
    pub fn into_inner(self) -> String {
        self.value
    }
}

impl fmt::Display for ForumThreadTitle {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.value)
    }
}

/// An immutable, NUL-free post body.
#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct ForumPostBody {
    value: String,
}

impl ForumPostBody {
    pub fn new(value: impl Into<String>) -> Result<Self, ContractError> {
        let value = value.into();
        validate_text(&value, "forum post body", FORUM_POST_BODY_MAX_BYTES, false)?;
        Ok(Self { value })
    }

    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.value
    }

    #[must_use]
    pub fn into_inner(self) -> String {
        self.value
    }
}

/// A bounded attachment label.  Bytes themselves remain in CAS and are
/// referred to by `ArtifactId`; this value never duplicates their contents.
#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct ForumAttachmentLabel {
    value: String,
}

impl ForumAttachmentLabel {
    pub fn new(value: impl Into<String>) -> Result<Self, ContractError> {
        let value = value.into();
        validate_text(
            &value,
            "forum attachment label",
            FORUM_ATTACHMENT_LABEL_MAX_BYTES,
            false,
        )?;
        Ok(Self { value })
    }

    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.value
    }
}

/// A bounded full-text search input.  PostgreSQL receives this string through
/// `websearch_to_tsquery('simple', ...)`; rejecting NUL and unbalanced quotes
/// here prevents a malformed query from becoming an ambiguous protocol value.
#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct ForumSearchQuery {
    value: String,
}

impl ForumSearchQuery {
    pub fn new(value: impl Into<String>) -> Result<Self, ContractError> {
        let value = value.into();
        validate_text(
            &value,
            "forum search query",
            FORUM_SEARCH_QUERY_MAX_BYTES,
            true,
        )?;
        if value.chars().filter(|character| *character == '"').count() % 2 != 0 {
            return Err(ContractError::InvalidValue {
                field: "forum search query",
                reason: "quoted phrases must be closed",
            });
        }
        Ok(Self { value })
    }

    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.value
    }
}

impl fmt::Display for ForumSearchQuery {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.value)
    }
}

/// A page size that cannot express an unbounded read/search.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct ForumPageLimit(u8);

impl ForumPageLimit {
    pub fn new(value: u8) -> Result<Self, ContractError> {
        if !(1..=FORUM_PAGE_MAX).contains(&value) {
            return Err(ContractError::InvalidValue {
                field: "forum page limit",
                reason: "must be between 1 and 20",
            });
        }
        Ok(Self(value))
    }

    #[must_use]
    pub const fn get(self) -> u8 {
        self.0
    }
}

impl Default for ForumPageLimit {
    fn default() -> Self {
        Self(FORUM_PAGE_MAX)
    }
}

/// A stable search continuation position.  `rank_bits` is the exact IEEE-754
/// representation of PostgreSQL's `ts_rank_cd` result, avoiding decimal
/// rounding when a client resumes a page.  Results sort by descending rank,
/// then ascending post ID; the query uses this pair as its strict seek key.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct ForumSearchCursor {
    pub rank_bits: u32,
    pub post_id: ForumPostId,
}

impl ForumSearchCursor {
    #[must_use]
    pub const fn new(rank_bits: u32, post_id: ForumPostId) -> Self {
        Self { rank_bits, post_id }
    }

    /// Encodes a cursor without adding another dependency to the protocol.
    /// The token is deliberately opaque to SDK callers but deterministic for
    /// retries and durable links.
    #[must_use]
    pub fn encode(self) -> String {
        format!("{:08x}.{}", self.rank_bits, self.post_id.get())
    }

    pub fn decode(value: &str) -> Result<Self, ContractError> {
        if value.is_empty() || value.len() > FORUM_SEARCH_CURSOR_MAX_BYTES {
            return Err(ContractError::InvalidValue {
                field: "forum search cursor",
                reason: "must be non-empty and bounded",
            });
        }
        let (rank, post) = value.split_once('.').ok_or(ContractError::InvalidValue {
            field: "forum search cursor",
            reason: "must contain rank and post ID",
        })?;
        if rank.len() != 8 || !rank.bytes().all(|byte| byte.is_ascii_hexdigit()) {
            return Err(ContractError::InvalidValue {
                field: "forum search cursor",
                reason: "rank must be eight hexadecimal digits",
            });
        }
        let rank_bits = u32::from_str_radix(rank, 16).map_err(|_| ContractError::InvalidValue {
            field: "forum search cursor",
            reason: "rank is out of range",
        })?;
        let rank = f32::from_bits(rank_bits);
        if !rank.is_finite() || rank.is_sign_negative() {
            return Err(ContractError::InvalidValue {
                field: "forum search cursor",
                reason: "rank must be a finite nonnegative value",
            });
        }
        let post_id = post
            .parse::<i64>()
            .map_err(|_| ContractError::InvalidValue {
                field: "forum search cursor",
                reason: "post ID is invalid",
            })?;
        Ok(Self::new(rank_bits, ForumPostId::new(post_id)?))
    }
}

impl FromStr for ForumSearchCursor {
    type Err = ContractError;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        Self::decode(value)
    }
}

/// A bounded snippet returned by search.  It is a derived view, not an
/// authoritative Forum body and is never stored as a row.
#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct ForumSnippet {
    value: String,
}

impl ForumSnippet {
    pub fn new(value: impl Into<String>) -> Result<Self, ContractError> {
        let value = value.into();
        validate_text(&value, "forum snippet", FORUM_SNIPPET_MAX_BYTES, false)?;
        Ok(Self { value })
    }

    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.value
    }
}

/// One search predicate.  All optional filters are applied by the one SQL
/// query; no client-side post-filtering may turn a bounded result into an
/// unbounded read.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ForumSearchInput {
    pub query: ForumSearchQuery,
    pub topic_id: Option<ForumTopicId>,
    pub thread_id: Option<ForumThreadId>,
    pub author_office: Option<AssignmentRole>,
    pub post_kind: Option<ForumPostKind>,
    pub created_after_micros: Option<u64>,
    pub created_before_micros: Option<u64>,
    pub limit: ForumPageLimit,
    pub cursor: Option<ForumSearchCursor>,
}

impl ForumSearchInput {
    #[must_use]
    pub fn new(query: ForumSearchQuery) -> Self {
        Self {
            query,
            topic_id: None,
            thread_id: None,
            author_office: None,
            post_kind: None,
            created_after_micros: None,
            created_before_micros: None,
            limit: ForumPageLimit::default(),
            cursor: None,
        }
    }

    pub fn validate(&self) -> Result<(), ContractError> {
        if let (Some(after), Some(before)) = (self.created_after_micros, self.created_before_micros)
            && after > before
        {
            return Err(ContractError::InvalidValue {
                field: "forum search time range",
                reason: "created_after must not be later than created_before",
            });
        }
        // Constructing the value validates the limit, but retaining this
        // guard documents the invariant if the representation changes.
        ForumPageLimit::new(self.limit.get())?;
        Ok(())
    }
}

/// A read request uses chronological post IDs as its continuation key.  IDs
/// are global, so the same key is stable even when another thread receives a
/// post between two reads.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ForumThreadPage {
    pub thread_id: ForumThreadId,
    pub after_post_id: Option<ForumPostId>,
    pub limit: ForumPageLimit,
}

impl ForumThreadPage {
    #[must_use]
    pub fn new(
        thread_id: ForumThreadId,
        after_post_id: Option<ForumPostId>,
        limit: ForumPageLimit,
    ) -> Self {
        Self {
            thread_id,
            after_post_id,
            limit,
        }
    }
}

/// One attachment relation.  The artifact itself is sealed separately in
/// CAS; this relation only records its identity and human-facing label.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ForumAttachmentInput {
    pub artifact_id: ArtifactId,
    pub label: ForumAttachmentLabel,
}

/// The immutable part of a post creation command.  Reply and supersession
/// references are checked against the target thread and the post ID sequence
/// by the kernel transition, not trusted from a caller assertion.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ForumPostInput {
    pub kind: ForumPostKind,
    pub body: ForumPostBody,
    pub reply_to: Option<ForumPostId>,
    pub supersedes: Option<ForumPostId>,
    pub attachments: Vec<ForumAttachmentInput>,
}

impl ForumPostInput {
    pub fn validate(&self) -> Result<(), ContractError> {
        if self.attachments.len() > FORUM_MAX_ATTACHMENTS_PER_POST {
            return Err(ContractError::InvalidValue {
                field: "forum post attachments",
                reason: "post exceeds the attachment count quota",
            });
        }
        let mut seen_artifacts = BTreeSet::new();
        for attachment in &self.attachments {
            if !seen_artifacts.insert(attachment.artifact_id) {
                return Err(ContractError::InvalidValue {
                    field: "forum post attachments",
                    reason: "an artifact may appear only once on a post",
                });
            }
        }
        if self.reply_to.is_none() && self.supersedes.is_none() {
            return Ok(());
        }
        if self.reply_to == self.supersedes {
            return Err(ContractError::InvalidValue {
                field: "forum post relations",
                reason: "reply and supersession must identify distinct targets",
            });
        }
        Ok(())
    }
}

/// Validates one bounded UTF-8 value.  Rust strings are already UTF-8, so the
/// byte check is intentionally `len`, not character count.
fn validate_text(
    value: &str,
    field: &'static str,
    maximum: usize,
    non_empty: bool,
) -> Result<(), ContractError> {
    if non_empty && value.is_empty() {
        return Err(ContractError::InvalidValue {
            field,
            reason: "must not be empty",
        });
    }
    if value.as_bytes().contains(&0) {
        return Err(ContractError::InvalidValue {
            field,
            reason: "NUL is not allowed",
        });
    }
    if value.len() > maximum {
        return Err(ContractError::ByteLimitExceeded { field, maximum });
    }
    Ok(())
}
