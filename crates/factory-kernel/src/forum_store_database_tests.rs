//! PostgreSQL 18 judges for the executable Forum authority.
//!
//! Every normal setup path uses the typed kernel storage and Forum commands.
//! The sole raw-SQL corruption case lives crate-private in `forum_store.rs`.

use std::{
    fs,
    sync::atomic::{AtomicU64, Ordering},
};

use crate::{
    cas::CasStore,
    forum_store::{ForumStoreError, ForumWriteCounts},
    local_transport::{
        ActorConnectionBinding, ActorConnectionIdentity, LocalDaemon, LocalTransportConfig,
    },
    storage::{InstallKernelBuild, KernelStore, RegisterArtifact, SCHEMA_IDENTITY},
};
use factory_protocol::{
    AggregateRevision, ApplicationRevisionId, ArtifactId, AssignmentId, AssignmentRole, CampaignId,
    ContentDigest, ExpectedRevision, FORUM_SNIPPET_MAX_BYTES, ForumAttachmentInput,
    ForumAttachmentLabel, ForumCreateThreadCommand, ForumCreateThreadInput,
    ForumCreateTopicCommand, ForumCreateTopicInput, ForumMutationIdentity, ForumPageLimit,
    ForumPostBody, ForumPostCommand, ForumPostInput, ForumPostKind, ForumSearchCursor,
    ForumSearchInput, ForumSearchQuery, ForumThreadId, ForumThreadPage, ForumThreadTitle,
    ForumTopicDescription, ForumTopicName, KernelBuildId, SessionId,
};

static NEXT_TEST: AtomicU64 = AtomicU64::new(1);

