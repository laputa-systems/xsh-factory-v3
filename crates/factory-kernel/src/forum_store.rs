//! PostgreSQL authority for permanent Forum records.
//!
//! This module is the only Forum authority and durable write surface: callers bind an
//! actor identity once, submit closed commands, and receive an audit-backed
//! receipt. It never exposes its pool to actors, SDK code, or tests.

use std::collections::BTreeMap;

use factory_protocol::{
    AggregateRevision, ArtifactId, AssignmentRole, AuditLogId, ContractError,
    FORUM_SNIPPET_MAX_BYTES, ForumAuthor, ForumCreateThreadCommand, ForumCreateTopicCommand,
    ForumPageLimit, ForumPostCommand, ForumPostId, ForumPostKind, ForumSearchInput, ForumThreadId,
    ForumThreadPage, ForumTopicId,
};
use sqlx::{PgPool, Postgres};
use thiserror::Error;

use crate::local_transport::ActorConnectionBinding;

const FORUM_ADVISORY_LOCK_KEY: i64 = 0x4656_335f_464f_5255;
// Keep Forum receipts in their own durable subject family.  Process custody
// uses 4/5/6 for campaigns, assignments, and sessions respectively; sharing
// those numbers made an audit subject ambiguous during restore verification.
const FORUM_TOPIC_SUBJECT: i16 = 10;
const FORUM_THREAD_SUBJECT: i16 = 11;
const FORUM_POST_SUBJECT: i16 = 12;
const CREATE_TOPIC_OPERATION: &str = "forum.topic.create";
const CREATE_THREAD_OPERATION: &str = "forum.thread.create";
const POST_OPERATION: &str = "forum.post.append";
const SUPERSEDE_TOPIC_OPERATION: &str = "forum.topic.supersede";
const SUPERSEDE_THREAD_OPERATION: &str = "forum.thread.supersede";

/// The physical PostgreSQL owner for Forum transitions and bounded reads.
#[derive(Clone, Debug)]
pub struct ForumStore {
    pool: PgPool,
}

/// Capability minted only by the kernel's operator transport. The private
/// marker prevents an actor payload or an application crate from constructing
/// Grand Architect authority by value. The operator socket adapter should
/// obtain this token from its kernel-owned dispatch path, then pass it to the
/// `*_with_authority` methods below.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct OperatorForumCapability {
    _private: (),
}

impl OperatorForumCapability {
    /// This constructor is crate-visible by design. A transport adapter in the
    /// kernel crate may mint the capability after authenticating the operator
    /// socket; actor/session bindings cannot call it.
    pub(crate) const fn from_operator_transport() -> Self {
        Self { _private: () }
    }
}

/// Forum mutation authority is either the inherited actor socket binding or a
/// kernel-minted operator capability. No JSON field can select either branch.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ForumAuthority {
    Actor(ActorConnectionBinding),
    GrandArchitect(OperatorForumCapability),
}

/// Store-owned topic supersession command. The protocol may add a richer wire
/// shape later; a supersession cannot be smuggled through ordinary creation.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ForumSupersedeTopicCommand {
    pub identity: factory_protocol::ForumMutationIdentity,
    pub supersedes_topic_id: ForumTopicId,
    pub name: factory_protocol::ForumTopicName,
    pub description: factory_protocol::ForumTopicDescription,
}

/// Store-owned thread supersession command. The replacement must remain under
/// the same topic as its historical target.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ForumSupersedeThreadCommand {
    pub identity: factory_protocol::ForumMutationIdentity,
    pub topic_id: ForumTopicId,
    pub supersedes_thread_id: ForumThreadId,
    pub title: factory_protocol::ForumThreadTitle,
}

impl ForumStore {
    /// Reuses the kernel's sole fixed PostgreSQL pool. Only
    /// [`crate::storage::KernelStore`] constructs this value after opening the
    /// database URL; Forum callers never receive a second pool or URL surface.
    pub(crate) fn from_kernel_pool(pool: PgPool) -> Self {
        Self { pool }
    }

    /// Kernel transport hook for the external Grand Architect. The returned
    /// capability is intentionally not constructible by actor/application
    /// crates. Parent daemon wiring should call this only for the operator
    /// socket, never for an actor descriptor.
    pub(crate) const fn operator_capability() -> OperatorForumCapability {
        OperatorForumCapability::from_operator_transport()
    }

    /// Returns the aggregate revision implied by accepted Forum audit receipts.
    /// This is a global append-only Forum sequence, protected by the matching
    /// transaction advisory lock on mutation; it needs no mutable content row.
    pub async fn status(&self) -> Result<ForumStatus, ForumStoreError> {
        let counts = self.write_counts().await?;
        Ok(ForumStatus {
            aggregate_revision: current_forum_revision(&self.pool).await?,
            counts,
        })
    }

    /// Counts durable Forum rows and Forum-owned audit receipts without
    /// creating a read receipt. This exists for operator status and zero-write
    /// integration judges, not as a general query escape hatch.
    pub async fn write_counts(&self) -> Result<ForumWriteCounts, ForumStoreError> {
        let row = sqlx::query!(
            "SELECT
                (SELECT count(*)::BIGINT FROM factory.forum_topics) AS \"topic_count!\",
                (SELECT count(*)::BIGINT FROM factory.forum_threads) AS \"thread_count!\",
                (SELECT count(*)::BIGINT FROM factory.forum_posts) AS \"post_count!\",
                (SELECT count(*)::BIGINT FROM factory.forum_attachments) AS \"attachment_count!\",
                (SELECT count(*)::BIGINT FROM factory.audit_log
                    WHERE operation IN ($1, $2, $3, $4, $5)) AS \"audit_count!\"",
            CREATE_TOPIC_OPERATION,
            CREATE_THREAD_OPERATION,
            POST_OPERATION,
            SUPERSEDE_TOPIC_OPERATION,
            SUPERSEDE_THREAD_OPERATION,
        )
        .fetch_one(&self.pool)
        .await?;
        Ok(ForumWriteCounts {
            topic_count: u64::try_from(row.topic_count)
                .map_err(|_| ForumStoreError::CountOutOfRange)?,
            thread_count: u64::try_from(row.thread_count)
                .map_err(|_| ForumStoreError::CountOutOfRange)?,
            post_count: u64::try_from(row.post_count)
                .map_err(|_| ForumStoreError::CountOutOfRange)?,
            attachment_count: u64::try_from(row.attachment_count)
                .map_err(|_| ForumStoreError::CountOutOfRange)?,
            audit_count: u64::try_from(row.audit_count)
                .map_err(|_| ForumStoreError::CountOutOfRange)?,
        })
    }