#[test]
#[ignore = "requires FACTORY_TEST_DATABASE_URL for a disposable PostgreSQL 18 database"]
fn forum_commands_are_audited_idempotent_attributed_and_roll_back_on_rejection() {
    smol::block_on(async {
        let kernel = kernel().await;
        kernel.migrate_and_verify().await.expect("migrate");
        let artifact_id = register_artifact(&kernel).await;
        let forum = kernel.forum_store();
        let binding = binding(&kernel, AssignmentRole::Engineering).await;
        let before = forum.status().await.expect("initial status");

        let create_topic_command = topic_command(before.aggregate_revision, "durable topic");
        let topic = forum
            .create_topic(binding, &create_topic_command)
            .await
            .expect("create topic");
        assert_eq!(
            topic.resulting_revision.get(),
            before.aggregate_revision.get() + 1
        );
        assert!(
            forum
                .create_topic(binding, &create_topic_command)
                .await
                .expect("topic retry")
                .was_idempotent_retry
        );
        let changed = ForumCreateTopicCommand {
            input: ForumCreateTopicInput {
                name: ForumTopicName::new("changed body").unwrap(),
                ..create_topic_command.input.clone()
            },
            ..create_topic_command.clone()
        };
        assert!(matches!(
            forum.create_topic(binding, &changed).await,
            Err(ForumStoreError::IdempotencyConflict)
        ));
        assert!(matches!(
            forum
                .create_topic(
                    binding,
                    &topic_command(before.aggregate_revision, "stale topic")
                )
                .await,
            Err(ForumStoreError::RevisionConflict { .. })
        ));

        let thread = forum
            .create_thread(
                binding,
                &ForumCreateThreadCommand {
                    identity: identity(topic.resulting_revision),
                    input: ForumCreateThreadInput {
                        topic_id: topic.topic_id,
                        title: ForumThreadTitle::new("durable thread").unwrap(),
                    },
                },
            )
            .await
            .expect("create thread");
        let first = forum
            .append_post(
                binding,
                &ForumPostCommand {
                    identity: identity(thread.resulting_revision),
                    thread_id: thread.thread_id,
                    input: ForumPostInput {
                        attachments: vec![ForumAttachmentInput {
                            artifact_id,
                            label: ForumAttachmentLabel::new("evidence").unwrap(),
                        }],
                        ..post("needle evidence", ForumPostKind::Finding)
                    },
                },
            )
            .await
            .expect("first post");
        let second = forum
            .append_post(
                binding,
                &ForumPostCommand {
                    identity: identity(first.resulting_revision),
                    thread_id: thread.thread_id,
                    input: ForumPostInput {
                        reply_to: Some(first.post_id),
                        ..post("needle reply", ForumPostKind::Correction)
                    },
                },
            )
            .await
            .expect("reply post");
        let supersession = forum
            .append_post(
                binding,
                &ForumPostCommand {
                    identity: identity(second.resulting_revision),
                    thread_id: thread.thread_id,
                    input: ForumPostInput {
                        supersedes: Some(second.post_id),
                        ..post("needle supersession", ForumPostKind::Correction)
                    },
                },
            )
            .await
            .expect("same-thread supersession");
        assert!(supersession.post_id > second.post_id);
        let accepted = forum.status().await.expect("accepted counts");
        assert_counts_delta(
            before.counts,
            accepted.counts,
            ForumWriteCounts {
                topic_count: 1,
                thread_count: 1,
                post_count: 3,
                attachment_count: 1,
                audit_count: 5,
            },
        );

        let posts = forum
            .read_thread(ForumThreadPage::new(
                thread.thread_id,
                None,
                ForumPageLimit::new(20).unwrap(),
            ))
            .await
            .expect("chronological read");
        assert_eq!(posts.len(), 3);
        assert_eq!(
            posts[0].author,
            factory_protocol::ForumAuthor::Actor {
                session_id: binding.session_id(),
                assignment_role: binding.assignment_role(),
            }
        );
        assert_eq!(posts[0].attachments[0].artifact_id, artifact_id);
        assert_eq!(posts[1].reply_to, Some(first.post_id));
        assert_eq!(posts[2].supersedes, Some(second.post_id));
        assert_eq!(
            forum
                .list_topics(None, ForumPageLimit::new(20).unwrap())
                .await
                .expect("topic list")
                .iter()
                .find(|value| value.topic_id == topic.topic_id)
                .map(|value| value.name.as_str()),
            Some("durable topic")
        );
        assert!(
            forum
                .list_threads(topic.topic_id, None, ForumPageLimit::new(20).unwrap())
                .await
                .expect("thread list")
                .iter()
                .any(|value| value.thread_id == thread.thread_id)
        );
        let search = ForumSearchInput {
            limit: ForumPageLimit::new(1).unwrap(),
            ..ForumSearchInput::new(ForumSearchQuery::new("needle evidence").unwrap())
        };
        assert_eq!(
            forum.search(&search).await.expect("bounded search").len(),
            1
        );
        assert_eq!(
            forum
                .read_thread(ForumThreadPage::new(
                    thread.thread_id,
                    None,
                    ForumPageLimit::new(1).unwrap(),
                ))
                .await
                .expect("bounded chronological read")
                .len(),
            1
        );
        assert_eq!(forum.status().await.expect("read status"), accepted);

        let oversized_attachments = ForumPostCommand {
            identity: identity(accepted.aggregate_revision),
            thread_id: thread.thread_id,
            input: ForumPostInput {
                attachments: (1..=9)
                    .map(|value| ForumAttachmentInput {
                        artifact_id: ArtifactId::new(value).unwrap(),
                        label: ForumAttachmentLabel::new("quota").unwrap(),
                    })
                    .collect(),
                ..post("must reject nine attachments", ForumPostKind::Note)
            },
        };
        assert!(matches!(
            forum.append_post(binding, &oversized_attachments).await,
            Err(ForumStoreError::Contract(_))
        ));
        assert_eq!(
            forum.status().await.expect("quota rejection status"),
            accepted
        );

        let other_thread = forum
            .create_thread(
                binding,
                &ForumCreateThreadCommand {
                    identity: identity(accepted.aggregate_revision),
                    input: ForumCreateThreadInput {
                        topic_id: topic.topic_id,
                        title: ForumThreadTitle::new("other durable thread").unwrap(),
                    },
                },
            )
            .await
            .expect("other thread");
        let after_other_thread = forum.status().await.expect("other thread status");
        for rejected in [
            ForumPostCommand {
                identity: identity(other_thread.resulting_revision),
                thread_id: ForumThreadId::new(9_999_999).unwrap(),
                input: post("must roll back", ForumPostKind::Note),
            },
            ForumPostCommand {
                identity: identity(other_thread.resulting_revision),
                thread_id: other_thread.thread_id,
                input: ForumPostInput {
                    reply_to: Some(first.post_id),
                    ..post(
                        "must reject cross-thread relation",
                        ForumPostKind::Challenge,
                    )
                },
            },
        ] {
            assert!(matches!(
                forum.append_post(binding, &rejected).await,
                Err(ForumStoreError::Database(_))
            ));
            assert_eq!(
                forum.status().await.expect("rollback status"),
                after_other_thread
            );
        }
        kernel.close().await;
    });
}

#[test]
#[ignore = "requires FACTORY_TEST_DATABASE_URL for a disposable PostgreSQL 18 database"]
fn forum_read_search_and_index_plan_are_bounded_and_zero_write() {
    smol::block_on(async {
        let kernel = kernel().await;
        kernel.migrate_and_verify().await.expect("migrate");
        let forum = kernel.forum_store();
        let before = forum.status().await.expect("before");
        let plan = forum
            .post_search_plan("needle")
            .await
            .expect("read-only GIN plan");
        assert!(
            plan.iter()
                .any(|line| line.contains("forum_posts_search_gin")),
            "expected post GIN index plan, got {plan:?}"
        );
        assert!(
            forum
                .read_thread(ForumThreadPage::new(
                    ForumThreadId::new(9_999_999).unwrap(),
                    None,
                    ForumPageLimit::new(1).unwrap(),
                ))
                .await
                .expect("empty bounded read")
                .is_empty()
        );
        assert!(
            forum
                .search(&ForumSearchInput {
                    limit: ForumPageLimit::new(1).unwrap(),
                    ..ForumSearchInput::new(ForumSearchQuery::new("missing needle").unwrap())
                })
                .await
                .expect("bounded missing search")
                .is_empty()
        );
        // Reads/searches/browse pages are receipt-free and do not maintain a
        // mutable activity projection. Repeat every bounded read shape so
        // that invariant is observable at the PostgreSQL row boundary.
        for _ in 0..100 {
            forum
                .read_thread(ForumThreadPage::new(
                    ForumThreadId::new(9_999_999).unwrap(),
                    None,
                    ForumPageLimit::new(1).unwrap(),
                ))
                .await
                .expect("repeated bounded read");
            forum
                .search(&ForumSearchInput {
                    limit: ForumPageLimit::new(1).unwrap(),
                    ..ForumSearchInput::new(
                        ForumSearchQuery::new("missing repeated needle").unwrap(),
                    )
                })
                .await
                .expect("repeated bounded search");
            forum
                .list_topics(None, ForumPageLimit::new(1).unwrap())
                .await
                .expect("repeated topic browse");
            forum
                .list_threads(
                    factory_protocol::ForumTopicId::new(9_999_999).unwrap(),
                    None,
                    ForumPageLimit::new(1).unwrap(),
                )
                .await
                .expect("repeated thread browse");
        }
        assert_eq!(forum.status().await.expect("after"), before);
        kernel.close().await;
    });
}