    /// Creates an immutable Forum topic and exactly one audit receipt.
    pub async fn create_topic(
        &self,
        binding: ActorConnectionBinding,
        command: &ForumCreateTopicCommand,
    ) -> Result<ForumTopicReceipt, ForumStoreError> {
        self.create_topic_with_authority(ForumAuthority::Actor(binding), command)
            .await
    }

    /// Creates a topic under either inherited actor authority or a
    /// kernel-minted Grand Architect capability.
    pub async fn create_topic_with_authority(
        &self,
        authority: ForumAuthority,
        command: &ForumCreateTopicCommand,
    ) -> Result<ForumTopicReceipt, ForumStoreError> {
        let principal = authority_principal(authority);
        let fingerprint = topic_fingerprint(authority, command);
        let mut transaction = self.pool.begin().await?;
        lock_forum(&mut transaction).await?;
        if let Some(receipt) = find_idempotent(
            &mut transaction,
            &principal,
            command.identity.client_command_id(),
            CREATE_TOPIC_OPERATION,
            fingerprint,
        )
        .await?
        {
            require_subject(&receipt, FORUM_TOPIC_SUBJECT)?;
            transaction.commit().await?;
            return Ok(ForumTopicReceipt {
                topic_id: ForumTopicId::new(receipt.subject_id)?,
                resulting_revision: receipt.resulting_revision,
                audit_log_id: AuditLogId::new(receipt.audit_log_id)?,
                was_idempotent_retry: true,
            });
        }
        let current = current_forum_revision(&mut *transaction).await?;
        require_expected(command.identity.expected_revision(), current)?;
        let (author_kind, author_session_id, author_office) = authority_author_columns(authority);
        let topic_id = sqlx::query_scalar!(
            "INSERT INTO factory.forum_topics (
                 author_kind, author_session_id, author_office, name, description
             ) VALUES ($1, $2, $3, $4, $5)
             RETURNING id",
            author_kind,
            author_session_id,
            author_office,
            command.input.name.as_str(),
            command.input.description.as_str(),
        )
        .fetch_one(&mut *transaction)
        .await?;
        let resulting_revision = current.next().map_err(ForumStoreError::from)?;
        let audit_log_id = insert_receipt(
            &mut transaction,
            &principal,
            command.identity.client_command_id(),
            CREATE_TOPIC_OPERATION,
            fingerprint,
            FORUM_TOPIC_SUBJECT,
            topic_id,
            resulting_revision,
        )
        .await?;
        transaction.commit().await?;
        Ok(ForumTopicReceipt {
            topic_id: ForumTopicId::new(topic_id)?,
            resulting_revision,
            audit_log_id: AuditLogId::new(audit_log_id)?,
            was_idempotent_retry: false,
        })
    }

    /// Creates an immutable thread below an existing topic and appends one
    /// audit receipt in the same transaction.
    pub async fn create_thread(
        &self,
        binding: ActorConnectionBinding,
        command: &ForumCreateThreadCommand,
    ) -> Result<ForumThreadReceipt, ForumStoreError> {
        self.create_thread_with_authority(ForumAuthority::Actor(binding), command)
            .await
    }

    /// Creates a thread under either inherited actor authority or a
    /// kernel-minted Grand Architect capability.
    pub async fn create_thread_with_authority(
        &self,
        authority: ForumAuthority,
        command: &ForumCreateThreadCommand,
    ) -> Result<ForumThreadReceipt, ForumStoreError> {
        let principal = authority_principal(authority);
        let fingerprint = thread_fingerprint(authority, command);
        let mut transaction = self.pool.begin().await?;
        lock_forum(&mut transaction).await?;
        if let Some(receipt) = find_idempotent(
            &mut transaction,
            &principal,
            command.identity.client_command_id(),
            CREATE_THREAD_OPERATION,
            fingerprint,
        )
        .await?
        {
            require_subject(&receipt, FORUM_THREAD_SUBJECT)?;
            transaction.commit().await?;
            return Ok(ForumThreadReceipt {
                thread_id: ForumThreadId::new(receipt.subject_id)?,
                resulting_revision: receipt.resulting_revision,
                audit_log_id: AuditLogId::new(receipt.audit_log_id)?,
                was_idempotent_retry: true,
            });
        }
        let current = current_forum_revision(&mut *transaction).await?;
        require_expected(command.identity.expected_revision(), current)?;
        let (author_kind, author_session_id, author_office) = authority_author_columns(authority);
        let thread_id = sqlx::query_scalar!(
            "INSERT INTO factory.forum_threads (
                 topic_id, author_kind, author_session_id, author_office, title
             ) VALUES ($1, $2, $3, $4, $5)
             RETURNING id",
            command.input.topic_id.get(),
            author_kind,
            author_session_id,
            author_office,
            command.input.title.as_str(),
        )
        .fetch_one(&mut *transaction)
        .await?;
        let resulting_revision = current.next().map_err(ForumStoreError::from)?;
        let audit_log_id = insert_receipt(
            &mut transaction,
            &principal,
            command.identity.client_command_id(),
            CREATE_THREAD_OPERATION,
            fingerprint,
            FORUM_THREAD_SUBJECT,
            thread_id,
            resulting_revision,
        )
        .await?;
        transaction.commit().await?;
        Ok(ForumThreadReceipt {
            thread_id: ForumThreadId::new(thread_id)?,
            resulting_revision,
            audit_log_id: AuditLogId::new(audit_log_id)?,
            was_idempotent_retry: false,
        })
    }

    /// Appends a replacement topic while retaining the historical topic
    /// bytes. The target is checked in the same transaction and the new row
    /// receives one distinct audit receipt.
    pub async fn supersede_topic(
        &self,
        binding: ActorConnectionBinding,
        command: &ForumSupersedeTopicCommand,
    ) -> Result<ForumTopicReceipt, ForumStoreError> {
        self.supersede_topic_with_authority(ForumAuthority::Actor(binding), command)
            .await
    }

    pub async fn supersede_topic_with_authority(
        &self,
        authority: ForumAuthority,
        command: &ForumSupersedeTopicCommand,
    ) -> Result<ForumTopicReceipt, ForumStoreError> {
        let principal = authority_principal(authority);
        let fingerprint = supersede_topic_fingerprint(authority, command);
        let mut transaction = self.pool.begin().await?;
        lock_forum(&mut transaction).await?;
        if let Some(receipt) = find_idempotent(
            &mut transaction,
            &principal,
            command.identity.client_command_id(),
            SUPERSEDE_TOPIC_OPERATION,
            fingerprint,
        )
        .await?
        {
            require_subject(&receipt, FORUM_TOPIC_SUBJECT)?;
            transaction.commit().await?;
            return Ok(ForumTopicReceipt {
                topic_id: ForumTopicId::new(receipt.subject_id)?,
                resulting_revision: receipt.resulting_revision,
                audit_log_id: AuditLogId::new(receipt.audit_log_id)?,
                was_idempotent_retry: true,
            });
        }
        let current = current_forum_revision(&mut *transaction).await?;
        require_expected(command.identity.expected_revision(), current)?;
        let target_exists = sqlx::query_scalar!(
            "SELECT EXISTS(SELECT 1 FROM factory.forum_topics WHERE id = $1) AS \"exists!\"",
            command.supersedes_topic_id.get(),
        )
        .fetch_one(&mut *transaction)
        .await?;
        if !target_exists {
            return Err(ForumStoreError::SupersessionTargetMissing);
        }
        let (author_kind, author_session_id, author_office) = authority_author_columns(authority);
        let topic_id = sqlx::query_scalar!(
            "INSERT INTO factory.forum_topics (
                 author_kind, author_session_id, author_office, name, description,
                 supersedes_topic_id
             ) VALUES ($1, $2, $3, $4, $5, $6)
             RETURNING id",
            author_kind,
            author_session_id,
            author_office,
            command.name.as_str(),
            command.description.as_str(),
            command.supersedes_topic_id.get(),
        )
        .fetch_one(&mut *transaction)
        .await?;
        let resulting_revision = current.next().map_err(ForumStoreError::from)?;
        let audit_log_id = insert_receipt(
            &mut transaction,
            &principal,
            command.identity.client_command_id(),
            SUPERSEDE_TOPIC_OPERATION,
            fingerprint,
            FORUM_TOPIC_SUBJECT,
            topic_id,
            resulting_revision,
        )
        .await?;
        transaction.commit().await?;
        Ok(ForumTopicReceipt {
            topic_id: ForumTopicId::new(topic_id)?,
            resulting_revision,
            audit_log_id: AuditLogId::new(audit_log_id)?,
            was_idempotent_retry: false,
        })
    }

    /// Appends a replacement thread and requires the historical target to be
    /// under the same topic. This prevents cross-topic supersession graphs.
    pub async fn supersede_thread(
        &self,
        binding: ActorConnectionBinding,
        command: &ForumSupersedeThreadCommand,
    ) -> Result<ForumThreadReceipt, ForumStoreError> {
        self.supersede_thread_with_authority(ForumAuthority::Actor(binding), command)
            .await
    }

    pub async fn supersede_thread_with_authority(
        &self,
        authority: ForumAuthority,
        command: &ForumSupersedeThreadCommand,
    ) -> Result<ForumThreadReceipt, ForumStoreError> {
        let principal = authority_principal(authority);
        let fingerprint = supersede_thread_fingerprint(authority, command);
        let mut transaction = self.pool.begin().await?;
        lock_forum(&mut transaction).await?;
        if let Some(receipt) = find_idempotent(
            &mut transaction,
            &principal,
            command.identity.client_command_id(),
            SUPERSEDE_THREAD_OPERATION,
            fingerprint,
        )
        .await?
        {
            require_subject(&receipt, FORUM_THREAD_SUBJECT)?;
            transaction.commit().await?;
            return Ok(ForumThreadReceipt {
                thread_id: ForumThreadId::new(receipt.subject_id)?,
                resulting_revision: receipt.resulting_revision,
                audit_log_id: AuditLogId::new(receipt.audit_log_id)?,
                was_idempotent_retry: true,
            });
        }
        let current = current_forum_revision(&mut *transaction).await?;
        require_expected(command.identity.expected_revision(), current)?;
        let target_topic = sqlx::query_scalar!(
            "SELECT topic_id FROM factory.forum_threads WHERE id = $1",
            command.supersedes_thread_id.get(),
        )
        .fetch_optional(&mut *transaction)
        .await?
        .ok_or(ForumStoreError::SupersessionTargetMissing)?;
        if target_topic != command.topic_id.get() {
            return Err(ForumStoreError::SupersessionParentMismatch);
        }
        let (author_kind, author_session_id, author_office) = authority_author_columns(authority);
        let thread_id = sqlx::query_scalar!(
            "INSERT INTO factory.forum_threads (
                 topic_id, author_kind, author_session_id, author_office, title,
                 supersedes_thread_id
             ) VALUES ($1, $2, $3, $4, $5, $6)
             RETURNING id",
            command.topic_id.get(),
            author_kind,
            author_session_id,
            author_office,
            command.title.as_str(),
            command.supersedes_thread_id.get(),
        )
        .fetch_one(&mut *transaction)
        .await?;
        let resulting_revision = current.next().map_err(ForumStoreError::from)?;
        let audit_log_id = insert_receipt(
            &mut transaction,
            &principal,
            command.identity.client_command_id(),
            SUPERSEDE_THREAD_OPERATION,
            fingerprint,
            FORUM_THREAD_SUBJECT,
            thread_id,
            resulting_revision,
        )
        .await?;
        transaction.commit().await?;
        Ok(ForumThreadReceipt {
            thread_id: ForumThreadId::new(thread_id)?,
            resulting_revision,
            audit_log_id: AuditLogId::new(audit_log_id)?,
            was_idempotent_retry: false,
        })
    }

    /// Appends one immutable post plus its bounded attachment relations and one
    /// audit receipt. Any FK, relation, or attachment failure rolls back all
    /// three kinds of write together.
    pub async fn append_post(
        &self,
        binding: ActorConnectionBinding,
        command: &ForumPostCommand,
    ) -> Result<ForumPostReceipt, ForumStoreError> {
        self.append_post_with_authority(ForumAuthority::Actor(binding), command)
            .await
    }

    /// Appends a post under either inherited actor authority or a
    /// kernel-minted Grand Architect capability.
    pub async fn append_post_with_authority(
        &self,
        authority: ForumAuthority,
        command: &ForumPostCommand,
    ) -> Result<ForumPostReceipt, ForumStoreError> {
        command.input.validate()?;
        let principal = authority_principal(authority);
        let fingerprint = post_fingerprint(authority, command);
        let mut transaction = self.pool.begin().await?;
        lock_forum(&mut transaction).await?;
        if let Some(receipt) = find_idempotent(
            &mut transaction,
            &principal,
            command.identity.client_command_id(),
            POST_OPERATION,
            fingerprint,
        )
        .await?
        {
            require_subject(&receipt, FORUM_POST_SUBJECT)?;
            transaction.commit().await?;
            return Ok(ForumPostReceipt {
                post_id: ForumPostId::new(receipt.subject_id)?,
                resulting_revision: receipt.resulting_revision,
                audit_log_id: AuditLogId::new(receipt.audit_log_id)?,
                was_idempotent_retry: true,
            });
        }
        let current = current_forum_revision(&mut *transaction).await?;
        require_expected(command.identity.expected_revision(), current)?;
        let (author_kind, author_session_id, author_office) = authority_author_columns(authority);
        let post_id = sqlx::query_scalar!(
            "INSERT INTO factory.forum_posts (
                 thread_id, author_kind, author_session_id, author_office, body,
                 kind, reply_to_post_id, supersedes_post_id
             ) VALUES ($1, $2, $3, $4, $5, $6, $7, $8)
             RETURNING id",
            command.thread_id.get(),
            author_kind,
            author_session_id,
            author_office,
            command.input.body.as_str(),
            post_kind_number(command.input.kind),
            command.input.reply_to.map(ForumPostId::get),
            command.input.supersedes.map(ForumPostId::get),
        )
        .fetch_one(&mut *transaction)
        .await?;
        for attachment in &command.input.attachments {
            sqlx::query!(
                "INSERT INTO factory.forum_attachments (post_id, artifact_id, label)
                 VALUES ($1, $2, $3)",
                post_id,
                attachment.artifact_id.get(),
                attachment.label.as_str(),
            )
            .execute(&mut *transaction)
            .await?;
        }
        let resulting_revision = current.next().map_err(ForumStoreError::from)?;
        let audit_log_id = insert_receipt(
            &mut transaction,
            &principal,
            command.identity.client_command_id(),
            POST_OPERATION,
            fingerprint,
            FORUM_POST_SUBJECT,
            post_id,
            resulting_revision,
        )
        .await?;
        transaction.commit().await?;
        Ok(ForumPostReceipt {
            post_id: ForumPostId::new(post_id)?,
            resulting_revision,
            audit_log_id: AuditLogId::new(audit_log_id)?,
            was_idempotent_retry: false,
        })
    }

    /// Reads a bounded chronological page of immutable post records, including
    /// bounded attachment relations. It performs no mutation and derives
    /// continuation from global post IDs.
    pub async fn read_thread(
        &self,
        page: ForumThreadPage,
    ) -> Result<Vec<ForumPostView>, ForumStoreError> {
        let after_post_id = page.after_post_id.map_or(0, ForumPostId::get);
        let rows = sqlx::query!(
            "SELECT id, thread_id, author_kind, author_session_id, author_office,
                    body, kind, reply_to_post_id, supersedes_post_id,
                    floor(extract(epoch FROM created_at) * 1000000)::BIGINT
                        AS \"created_at_micros!\"
             FROM factory.forum_posts
             WHERE thread_id = $1 AND id > $2
             ORDER BY id ASC
             LIMIT $3",
            page.thread_id.get(),
            after_post_id,
            i64::from(page.limit.get()),
        )
        .fetch_all(&self.pool)
        .await?;
        if rows.is_empty() {
            return Ok(Vec::new());
        }
        let post_ids = rows.iter().map(|row| row.id).collect::<Vec<_>>();
        let attachment_rows = sqlx::query!(
            "SELECT post_id, artifact_id, label
             FROM factory.forum_attachments
             WHERE post_id = ANY($1)
             ORDER BY post_id ASC, artifact_id ASC",
            &post_ids[..],
        )
        .fetch_all(&self.pool)
        .await?;
        let mut attachments = BTreeMap::<i64, Vec<ForumAttachmentView>>::new();
        for attachment in attachment_rows {
            attachments
                .entry(attachment.post_id)
                .or_default()
                .push(ForumAttachmentView {
                    artifact_id: ArtifactId::new(attachment.artifact_id)?,
                    label: attachment.label,
                });
        }
        rows.into_iter()
            .map(|row| {
                Ok(ForumPostView {
                    post_id: ForumPostId::new(row.id)?,
                    thread_id: ForumThreadId::new(row.thread_id)?,
                    author: author_from_columns(
                        row.author_kind,
                        row.author_session_id,
                        row.author_office,
                    )?,
                    body: row.body,
                    kind: post_kind_from_number(row.kind)?,
                    reply_to: row.reply_to_post_id.map(ForumPostId::new).transpose()?,
                    supersedes: row.supersedes_post_id.map(ForumPostId::new).transpose()?,
                    attachments: attachments.remove(&row.id).unwrap_or_default(),
                    created_at_micros: u64::try_from(row.created_at_micros)
                        .map_err(|_| ForumStoreError::TimestampOutOfRange)?,
                })
            })
            .collect()
    }

    /// Lists topics in immutable creation order. The cursor is the topic ID
    /// and therefore matches the ordering exactly; it remains stable when a
    /// later post changes derived activity. The parent topic row is never
    /// updated by a post append, so this remains a zero-write browse.
    pub async fn list_topics(
        &self,
        after_topic_id: Option<ForumTopicId>,
        limit: ForumPageLimit,
    ) -> Result<Vec<ForumTopicView>, ForumStoreError> {
        let rows = sqlx::query!(
            "SELECT t.id, t.author_kind, t.author_session_id, t.author_office,
                    t.name, t.description,
                    floor(extract(epoch FROM t.created_at) * 1000000)::BIGINT
                        AS \"created_at_micros!\"
             FROM factory.forum_topics AS t
             WHERE ($1::bigint IS NULL OR t.id > $1)
             ORDER BY t.id ASC
             LIMIT $2",
            after_topic_id.map(ForumTopicId::get),
            i64::from(limit.get()),
        )
        .fetch_all(&self.pool)
        .await?;
        rows.into_iter()
            .map(|row| {
                Ok(ForumTopicView {
                    topic_id: ForumTopicId::new(row.id)?,
                    author: author_from_columns(
                        row.author_kind,
                        row.author_session_id,
                        row.author_office,
                    )?,
                    name: row.name,
                    description: row.description,
                    created_at_micros: u64::try_from(row.created_at_micros)
                        .map_err(|_| ForumStoreError::TimestampOutOfRange)?,
                })
            })
            .collect()
    }

    /// Lists threads in immutable creation order. The thread ID cursor and
    /// ascending order are a single lexicographic contract, so no row can be
    /// skipped or repeated when posts are appended between pages.
    pub async fn list_threads(
        &self,
        topic_id: ForumTopicId,
        after_thread_id: Option<ForumThreadId>,
        limit: ForumPageLimit,
    ) -> Result<Vec<ForumThreadView>, ForumStoreError> {
        let rows = sqlx::query!(
            "SELECT th.id, th.topic_id, th.author_kind, th.author_session_id,
                    th.author_office, th.title,
                    floor(extract(epoch FROM th.created_at) * 1000000)::BIGINT
                        AS \"created_at_micros!\"
             FROM factory.forum_threads AS th
             WHERE th.topic_id = $1
               AND ($2::bigint IS NULL OR th.id > $2)
             ORDER BY th.id ASC
             LIMIT $3",
            topic_id.get(),
            after_thread_id.map(ForumThreadId::get),
            i64::from(limit.get()),
        )
        .fetch_all(&self.pool)
        .await?;
        rows.into_iter()
            .map(|row| {
                Ok(ForumThreadView {
                    thread_id: ForumThreadId::new(row.id)?,
                    topic_id: ForumTopicId::new(row.topic_id)?,
                    author: author_from_columns(
                        row.author_kind,
                        row.author_session_id,
                        row.author_office,
                    )?,
                    title: row.title,
                    created_at_micros: u64::try_from(row.created_at_micros)
                        .map_err(|_| ForumStoreError::TimestampOutOfRange)?,
                })
            })
            .collect()
    }

    /// Executes the one bounded, order-independent full-text search. The
    /// query applies every filter in SQL and returns global post IDs as its
    /// stable continuation tie-breaker.
    pub async fn search(
        &self,
        input: &ForumSearchInput,
    ) -> Result<Vec<ForumSearchHit>, ForumStoreError> {
        input.validate()?;
        let cursor_rank = input.cursor.map(|cursor| f32::from_bits(cursor.rank_bits));
        let cursor_post_id = input.cursor.map(|cursor| cursor.post_id.get());
        let created_after = micros_to_i64(input.created_after_micros)?;
        let created_before = micros_to_i64(input.created_before_micros)?;
        let rows = sqlx::query!(
            "WITH search AS (
                SELECT websearch_to_tsquery('simple', $1::text) AS query
             ), matches AS (
                SELECT t.id AS topic_id, th.id AS thread_id, p.id AS post_id,
                       ts_rank_cd(p.search_vector, search.query) AS rank,
                       0::smallint AS source_precedence,
                       p.body AS matched_text
                FROM factory.forum_posts AS p
                JOIN factory.forum_threads AS th ON th.id = p.thread_id
                JOIN factory.forum_topics AS t ON t.id = th.topic_id
                CROSS JOIN search
                WHERE p.search_vector @@ search.query
                UNION ALL
                SELECT t.id, th.id, p.id, ts_rank_cd(th.search_vector, search.query),
                       1::smallint, th.title
                FROM factory.forum_threads AS th
                JOIN factory.forum_topics AS t ON t.id = th.topic_id
                JOIN factory.forum_posts AS p ON p.thread_id = th.id
                CROSS JOIN search
                WHERE th.search_vector @@ search.query
                UNION ALL
                SELECT t.id, th.id, p.id, ts_rank_cd(t.search_vector, search.query),
                       2::smallint, t.name || ' — ' || t.description
                FROM factory.forum_topics AS t
                JOIN factory.forum_threads AS th ON th.topic_id = t.id
                JOIN factory.forum_posts AS p ON p.thread_id = th.id
                CROSS JOIN search
                WHERE t.search_vector @@ search.query
             ), best AS (
                SELECT DISTINCT ON (post_id)
                       topic_id, thread_id, post_id, rank, source_precedence, matched_text
                FROM matches
                ORDER BY post_id, rank DESC, source_precedence ASC
             )
             SELECT best.topic_id AS \"topic_id!\", best.thread_id AS \"thread_id!\",
                    best.post_id AS \"post_id!\", p.kind AS \"kind!\", p.author_office,
                    best.rank AS \"rank!\",
                    ts_headline(
                      'simple', best.matched_text, search.query,
                      'MaxWords=80, MinWords=12, MaxFragments=2, StartSel=<mark>, StopSel=</mark>'
                    ) AS \"snippet!\",
                    t.name AS \"topic_name!\", th.title AS \"thread_title!\"
             FROM best
             JOIN factory.forum_posts AS p ON p.id = best.post_id
             JOIN factory.forum_threads AS th ON th.id = best.thread_id
             JOIN factory.forum_topics AS t ON t.id = best.topic_id
             CROSS JOIN search
             WHERE ($2::bigint IS NULL OR best.topic_id = $2)
               AND ($3::bigint IS NULL OR best.thread_id = $3)
               AND ($4::smallint IS NULL OR p.author_office = $4)
               AND ($5::smallint IS NULL OR p.kind = $5)
               AND ($6::bigint IS NULL OR p.created_at >= to_timestamp($6::double precision / 1000000.0))
               AND ($7::bigint IS NULL OR p.created_at < to_timestamp($7::double precision / 1000000.0))
               AND ($8::real IS NULL OR best.rank < $8 OR (best.rank = $8 AND best.post_id > $9))
             ORDER BY best.rank DESC, best.post_id ASC
             LIMIT $10",
            input.query.as_str(),
            input.topic_id.map(ForumTopicId::get),
            input.thread_id.map(ForumThreadId::get),
            input.author_office.map(office_number),
            input.post_kind.map(post_kind_number),
            created_after,
            created_before,
            cursor_rank,
            cursor_post_id,
            i64::from(input.limit.get()),
        )
        .fetch_all(&self.pool)
        .await?;
        rows.into_iter()
            .map(|row| {
                Ok(ForumSearchHit {
                    topic_id: ForumTopicId::new(row.topic_id)?,
                    thread_id: ForumThreadId::new(row.thread_id)?,
                    post_id: ForumPostId::new(row.post_id)?,
                    kind: post_kind_from_number(row.kind)?,
                    author_office: row.author_office.map(office_from_number).transpose()?,
                    rank_bits: row.rank.to_bits(),
                    snippet: truncate_utf8(row.snippet, FORUM_SNIPPET_MAX_BYTES),
                    topic_name: row.topic_name,
                    thread_title: row.thread_title,
                })
            })
            .collect()
    }

    /// A bounded read-only diagnostic used by the PostgreSQL 18 index judge.
    /// It cannot execute arbitrary SQL and leaves no row or audit receipt.
    pub async fn post_search_plan(&self, query: &str) -> Result<Vec<String>, ForumStoreError> {
        let mut transaction = self.pool.begin().await?;
        sqlx::query!("SET LOCAL enable_seqscan = off")
            .execute(&mut *transaction)
            .await?;
        let plan = sqlx::query_scalar!(
            "EXPLAIN (COSTS OFF)
             SELECT id FROM factory.forum_posts
             WHERE search_vector @@ websearch_to_tsquery('simple', $1::text)",
            query,
        )
        .fetch_all(&mut *transaction)
        .await?;
        transaction.commit().await?;
        Ok(plan.into_iter().flatten().collect())
    }
}