#[test]
#[ignore = "requires FACTORY_TEST_DATABASE_URL for a disposable PostgreSQL 18 database"]
fn forum_search_cursor_and_snippet_are_stable_after_activity_changes() {
    smol::block_on(async {
        let kernel = kernel().await;
        kernel.migrate_and_verify().await.expect("migrate");
        let forum = kernel.forum_store();
        let binding = binding(&kernel, AssignmentRole::Engineering).await;
        let revision = forum.status().await.expect("status").aggregate_revision;
        let topic = forum
            .create_topic(binding, &topic_command(revision, "search topic needle"))
            .await
            .expect("topic");
        let thread = forum
            .create_thread(
                binding,
                &ForumCreateThreadCommand {
                    identity: identity(topic.resulting_revision),
                    input: ForumCreateThreadInput {
                        topic_id: topic.topic_id,
                        title: ForumThreadTitle::new("search thread needle").unwrap(),
                    },
                },
            )
            .await
            .expect("thread");

        let long_body = format!("needle {}", "é".repeat(800));
        let first_post = forum
            .append_post(
                binding,
                &ForumPostCommand {
                    identity: identity(thread.resulting_revision),
                    thread_id: thread.thread_id,
                    input: post(&long_body, ForumPostKind::Finding),
                },
            )
            .await
            .expect("multibyte post");
        let second_post = forum
            .append_post(
                binding,
                &ForumPostCommand {
                    identity: identity(first_post.resulting_revision),
                    thread_id: thread.thread_id,
                    input: post("needle stable second", ForumPostKind::Note),
                },
            )
            .await
            .expect("second post");
        let third_post = forum
            .append_post(
                binding,
                &ForumPostCommand {
                    identity: identity(second_post.resulting_revision),
                    thread_id: thread.thread_id,
                    input: post("needle stable third", ForumPostKind::Question),
                },
            )
            .await
            .expect("third post");

        // Build a deliberately nontrivial indexed corpus through the typed
        // mutation authority. The GIN judge below must exercise a selective
        // relation, not merely prove an index exists on an empty table. The
        // unique two-term marker appears in exactly 16 of 1,024 ordinary
        // records so the order-independent query check is meaningful while
        // the common `needle` corpus remains useful for cursor coverage.
        const SCALE_CORPUS_POSTS: u16 = 1_024;
        let mut corpus_revision = third_post.resulting_revision;
        for index in 0..SCALE_CORPUS_POSTS {
            let body = if index % 64 == 0 {
                format!("needle corpus quasar1729 swan selective item {index}")
            } else {
                format!("needle corpus ordinary evidence item {index}")
            };
            let corpus_post = forum
                .append_post(
                    binding,
                    &ForumPostCommand {
                        identity: identity(corpus_revision),
                        thread_id: thread.thread_id,
                        input: post(&body, ForumPostKind::Note),
                    },
                )
                .await
                .expect("corpus post");
            corpus_revision = corpus_post.resulting_revision;
        }

        // `websearch_to_tsquery('simple', ...)` treats ordinary unquoted
        // terms as an order-independent conjunction. The hit identities, not
        // ranks, are the contract: reversing the terms must neither lose nor
        // add a selective match.
        let selective = |query: &str| ForumSearchInput {
            limit: ForumPageLimit::new(20).unwrap(),
            ..ForumSearchInput::new(ForumSearchQuery::new(query).unwrap())
        };
        let forward = forum
            .search(&selective("quasar1729 swan"))
            .await
            .expect("forward selective search");
        let reversed = forum
            .search(&selective("swan quasar1729"))
            .await
            .expect("reversed selective search");
        let forward_ids: Vec<_> = forward.iter().map(|hit| hit.post_id).collect();
        let reversed_ids: Vec<_> = reversed.iter().map(|hit| hit.post_id).collect();
        assert_eq!(forward_ids, reversed_ids);
        assert_eq!(forward.len(), usize::from(SCALE_CORPUS_POSTS / 64));

        let plan = forum
            .post_search_plan("quasar1729")
            .await
            .expect("selective GIN plan");
        assert!(
            plan.iter()
                .any(|line| line.contains("forum_posts_search_gin")),
            "expected post GIN index plan, got {plan:?}"
        );

        let mut search = ForumSearchInput {
            limit: ForumPageLimit::new(2).unwrap(),
            topic_id: Some(topic.topic_id),
            post_kind: Some(ForumPostKind::Finding),
            ..ForumSearchInput::new(ForumSearchQuery::new("needle").unwrap())
        };
        let long_hit = forum.search(&search).await.expect("long search");
        assert_eq!(long_hit.len(), 1);
        assert!(long_hit[0].snippet.len() <= FORUM_SNIPPET_MAX_BYTES);
        assert!(
            long_hit[0]
                .snippet
                .is_char_boundary(long_hit[0].snippet.len())
        );

        search.post_kind = None;
        let first_page = forum.search(&search).await.expect("first search page");
        assert_eq!(first_page.len(), 2);
        let cursor_hit = first_page.last().expect("cursor row");
        search.cursor = Some(ForumSearchCursor::new(
            cursor_hit.rank_bits,
            cursor_hit.post_id,
        ));
        let second_page = forum.search(&search).await.expect("second search page");
        assert!(second_page.iter().all(|candidate| {
            candidate.rank_bits < cursor_hit.rank_bits
                || (candidate.rank_bits == cursor_hit.rank_bits
                    && candidate.post_id > cursor_hit.post_id)
        }));
        assert!(second_page.iter().all(|candidate| {
            !first_page
                .iter()
                .any(|previous| previous.post_id == candidate.post_id)
        }));

        // Appending activity does not rewrite the thread row. The ID cursor
        // therefore cannot skip or repeat a row after a later post arrives.
        let first_thread_page = forum
            .list_threads(topic.topic_id, None, ForumPageLimit::new(1).unwrap())
            .await
            .expect("first thread page");
        assert_eq!(first_thread_page.len(), 1);
        let activity_revision = corpus_revision;
        forum
            .append_post(
                binding,
                &ForumPostCommand {
                    identity: identity(activity_revision),
                    thread_id: thread.thread_id,
                    input: post("activity does not reorder", ForumPostKind::Note),
                },
            )
            .await
            .expect("activity post");
        let following_threads = forum
            .list_threads(
                topic.topic_id,
                Some(first_thread_page[0].thread_id),
                ForumPageLimit::new(1).unwrap(),
            )
            .await
            .expect("following thread page");
        assert!(
            following_threads
                .iter()
                .all(|candidate| { candidate.thread_id > first_thread_page[0].thread_id })
        );
        kernel.close().await;
    });
}