/// Read-only durable Forum status.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ForumStatus {
    pub aggregate_revision: AggregateRevision,
    pub counts: ForumWriteCounts,
}

/// Bounded durable row counts used for status and write-amplification judges.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct ForumWriteCounts {
    pub topic_count: u64,
    pub thread_count: u64,
    pub post_count: u64,
    pub attachment_count: u64,
    pub audit_count: u64,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ForumTopicReceipt {
    pub topic_id: ForumTopicId,
    pub resulting_revision: AggregateRevision,
    pub audit_log_id: AuditLogId,
    pub was_idempotent_retry: bool,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ForumThreadReceipt {
    pub thread_id: ForumThreadId,
    pub resulting_revision: AggregateRevision,
    pub audit_log_id: AuditLogId,
    pub was_idempotent_retry: bool,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ForumPostReceipt {
    pub post_id: ForumPostId,
    pub resulting_revision: AggregateRevision,
    pub audit_log_id: AuditLogId,
    pub was_idempotent_retry: bool,
}

/// Immutable data returned by bounded topic browse.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ForumTopicView {
    pub topic_id: ForumTopicId,
    pub author: ForumAuthor,
    pub name: String,
    pub description: String,
    pub created_at_micros: u64,
}

/// Immutable data returned by bounded thread browse.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ForumThreadView {
    pub thread_id: ForumThreadId,
    pub topic_id: ForumTopicId,
    pub author: ForumAuthor,
    pub title: String,
    pub created_at_micros: u64,
}

/// One immutable attachment relation returned with a chronological post.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ForumAttachmentView {
    pub artifact_id: ArtifactId,
    pub label: String,
}

/// Immutable data returned by bounded chronological thread reads.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ForumPostView {
    pub post_id: ForumPostId,
    pub thread_id: ForumThreadId,
    pub author: ForumAuthor,
    pub body: String,
    pub kind: ForumPostKind,
    pub reply_to: Option<ForumPostId>,
    pub supersedes: Option<ForumPostId>,
    pub attachments: Vec<ForumAttachmentView>,
    pub created_at_micros: u64,
}

/// A bounded Forum search row. The rank bits are used verbatim to make the
/// follow-up cursor independent of decimal rendering.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ForumSearchHit {
    pub topic_id: ForumTopicId,
    pub thread_id: ForumThreadId,
    pub post_id: ForumPostId,
    pub kind: ForumPostKind,
    pub author_office: Option<AssignmentRole>,
    pub rank_bits: u32,
    pub snippet: String,
    pub topic_name: String,
    pub thread_title: String,
}

#[derive(Debug, Error)]
pub enum ForumStoreError {
    #[error("invalid database URL: {source}")]
    InvalidDatabaseUrl { source: sqlx::Error },

    #[error(transparent)]
    Database(#[from] sqlx::Error),

    #[error(transparent)]
    Contract(#[from] ContractError),

    #[error("Forum principal must use 1 through 160 ASCII letters, digits, '.', ':', '_', or '-'")]
    InvalidPrincipal,

    #[error("Forum expected revision {expected:?} does not match current {current:?}")]
    RevisionConflict {
        expected: AggregateRevision,
        current: AggregateRevision,
    },

    #[error("Forum command ID is already associated with a different command")]
    IdempotencyConflict,

    #[error("Forum audit receipt has an unexpected subject kind")]
    AuditSubjectKindMismatch,

    #[error("Forum SQL count cannot be represented as u64")]
    CountOutOfRange,

    #[error("Forum revision cannot be represented by PostgreSQL BIGINT")]
    RevisionOutOfRange,

    #[error("Forum timestamp cannot be represented by PostgreSQL BIGINT microseconds")]
    TimestampOutOfRange,

    #[error("Forum row contains an unknown office discriminant {0}")]
    UnknownOffice(i16),

    #[error("Forum row contains an unknown post-kind discriminant {0}")]
    UnknownPostKind(i16),

    #[error("Forum row contains an invalid author identity")]
    InvalidStoredAuthor,

    #[error("Forum supersession target does not exist")]
    SupersessionTargetMissing,

    #[error("Forum thread supersession target belongs to another topic")]
    SupersessionParentMismatch,
}

#[derive(Clone, Copy)]
struct StoredReceipt {
    audit_log_id: i64,
    subject_kind: i16,
    subject_id: i64,
    resulting_revision: AggregateRevision,
}

async fn lock_forum(
    transaction: &mut sqlx::Transaction<'_, Postgres>,
) -> Result<(), ForumStoreError> {
    sqlx::query!("SELECT pg_advisory_xact_lock($1)", FORUM_ADVISORY_LOCK_KEY)
        .execute(&mut **transaction)
        .await?;
    Ok(())
}

async fn current_forum_revision<'e, E>(executor: E) -> Result<AggregateRevision, ForumStoreError>
where
    E: sqlx::Executor<'e, Database = Postgres>,
{
    let revision = sqlx::query_scalar!(
        "SELECT COALESCE(MAX(resulting_revision), 0)::BIGINT AS \"revision!\"
         FROM factory.audit_log
         WHERE operation IN ($1, $2, $3, $4, $5)",
        CREATE_TOPIC_OPERATION,
        CREATE_THREAD_OPERATION,
        POST_OPERATION,
        SUPERSEDE_TOPIC_OPERATION,
        SUPERSEDE_THREAD_OPERATION,
    )
    .fetch_one(executor)
    .await?;
    aggregate_revision(revision)
}

async fn find_idempotent(
    transaction: &mut sqlx::Transaction<'_, Postgres>,
    principal: &str,
    command_id: &str,
    operation: &'static str,
    fingerprint: [u8; 32],
) -> Result<Option<StoredReceipt>, ForumStoreError> {
    let row = sqlx::query!(
        "SELECT id, operation, command_fingerprint, subject_kind, subject_id, resulting_revision
         FROM factory.audit_log
         WHERE principal = $1 AND command_id = $2",
        principal,
        command_id,
    )
    .fetch_optional(&mut **transaction)
    .await?;
    let Some(row) = row else {
        return Ok(None);
    };
    if row.operation != operation || row.command_fingerprint.as_slice() != fingerprint {
        return Err(ForumStoreError::IdempotencyConflict);
    }
    Ok(Some(StoredReceipt {
        audit_log_id: row.id,
        subject_kind: row.subject_kind,
        subject_id: row.subject_id,
        resulting_revision: aggregate_revision(row.resulting_revision)?,
    }))
}

async fn insert_receipt(
    transaction: &mut sqlx::Transaction<'_, Postgres>,
    principal: &str,
    command_id: &str,
    operation: &'static str,
    fingerprint: [u8; 32],
    subject_kind: i16,
    subject_id: i64,
    resulting_revision: AggregateRevision,
) -> Result<i64, ForumStoreError> {
    Ok(sqlx::query_scalar!(
        "INSERT INTO factory.audit_log (
             principal, command_id, operation, command_fingerprint,
             subject_kind, subject_id, resulting_revision
         ) VALUES ($1, $2, $3, $4, $5, $6, $7)
         RETURNING id",
        principal,
        command_id,
        operation,
        &fingerprint[..],
        subject_kind,
        subject_id,
        i64::try_from(resulting_revision.get()).map_err(|_| ForumStoreError::RevisionOutOfRange)?,
    )
    .fetch_one(&mut **transaction)
    .await?)
}

fn require_subject(receipt: &StoredReceipt, expected: i16) -> Result<(), ForumStoreError> {
    if receipt.subject_kind == expected {
        Ok(())
    } else {
        Err(ForumStoreError::AuditSubjectKindMismatch)
    }
}

fn require_expected(
    expected: AggregateRevision,
    current: AggregateRevision,
) -> Result<(), ForumStoreError> {
    if expected == current {
        Ok(())
    } else {
        Err(ForumStoreError::RevisionConflict { expected, current })
    }
}

fn aggregate_revision(value: i64) -> Result<AggregateRevision, ForumStoreError> {
    let value = u64::try_from(value).map_err(|_| ForumStoreError::RevisionOutOfRange)?;
    Ok(AggregateRevision::from_persisted(value))
}

fn authority_author_columns(authority: ForumAuthority) -> (i16, Option<i64>, Option<i16>) {
    match authority {
        ForumAuthority::Actor(binding) => (
            0,
            Some(binding.session_id().get()),
            Some(office_number(binding.assignment_role())),
        ),
        ForumAuthority::GrandArchitect(_) => (1, None, None),
    }
}

fn author_from_columns(
    author_kind: i16,
    author_session_id: Option<i64>,
    author_office: Option<i16>,
) -> Result<ForumAuthor, ForumStoreError> {
    match (author_kind, author_session_id, author_office) {
        (0, Some(session_id), Some(office)) => Ok(ForumAuthor::Actor {
            session_id: factory_protocol::SessionId::new(session_id)?,
            assignment_role: office_from_number(office)?,
        }),
        (1, None, None) => Ok(ForumAuthor::GrandArchitect),
        _ => Err(ForumStoreError::InvalidStoredAuthor),
    }
}

fn actor_principal(binding: ActorConnectionBinding) -> String {
    // This deterministic principal derives solely from the complete accepted
    // socket binding. Keeping assignment/application/campaign in the key
    // prevents a reused session number from sharing a retry namespace across
    // distinct admitted actor occurrences.
    format!(
        "actor-session-{}-assignment-{}-application-{}-campaign-{}",
        binding.session_id().get(),
        binding.assignment_id().get(),
        binding.application_revision_id().get(),
        binding.campaign_id().get(),
    )
}

fn authority_principal(authority: ForumAuthority) -> String {
    match authority {
        ForumAuthority::Actor(binding) => actor_principal(binding),
        ForumAuthority::GrandArchitect(_) => "grand-architect".to_owned(),
    }
}

const fn office_number(assignment_role: AssignmentRole) -> i16 {
    match assignment_role {
        AssignmentRole::ProductResearch => 0,
        AssignmentRole::Engineering => 1,
        AssignmentRole::Quality => 2,
    }
}

fn office_from_number(value: i16) -> Result<AssignmentRole, ForumStoreError> {
    match value {
        0 => Ok(AssignmentRole::ProductResearch),
        1 => Ok(AssignmentRole::Engineering),
        2 => Ok(AssignmentRole::Quality),
        _ => Err(ForumStoreError::UnknownOffice(value)),
    }
}

const fn post_kind_number(kind: ForumPostKind) -> i16 {
    match kind {
        ForumPostKind::Note => 0,
        ForumPostKind::Question => 1,
        ForumPostKind::Finding => 2,
        ForumPostKind::Proposal => 3,
        ForumPostKind::Challenge => 4,
        ForumPostKind::Correction => 5,
        ForumPostKind::DecisionLink => 6,
    }
}

fn post_kind_from_number(value: i16) -> Result<ForumPostKind, ForumStoreError> {
    match value {
        0 => Ok(ForumPostKind::Note),
        1 => Ok(ForumPostKind::Question),
        2 => Ok(ForumPostKind::Finding),
        3 => Ok(ForumPostKind::Proposal),
        4 => Ok(ForumPostKind::Challenge),
        5 => Ok(ForumPostKind::Correction),
        6 => Ok(ForumPostKind::DecisionLink),
        _ => Err(ForumStoreError::UnknownPostKind(value)),
    }
}

fn micros_to_i64(value: Option<u64>) -> Result<Option<i64>, ForumStoreError> {
    value
        .map(|value| i64::try_from(value).map_err(|_| ForumStoreError::TimestampOutOfRange))
        .transpose()
}

/// Truncates a derived `ts_headline` at a UTF-8 boundary. PostgreSQL's
/// MaxWords/MaxFragments options bound words, not bytes, so the kernel applies
/// the final byte quota before a snippet crosses the protocol boundary.
fn truncate_utf8(value: String, maximum: usize) -> String {
    if value.len() <= maximum {
        return value;
    }
    let mut end = maximum;
    while end > 0 && !value.is_char_boundary(end) {
        end -= 1;
    }
    value[..end].to_owned()
}

fn topic_fingerprint(authority: ForumAuthority, command: &ForumCreateTopicCommand) -> [u8; 32] {
    let mut hasher = command_hasher(
        authority,
        CREATE_TOPIC_OPERATION,
        command.identity.client_command_id(),
        command.identity.expected_revision(),
    );
    hash_text(&mut hasher, command.input.name.as_str());
    hash_text(&mut hasher, command.input.description.as_str());
    *hasher.finalize().as_bytes()
}

fn thread_fingerprint(authority: ForumAuthority, command: &ForumCreateThreadCommand) -> [u8; 32] {
    let mut hasher = command_hasher(
        authority,
        CREATE_THREAD_OPERATION,
        command.identity.client_command_id(),
        command.identity.expected_revision(),
    );
    hasher.update(&command.input.topic_id.get().to_be_bytes());
    hash_text(&mut hasher, command.input.title.as_str());
    *hasher.finalize().as_bytes()
}

fn post_fingerprint(authority: ForumAuthority, command: &ForumPostCommand) -> [u8; 32] {
    let mut hasher = command_hasher(
        authority,
        POST_OPERATION,
        command.identity.client_command_id(),
        command.identity.expected_revision(),
    );
    hasher.update(&command.thread_id.get().to_be_bytes());
    hasher.update(&post_kind_number(command.input.kind).to_be_bytes());
    hash_text(&mut hasher, command.input.body.as_str());
    hash_optional_i64(&mut hasher, command.input.reply_to.map(ForumPostId::get));
    hash_optional_i64(&mut hasher, command.input.supersedes.map(ForumPostId::get));
    hasher.update(&(command.input.attachments.len() as u64).to_be_bytes());
    for attachment in &command.input.attachments {
        hasher.update(&attachment.artifact_id.get().to_be_bytes());
        hash_text(&mut hasher, attachment.label.as_str());
    }
    *hasher.finalize().as_bytes()
}

fn supersede_topic_fingerprint(
    authority: ForumAuthority,
    command: &ForumSupersedeTopicCommand,
) -> [u8; 32] {
    let mut hasher = command_hasher(
        authority,
        SUPERSEDE_TOPIC_OPERATION,
        command.identity.client_command_id(),
        command.identity.expected_revision(),
    );
    hasher.update(&command.supersedes_topic_id.get().to_be_bytes());
    hash_text(&mut hasher, command.name.as_str());
    hash_text(&mut hasher, command.description.as_str());
    *hasher.finalize().as_bytes()
}

fn supersede_thread_fingerprint(
    authority: ForumAuthority,
    command: &ForumSupersedeThreadCommand,
) -> [u8; 32] {
    let mut hasher = command_hasher(
        authority,
        SUPERSEDE_THREAD_OPERATION,
        command.identity.client_command_id(),
        command.identity.expected_revision(),
    );
    hasher.update(&command.topic_id.get().to_be_bytes());
    hasher.update(&command.supersedes_thread_id.get().to_be_bytes());
    hash_text(&mut hasher, command.title.as_str());
    *hasher.finalize().as_bytes()
}

fn command_hasher(
    authority: ForumAuthority,
    operation: &'static str,
    command_id: &str,
    expected_revision: AggregateRevision,
) -> blake3::Hasher {
    let mut hasher = blake3::Hasher::new();
    hash_text(&mut hasher, operation);
    hash_text(&mut hasher, &authority_principal(authority));
    hash_text(&mut hasher, command_id);
    hasher.update(&expected_revision.get().to_be_bytes());
    let (kind, session, office) = authority_author_columns(authority);
    hasher.update(&kind.to_be_bytes());
    hash_optional_i64(&mut hasher, session);
    hasher.update(&office.unwrap_or(-1).to_be_bytes());
    hasher
}

fn hash_text(hasher: &mut blake3::Hasher, value: &str) {
    hasher.update(&(value.len() as u64).to_be_bytes());
    hasher.update(value.as_bytes());
}

fn hash_optional_i64(hasher: &mut blake3::Hasher, value: Option<i64>) {
    match value {
        Some(value) => {
            hasher.update(&[1]);
            hasher.update(&value.to_be_bytes());
        }
        None => {
            hasher.update(&[0]);
        }
    }
}

#[cfg(test)]
mod forum_read_contract_tests {
    use super::truncate_utf8;
    use factory_protocol::FORUM_SNIPPET_MAX_BYTES;