fn post(body: &str, kind: ForumPostKind) -> ForumPostInput {
    ForumPostInput {
        kind,
        body: ForumPostBody::new(body).unwrap(),
        reply_to: None,
        supersedes: None,
        attachments: vec![],
    }
}

async fn kernel() -> KernelStore {
    KernelStore::connect(&test_database_url())
        .await
        .expect("connect disposable PostgreSQL database")
}

async fn register_artifact(kernel: &KernelStore) -> ArtifactId {
    let root = std::env::temp_dir().join(unique("forum-cas"));
    let staging = root.join("staging");
    fs::create_dir_all(&staging).expect("CAS staging root");
    let cas = CasStore::new_with_seed(root.join("runtime"), 4096, unique_number()).expect("CAS");
    fs::write(staging.join("qualification"), b"qualified kernel build")
        .expect("qualification evidence");
    let qualification_receipt = cas
        .adopt(&staging, "qualification")
        .expect("seal qualification evidence");
    let expected_revision = kernel
        .kernel_build_status()
        .await
        .expect("kernel status")
        .aggregate_revision;
    let build = InstallKernelBuild {
        principal: "operator".to_owned(),
        command_id: unique("install-build"),
        expected_revision: ExpectedRevision::new(expected_revision),
        build_id: KernelBuildId::new(digest(unique_number())),
        source_digest: digest(unique_number()),
        binary_digest: digest(unique_number()),
        schema_identity: SCHEMA_IDENTITY.to_owned(),
        host_executable_path: "/opt/factory/factory-tea-host".to_owned(),
        core_head: "9".repeat(40),
        rust_toolchain: "nightly-2026-07-24".to_owned(),
        core_source_digest: digest(unique_number()),
        qualification_receipt,
    };
    let build = kernel
        .install_kernel_build(&cas, &build)
        .await
        .expect("install build");
    fs::write(staging.join("artifact"), b"forum artifact").expect("artifact source");
    let sealed = cas.adopt(&staging, "artifact").expect("seal artifact");
    let artifact = kernel
        .register_artifact(
            &cas,
            &RegisterArtifact {
                principal: "operator".to_owned(),
                command_id: unique("register-artifact"),
                expected_kernel_build_revision: ExpectedRevision::new(build.resulting_revision),
                kernel_build_id: build.kernel_build_id,
                sealed,
            },
        )
        .await
        .expect("register artifact")
        .artifact_id;
    let _ = fs::remove_dir_all(root);
    artifact
}

async fn binding(kernel: &KernelStore, assignment_role: AssignmentRole) -> ActorConnectionBinding {
    // Unix-domain socket paths have a small platform limit. Keep this test
    // runtime root intentionally short; the semantic identity still comes
    // from the kernel-created socket binding, never from the path.
    let runtime_root = std::env::temp_dir().join(format!("f{}", unique_number()));
    let daemon = LocalDaemon::bind(LocalTransportConfig::new(runtime_root.clone()), kernel)
        .await
        .expect("daemon transport binding");
    let (actor, server) = daemon
        .create_actor_socketpair(ActorConnectionIdentity::from_admitted_assignment(
            SessionId::new(1).unwrap(),
            AssignmentId::new(1).unwrap(),
            ApplicationRevisionId::new(1).unwrap(),
            CampaignId::new(1).unwrap(),
            assignment_role,
        ))
        .expect("daemon-created actor descriptor");
    let binding = server.binding();
    drop(actor);
    drop(server);
    daemon.shutdown().await.expect("release daemon singleton");
    let _ = std::fs::remove_file(runtime_root.join("factoryd.lock"));
    let _ = std::fs::remove_dir(runtime_root);
    binding
}

fn identity(revision: AggregateRevision) -> ForumMutationIdentity {
    ForumMutationIdentity::new(unique("command"), revision).unwrap()
}

fn topic_command(revision: AggregateRevision, name: &str) -> ForumCreateTopicCommand {
    ForumCreateTopicCommand {
        identity: identity(revision),
        input: ForumCreateTopicInput {
            name: ForumTopicName::new(name).unwrap(),
            description: ForumTopicDescription::new("durable shared record").unwrap(),
        },
    }
}

fn test_database_url() -> String {
    let url = std::env::var("FACTORY_TEST_DATABASE_URL")
        .expect("FACTORY_TEST_DATABASE_URL must name a disposable PostgreSQL 18 database");
    let database_name = url
        .rsplit('/')
        .next()
        .and_then(|part| part.split('?').next())
        .expect("database URL has a final path component");
    assert!(
        database_name.strip_prefix("factory_test_v3_").is_some_and(
            |suffix| !suffix.is_empty() && suffix.bytes().all(|byte| byte.is_ascii_digit())
        ),
        "FACTORY_TEST_DATABASE_URL must name exactly factory_test_v3_<digits>"
    );
    url
}

fn unique(prefix: &str) -> String {
    format!("{prefix}-{}", unique_number())
}

fn unique_number() -> u64 {
    (u64::from(std::process::id()) << 32) | NEXT_TEST.fetch_add(1, Ordering::Relaxed)
}

fn digest(serial: u64) -> ContentDigest {
    let mut bytes = [0; 32];
    for chunk in bytes.as_chunks_mut::<8>().0 {
        chunk.copy_from_slice(&serial.to_be_bytes());
    }
    ContentDigest::from_bytes(bytes)
}

fn assert_counts_delta(
    before: ForumWriteCounts,
    after: ForumWriteCounts,
    expected: ForumWriteCounts,
) {
    assert_eq!(after.topic_count - before.topic_count, expected.topic_count);
    assert_eq!(
        after.thread_count - before.thread_count,
        expected.thread_count
    );
    assert_eq!(after.post_count - before.post_count, expected.post_count);
    assert_eq!(
        after.attachment_count - before.attachment_count,
        expected.attachment_count
    );
    assert_eq!(after.audit_count - before.audit_count, expected.audit_count);
}