    #[test]
    fn snippet_truncation_never_splits_utf8() {
        let source = "needle ".to_owned() + &"é".repeat(FORUM_SNIPPET_MAX_BYTES);
        let snippet = truncate_utf8(source, FORUM_SNIPPET_MAX_BYTES);
        assert!(snippet.len() <= FORUM_SNIPPET_MAX_BYTES);
        assert!(snippet.is_char_boundary(snippet.len()));
        assert!(snippet.starts_with("needle "));
    }
}

// Raw SQL is intentionally confined to this crate-private corruption judge.
// Public integration tests seed and exercise Forum only through typed kernel
// commands; this test alone proves PostgreSQL rejects an attempted bypass of
// the immutable-row contract.
#[cfg(test)]
mod immutable_row_database_test {
    use super::*;
    use crate::storage::KernelStore;
    use factory_protocol::{
        ApplicationRevisionId, AssignmentId, CampaignId, ForumCreateTopicInput,
        ForumMutationIdentity, ForumTopicDescription, ForumTopicName,
    };

    #[test]
    #[ignore = "requires FACTORY_TEST_DATABASE_URL for a disposable PostgreSQL 18 database"]
    fn update_and_delete_of_a_forum_topic_are_rejected_by_postgresql() {
        smol::block_on(async {
            let url =
                std::env::var("FACTORY_TEST_DATABASE_URL").expect("FACTORY_TEST_DATABASE_URL");
            let database_name = url
                .rsplit('/')
                .next()
                .and_then(|part| part.split('?').next())
                .expect("database name");
            assert!(
                database_name
                    .strip_prefix("factory_test_v3_")
                    .is_some_and(|suffix| !suffix.is_empty()
                        && suffix.bytes().all(|byte| byte.is_ascii_digit()))
            );
            let kernel = KernelStore::connect(&url).await.expect("connect");
            kernel.migrate_and_verify().await.expect("migrate");
            let forum = kernel.forum_store();
            let revision = forum.status().await.expect("status").aggregate_revision;
            let binding = ActorConnectionBinding::from_identity(
                crate::local_transport::ActorConnectionIdentity::from_admitted_assignment(
                    factory_protocol::SessionId::new(1).unwrap(),
                    AssignmentId::new(1).unwrap(),
                    ApplicationRevisionId::new(1).unwrap(),
                    CampaignId::new(1).unwrap(),
                    AssignmentRole::Quality,
                ),
            );
            let command = ForumCreateTopicCommand {
                identity: ForumMutationIdentity::new(
                    format!("immutable-trigger-{}", std::process::id()),
                    revision,
                )
                .unwrap(),
                input: ForumCreateTopicInput {
                    name: ForumTopicName::new("immutable database topic").unwrap(),
                    description: ForumTopicDescription::new("").unwrap(),
                },
            };
            let topic = forum.create_topic(binding, &command).await.expect("topic");
            assert!(
                sqlx::query!(
                    "UPDATE factory.forum_topics SET name = $1 WHERE id = $2",
                    "forbidden edit",
                    topic.topic_id.get(),
                )
                .execute(&forum.pool)
                .await
                .is_err()
            );
            assert!(
                sqlx::query!(
                    "DELETE FROM factory.forum_topics WHERE id = $1",
                    topic.topic_id.get(),
                )
                .execute(&forum.pool)
                .await
                .is_err()
            );
            kernel.close().await;
        });
    }
}
