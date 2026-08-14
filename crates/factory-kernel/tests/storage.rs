//! PostgreSQL 18 integration judges for the first authority transitions.
//!
//! The test target accepts only an explicitly supplied, already-created
//! disposable database. All test setup proceeds through kernel commands; it
//! does not expose a raw pool or bypass durable authority to seed rows.

use std::fs;
use std::sync::atomic::{AtomicU64, Ordering};

use factory_kernel::cas::CasStore;
use factory_kernel::process::StartCampaign;
use factory_kernel::scheduler::{ClaimReadyTicketAction, SchedulerNextAction};
use factory_kernel::storage::{
    ActivateApplicationRevision, AdmitCompiledApplication, InstallKernelBuild, KernelStore,
    RegisterArtifact, RegisterRepository, SCHEMA_IDENTITY, StoreError,
};
use factory_kernel::ticket_store::{
    ClaimOutcome, ClaimSponsoredTicket, CurrentHeadRequalification, FailTicketAttempt,
    ReleaseOutcome, ReleaseTicketAttempt, SponsorTicketRevision, SubmitTicketProposal,
};
use factory_protocol::{
    AggregateRevision, ApplicationKey, ArchitectPrincipalV1, ContentDigest, ExpectedRevision,
    MicroUsd, SealedArtifactReferenceV1,
};

static NEXT_TEST: AtomicU64 = AtomicU64::new(1);

#[test]
#[ignore = "requires FACTORY_TEST_DATABASE_URL for a disposable PostgreSQL 18 database"]
fn migration_identity_and_status_reads_are_provider_free_and_idempotent() {
    smol::block_on(async {
        let store = store().await;
        store.migrate_and_verify().await.expect("migrate");
        store
            .migrate_and_verify()
            .await
            .expect("idempotent migrate");
        store.verify_schema_identity().await.expect("identity");
        let inspection = sqlx::PgPool::connect(&test_database_url())
            .await
            .expect("connect schema inspection pool");
        let migration_count: i64 = sqlx::query_scalar("SELECT count(*) FROM _sqlx_migrations")
            .fetch_one(&inspection)
            .await
            .expect("canonical migration count");
        assert_eq!(
            migration_count, 3,
            "fresh V3 applies the canonical base and two additive authority migrations"
        );
        let table_count: i64 = sqlx::query_scalar(
            "SELECT count(*)
             FROM information_schema.tables
             WHERE table_schema = 'factory' AND table_type = 'BASE TABLE'",
        )
        .fetch_one(&inspection)
        .await
        .expect("Factory table count");
        assert_eq!(
            table_count, 21,
            "the schema adds one durable-office relation to the original authority tables"
        );
        inspection.close().await;
        let before = store.kernel_build_status().await.expect("read-only status");
        let application = store
            .application_status(&ApplicationKey::parse(unique("read")).expect("key"))
            .await
            .expect("read-only application status");
        assert_eq!(application.aggregate_revision, AggregateRevision::initial());
        assert_eq!(store.kernel_build_status().await.expect("status"), before);
        store.close().await;
    });
}

#[test]
#[ignore = "requires FACTORY_TEST_DATABASE_URL for a disposable PostgreSQL 18 database"]
fn install_register_and_seal_transitions_are_idempotent_and_guarded() {
    smol::block_on(async {
        let store = store().await;
        store.migrate_and_verify().await.expect("migrate");
        let build = install_build(&store).await;
        let build_status = store.kernel_build_status().await.expect("build status");
        assert_eq!(
            build_status.current_kernel_build_id,
            Some(build.kernel_build_id)
        );
        assert_eq!(
            build_status.aggregate_revision,
            build.receipt.resulting_revision
        );

        let retry = store
            .install_kernel_build(&build.cas, &build.command)
            .await
            .expect("exact install retry");
        assert!(retry.was_idempotent_retry);
        assert_eq!(retry.audit_log_id, build.receipt.audit_log_id);

        let stale = InstallKernelBuild {
            command_id: unique("stale-build"),
            ..build.command.clone()
        };
        assert!(matches!(
            store.install_kernel_build(&build.cas, &stale).await,
            Err(StoreError::RevisionConflict { .. })
        ));

        let repository_command = RegisterRepository {
            principal: "architect".to_owned(),
            command_id: unique("register-repository"),
            expected_revision: ExpectedRevision::new(AggregateRevision::initial()),
            repository_key: unique("repository"),
            canonical_local_path: format!("/tmp/{}", unique("product")),
            default_branch: "main".to_owned(),
        };
        let repository = store
            .register_repository(&repository_command)
            .await
            .expect("register repository");
        assert!(
            store
                .register_repository(&repository_command)
                .await
                .expect("repository retry")
                .was_idempotent_retry
        );
        let changed_repository = RegisterRepository {
            default_branch: "trunk".to_owned(),
            ..repository_command.clone()
        };
        assert!(matches!(
            store.register_repository(&changed_repository).await,
            Err(StoreError::IdempotencyConflict { .. })
        ));

        let artifact_command = artifact_command(&build, "artifact");
        let artifact = store
            .register_artifact(&build.cas, &artifact_command)
            .await
            .expect("register artifact");
        assert!(
            store
                .register_artifact(&build.cas, &artifact_command)
                .await
                .expect("artifact retry")
                .was_idempotent_retry
        );
        let conflicting_artifact = RegisterArtifact {
            command_id: unique("artifact-conflict"),
            ..artifact_command
        };
        assert!(
            store
                .register_artifact(&build.cas, &conflicting_artifact)
                .await
                .expect("physical duplicate reuse")
                .was_reused
        );
        assert!(repository.repository_id.get() > 0);
        assert!(artifact.artifact_id.get() > 0);
        store.close().await;
    });
}

#[test]
#[ignore = "requires FACTORY_TEST_DATABASE_URL for a disposable PostgreSQL 18 database"]
fn offline_build_installation_can_advance_the_current_qualified_build() {
    smol::block_on(async {
        let store = store().await;
        store.migrate_and_verify().await.expect("migrate");

        let first = install_build(&store).await;
        let second = install_build(&store).await;
        assert_ne!(first.kernel_build_id, second.kernel_build_id);
        assert_eq!(
            second.receipt.resulting_revision.get(),
            first.receipt.resulting_revision.get() + 1,
            "the replacement is a single revision-fenced build transition"
        );

        let status = store.kernel_build_status().await.expect("build status");
        assert_eq!(status.current_kernel_build_id, Some(second.kernel_build_id));
        assert_eq!(status.aggregate_revision, second.receipt.resulting_revision);

        let retry = store
            .install_kernel_build(&second.cas, &second.command)
            .await
            .expect("exact replacement retry");
        assert!(retry.was_idempotent_retry);
        assert_eq!(retry.audit_log_id, second.receipt.audit_log_id);
        store.close().await;
    });
}

#[test]
#[ignore = "requires FACTORY_TEST_DATABASE_URL for a disposable PostgreSQL 18 database"]
fn application_admission_is_atomic_idempotent_and_revision_guarded() {
    smol::block_on(async {
        let store = store().await;
        store.migrate_and_verify().await.expect("migrate");
        let build = install_build(&store).await;
        let source_root = build.cas.runtime_root().join("application-source");
        fs::create_dir_all(&source_root).expect("application source root");
        let repository_key = unique("repository");
        let repository_path = format!("/tmp/{}", unique("product"));
        let repository = store
            .register_repository(&RegisterRepository {
                principal: "architect".to_owned(),
                command_id: unique("repository"),
                expected_revision: ExpectedRevision::new(AggregateRevision::initial()),
                repository_key: repository_key.clone(),
                canonical_local_path: repository_path.clone(),
                default_branch: "main".to_owned(),
            })
            .await
            .expect("repository");

        let templates = [
            ("mission.md", b"mission".as_slice()),
            ("product-system.md", b"product system".as_slice()),
            ("product-assignment.md", b"product assignment".as_slice()),
            ("engineering-system.md", b"engineering system".as_slice()),
            (
                "engineering-assignment.md",
                b"engineering assignment".as_slice(),
            ),
            ("quality-system.md", b"quality system".as_slice()),
            ("quality-assignment.md", b"quality assignment".as_slice()),
        ];
        let mut template_digests = Vec::with_capacity(templates.len());
        for (path, bytes) in templates {
            fs::write(source_root.join(path), bytes).expect("template");
            template_digests.push((path, ContentDigest::of_bytes(bytes)));
        }
        let application_key = unique("application");
        fs::write(
            source_root.join("bundle.json"),
            application_bundle_json(
                &application_key,
                &repository_key,
                &repository_path,
                &template_digests,
            ),
        )
        .expect("bundle");
        let command = AdmitCompiledApplication {
            principal: "architect".to_owned(),
            command_id: unique("application-admit"),
            expected_revision: ExpectedRevision::new(AggregateRevision::initial()),
            expected_kernel_build_revision: ExpectedRevision::new(build.receipt.resulting_revision),
            kernel_build_id: build.kernel_build_id,
            source_root: source_root.clone(),
            bundle_relative_path: "bundle.json".into(),
        };
        let accepted = store
            .admit_compiled_application(&build.cas, &command)
            .await
            .expect("compiled application admission");
        assert_eq!(accepted.resulting_revision.get(), 1);
        let activation_command = artifact_command(&build, "application-activation");
        let activation_rationale = SealedArtifactReferenceV1 {
            artifact_id: store
                .register_artifact(&build.cas, &activation_command)
                .await
                .expect("activation rationale")
                .artifact_id,
            digest: activation_command.sealed.digest(),
            byte_length: activation_command.sealed.byte_length(),
        };
        store
            .activate_application_revision(&ActivateApplicationRevision {
                principal: ArchitectPrincipalV1::parse("architect").expect("principal"),
                command_id: unique("application-activate"),
                expected_revision: ExpectedRevision::new(accepted.resulting_revision),
                application_key: ApplicationKey::parse(application_key).expect("application key"),
                application_revision_id: accepted.application_revision_id,
                rationale: activation_rationale,
            })
            .await
            .expect("activate application");
        // The canonical fresh schema stores seven fixed templates directly on
        // the application revision. The durable-office relation is the only
        // additional Phase 1 table; templates remain direct authority facts.
        let inspection = sqlx::PgPool::connect(&test_database_url())
            .await
            .expect("read-only schema inspection pool");
        let application_templates_relation = sqlx::query_scalar!(
            "SELECT to_regclass('factory.application_revision_templates')::TEXT
             AS \"relation?\""
        )
        .fetch_one(&inspection)
        .await
        .expect("template relation inspection");
        assert_eq!(application_templates_relation, None);
        let table_count = sqlx::query_scalar!(
            "SELECT count(*)::BIGINT AS \"count!\"
             FROM information_schema.tables
             WHERE table_schema = 'factory' AND table_type = 'BASE TABLE'"
        )
        .fetch_one(&inspection)
        .await
        .expect("Factory table count");
        assert!(
            table_count <= 21,
            "Factory table count exceeded the durable-office authority cap"
        );
        let fixed_templates = sqlx::query!(
            "SELECT mission_artifact_id,
                    product_research_system_template_artifact_id,
                    product_research_assignment_template_artifact_id,
                    engineering_system_template_artifact_id,
                    engineering_assignment_template_artifact_id,
                    quality_system_template_artifact_id,
                    quality_assignment_template_artifact_id
             FROM factory.application_revisions WHERE id = $1",
            accepted.application_revision_id.get()
        )
        .fetch_one(&inspection)
        .await
        .expect("fixed application templates");
        assert!(fixed_templates.mission_artifact_id > 0);
        assert!(fixed_templates.product_research_system_template_artifact_id > 0);
        assert!(fixed_templates.product_research_assignment_template_artifact_id > 0);
        assert!(fixed_templates.engineering_system_template_artifact_id > 0);
        assert!(fixed_templates.engineering_assignment_template_artifact_id > 0);
        assert!(fixed_templates.quality_system_template_artifact_id > 0);
        assert!(fixed_templates.quality_assignment_template_artifact_id > 0);
        inspection.close().await;

        let proposal_artifact = store
            .register_artifact(&build.cas, &artifact_command(&build, "ticket-proposal"))
            .await
            .expect("ticket proposal artifact");
        let reproducer_artifact = store
            .register_artifact(&build.cas, &artifact_command(&build, "ticket-reproducer"))
            .await
            .expect("ticket reproducer artifact");
        let expected_artifact = store
            .register_artifact(&build.cas, &artifact_command(&build, "ticket-expected"))
            .await
            .expect("ticket expected observation artifact");
        let observed_artifact = store
            .register_artifact(&build.cas, &artifact_command(&build, "ticket-observed"))
            .await
            .expect("ticket observed observation artifact");
        let ticket_store = store.ticket_store();
        let proposal = SubmitTicketProposal {
            principal: "product".to_owned(),
            command_id: unique("ticket-propose"),
            expected_application_revision: ExpectedRevision::new(accepted.resulting_revision),
            application_revision_id: accepted.application_revision_id,
            proposal_artifact_id: proposal_artifact.artifact_id,
            reproducer_artifact_id: reproducer_artifact.artifact_id,
            expected_observation_artifact_id: expected_artifact.artifact_id,
            first_actual_observation_artifact_id: observed_artifact.artifact_id,
            second_actual_observation_artifact_id: observed_artifact.artifact_id,
            discovery_commit: "base-commit".to_owned(),
            discovery_tree: "base-tree".to_owned(),
        };
        let proposed = ticket_store
            .submit_ticket_proposal(&proposal)
            .await
            .expect("fresh circular ticket/current-revision insertion");
        assert!(
            ticket_store
                .submit_ticket_proposal(&proposal)
                .await
                .expect("exact proposal retry")
                .was_idempotent_retry
        );
        let context = ticket_store
            .proposal_admission_context(accepted.application_revision_id)
            .await
            .expect("read-only proposal context");
        assert_eq!(context.proposal_maximum, 1);
        assert_eq!(
            ticket_store
                .live_ticket_proposal_artifacts(accepted.application_revision_id)
                .await
                .expect("read-only live proposals")
                .len(),
            1
        );
        let proposal_backpressure = SubmitTicketProposal {
            command_id: unique("ticket-propose-over-cap"),
            ..proposal.clone()
        };
        assert!(matches!(
            ticket_store
                .submit_ticket_proposal(&proposal_backpressure)
                .await,
            Err(StoreError::ProposalBufferFull)
        ));
        let sponsored = ticket_store
            .sponsor_ticket_revision(&SponsorTicketRevision {
                principal: "architect".to_owned(),
                command_id: unique("ticket-sponsor"),
                ticket_revision_id: proposed.ticket_revision_id,
                expected_ticket_revision: ExpectedRevision::new(proposed.resulting_revision),
                reason: "bounded useful work".to_owned(),
            })
            .await
            .expect("sponsor reproducible ticket");
        assert_eq!(sponsored.state, factory_protocol::TicketState::Sponsored);
        let start_campaign = StartCampaign {
            principal: "architect".to_owned(),
            command_id: unique("ticket-campaign"),
            expected_application_revision: ExpectedRevision::new(accepted.resulting_revision),
            application_revision_id: accepted.application_revision_id,
            aggregate_budget: MicroUsd::new(100),
            deadline_unix_millis: 4_000_000_000_000,
            delivery_target: 1,
        };
        let campaign = store
            .process_store()
            .start_campaign(&start_campaign)
            .await
            .expect("campaign for exact ticket claim");
        assert_eq!(campaign.kernel_build_id, build.kernel_build_id);
        assert_eq!(
            campaign.application_revision_id,
            accepted.application_revision_id
        );
        assert_eq!(campaign.repository_id, repository.repository_id);
        let campaign_retry = store
            .process_store()
            .start_campaign(&start_campaign)
            .await
            .expect("idempotent campaign retry");
        assert!(campaign_retry.was_idempotent_retry);
        assert_eq!(campaign_retry.campaign_id, campaign.campaign_id);
        assert_eq!(campaign_retry.kernel_build_id, campaign.kernel_build_id);
        assert_eq!(campaign_retry.repository_id, campaign.repository_id);
        let before_campaign_status = store
            .process_store()
            .process_fact_counts()
            .await
            .expect("fact counts before campaign status");
        let campaign_status = store
            .process_store()
            .campaign_status(campaign.campaign_id)
            .await
            .expect("read-only campaign status");
        assert_eq!(campaign_status.kernel_build_id, campaign.kernel_build_id);
        assert_eq!(
            campaign_status.application_revision_id,
            campaign.application_revision_id
        );
        assert_eq!(campaign_status.repository_id, campaign.repository_id);
        assert_eq!(campaign_status.delivery_target, 1);
        assert_eq!(
            store
                .process_store()
                .process_fact_counts()
                .await
                .expect("fact counts after campaign status"),
            before_campaign_status
        );
        let requalification = CurrentHeadRequalification {
            current_head_commit: "current-commit".to_owned(),
            current_head_tree: "current-tree".to_owned(),
            first_actual_observation_artifact_id: observed_artifact.artifact_id,
            second_actual_observation_artifact_id: observed_artifact.artifact_id,
        };
        let scheduler = store.ticket_scheduler();
        let scheduler_inspection = sqlx::PgPool::connect(&test_database_url())
            .await
            .expect("read-only scheduler inspection pool");
        let audit_count_before_scheduler_read =
            sqlx::query_scalar!("SELECT count(*)::BIGINT AS \"count!\" FROM factory.audit_log")
                .fetch_one(&scheduler_inspection)
                .await
                .expect("audit count before scheduler read");
        let next_action = scheduler
            .next_action(campaign.campaign_id)
            .await
            .expect("read-only FIFO scheduler action");
        let audit_count_after_scheduler_read =
            sqlx::query_scalar!("SELECT count(*)::BIGINT AS \"count!\" FROM factory.audit_log")
                .fetch_one(&scheduler_inspection)
                .await
                .expect("audit count after scheduler read");
        assert_eq!(
            audit_count_after_scheduler_read,
            audit_count_before_scheduler_read
        );
        let SchedulerNextAction::ClaimReadyTicket(claim_action) = next_action else {
            panic!("sponsored FIFO head must become the next engineering claim");
        };
        assert_eq!(
            claim_action,
            ClaimReadyTicketAction {
                campaign_id: campaign.campaign_id,
                expected_campaign_revision: ExpectedRevision::new(campaign.resulting_revision),
                ticket: factory_kernel::ticket_store::SponsoredTicketClaimContext {
                    ticket_revision_id: proposed.ticket_revision_id,
                    revision: sponsored.resulting_revision,
                },
            }
        );
        scheduler_inspection.close().await;
        let ticket_claim_command_id = unique("ticket-claim");
        let claimed = scheduler
            .claim_ready_ticket(
                "scheduler",
                &ticket_claim_command_id,
                claim_action,
                requalification.clone(),
            )
            .await
            .expect("claim exact sponsored ticket through scheduler authority");
        assert!(
            scheduler
                .claim_ready_ticket(
                    "scheduler",
                    &ticket_claim_command_id,
                    claim_action,
                    requalification.clone(),
                )
                .await
                .expect("exact scheduler claim retry")
                .was_idempotent_retry
        );
        let ticket_attempt_id = match claimed.outcome {
            ClaimOutcome::Claimed { ticket_attempt_id } => ticket_attempt_id,
            ClaimOutcome::Resolved | ClaimOutcome::Blocked => {
                panic!("reproduced ticket was not claimed")
            }
        };
        let failed = ticket_store
            .fail_ticket_attempt(&FailTicketAttempt {
                principal: "engineering".to_owned(),
                command_id: unique("ticket-attempt-fail"),
                ticket_attempt_id,
                expected_attempt_revision: ExpectedRevision::new(AggregateRevision::initial()),
                expected_ticket_revision: ExpectedRevision::new(claimed.resulting_ticket_revision),
                reason: "synthetic child failure".to_owned(),
            })
            .await
            .expect("terminal attempt failure");
        assert!(matches!(
            ticket_store
                .claim_sponsored_ticket(&ClaimSponsoredTicket {
                    principal: "scheduler".to_owned(),
                    command_id: unique("ticket-no-auto-retry"),
                    campaign_id: campaign.campaign_id,
                    expected_campaign_revision: ExpectedRevision::new(campaign.resulting_revision),
                    ticket_revision_id: proposed.ticket_revision_id,
                    expected_ticket_revision: ExpectedRevision::new(
                        claimed.resulting_ticket_revision
                    ),
                    requalification: requalification.clone(),
                })
                .await,
            Err(StoreError::TicketStateConflict { .. })
        ));
        let released = ticket_store
            .release_ticket_attempt(&ReleaseTicketAttempt {
                principal: "architect".to_owned(),
                command_id: unique("ticket-release"),
                ticket_attempt_id,
                expected_attempt_revision: ExpectedRevision::new(failed.resulting_attempt_revision),
                expected_ticket_revision: ExpectedRevision::new(claimed.resulting_ticket_revision),
                reason: "explicit fresh-head release".to_owned(),
                requalification,
            })
            .await
            .expect("explicit successful release");
        assert_eq!(released.outcome, ReleaseOutcome::Released);
        let concurrent_proposal_a = store
            .register_artifact(
                &build.cas,
                &artifact_command(&build, "concurrent-proposal-a"),
            )
            .await
            .expect("concurrent proposal A artifact");
        let concurrent_reproducer_a = store
            .register_artifact(
                &build.cas,
                &artifact_command(&build, "concurrent-reproducer-a"),
            )
            .await
            .expect("concurrent reproducer A artifact");
        let concurrent_proposal_b = store
            .register_artifact(
                &build.cas,
                &artifact_command(&build, "concurrent-proposal-b"),
            )
            .await
            .expect("concurrent proposal B artifact");
        let concurrent_reproducer_b = store
            .register_artifact(
                &build.cas,
                &artifact_command(&build, "concurrent-reproducer-b"),
            )
            .await
            .expect("concurrent reproducer B artifact");
        let proposal_a = SubmitTicketProposal {
            command_id: unique("concurrent-proposal-a"),
            proposal_artifact_id: concurrent_proposal_a.artifact_id,
            reproducer_artifact_id: concurrent_reproducer_a.artifact_id,
            ..proposal.clone()
        };
        let proposal_b = SubmitTicketProposal {
            command_id: unique("concurrent-proposal-b"),
            proposal_artifact_id: concurrent_proposal_b.artifact_id,
            reproducer_artifact_id: concurrent_reproducer_b.artifact_id,
            ..proposal.clone()
        };
        let (first_concurrent, second_concurrent) = smol::future::zip(
            ticket_store.submit_ticket_proposal(&proposal_a),
            ticket_store.submit_ticket_proposal(&proposal_b),
        )
        .await;
        let queued = match (first_concurrent, second_concurrent) {
            (Ok(queued), Err(StoreError::ProposalBufferFull))
            | (Err(StoreError::ProposalBufferFull), Ok(queued)) => queued,
            (first, second) => {
                panic!("proposal-cap race admitted an invalid outcome: {first:?}, {second:?}")
            }
        };
        let queued_sponsored = ticket_store
            .sponsor_ticket_revision(&SponsorTicketRevision {
                principal: "architect".to_owned(),
                command_id: unique("ticket-sponsor-second-ready"),
                ticket_revision_id: queued.ticket_revision_id,
                expected_ticket_revision: ExpectedRevision::new(queued.resulting_revision),
                reason: "second bounded ready ticket".to_owned(),
            })
            .await
            .expect("sponsor the second ready ticket");
        let resolved = ticket_store
            .claim_sponsored_ticket(&ClaimSponsoredTicket {
                principal: "scheduler".to_owned(),
                command_id: unique("ticket-resolved-new-head"),
                campaign_id: campaign.campaign_id,
                expected_campaign_revision: ExpectedRevision::new(campaign.resulting_revision),
                ticket_revision_id: proposed.ticket_revision_id,
                expected_ticket_revision: ExpectedRevision::new(released.resulting_ticket_revision),
                requalification: CurrentHeadRequalification {
                    current_head_commit: "resolved-commit".to_owned(),
                    current_head_tree: "resolved-tree".to_owned(),
                    first_actual_observation_artifact_id: expected_artifact.artifact_id,
                    second_actual_observation_artifact_id: expected_artifact.artifact_id,
                },
            })
            .await
            .expect("resolved on new product head");
        assert_eq!(resolved.outcome, ClaimOutcome::Resolved);
        let diverged = ticket_store
            .claim_sponsored_ticket(&ClaimSponsoredTicket {
                principal: "scheduler".to_owned(),
                command_id: unique("ticket-divergent-new-head"),
                campaign_id: campaign.campaign_id,
                expected_campaign_revision: ExpectedRevision::new(campaign.resulting_revision),
                ticket_revision_id: queued.ticket_revision_id,
                expected_ticket_revision: ExpectedRevision::new(
                    queued_sponsored.resulting_revision,
                ),
                requalification: CurrentHeadRequalification {
                    current_head_commit: "diverged-commit".to_owned(),
                    current_head_tree: "diverged-tree".to_owned(),
                    first_actual_observation_artifact_id: observed_artifact.artifact_id,
                    second_actual_observation_artifact_id: expected_artifact.artifact_id,
                },
            })
            .await
            .expect("divergent current-head reproducer is a durable outcome");
        assert_eq!(diverged.outcome, ClaimOutcome::Blocked);
        // Claims are durable before assignment/session creation. Two daemon
        // loops may therefore observe two ready rows concurrently, but the
        // application advisory lock must admit just one InFlight ticket and
        // leave the losing ticket and its audit receipt untouched.
        let wip_proposal_a_artifact = store
            .register_artifact(&build.cas, &artifact_command(&build, "wip-proposal-a"))
            .await
            .expect("WIP proposal A artifact");
        let wip_reproducer_a_artifact = store
            .register_artifact(&build.cas, &artifact_command(&build, "wip-reproducer-a"))
            .await
            .expect("WIP reproducer A artifact");
        let wip_proposal_b_artifact = store
            .register_artifact(&build.cas, &artifact_command(&build, "wip-proposal-b"))
            .await
            .expect("WIP proposal B artifact");
        let wip_reproducer_b_artifact = store
            .register_artifact(&build.cas, &artifact_command(&build, "wip-reproducer-b"))
            .await
            .expect("WIP reproducer B artifact");
        let wip_proposed_a = ticket_store
            .submit_ticket_proposal(&SubmitTicketProposal {
                command_id: unique("wip-propose-a"),
                proposal_artifact_id: wip_proposal_a_artifact.artifact_id,
                reproducer_artifact_id: wip_reproducer_a_artifact.artifact_id,
                ..proposal.clone()
            })
            .await
            .expect("WIP proposal A");
        let wip_sponsored_a = ticket_store
            .sponsor_ticket_revision(&SponsorTicketRevision {
                principal: "architect".to_owned(),
                command_id: unique("wip-sponsor-a"),
                ticket_revision_id: wip_proposed_a.ticket_revision_id,
                expected_ticket_revision: ExpectedRevision::new(wip_proposed_a.resulting_revision),
                reason: "first racing ready ticket".to_owned(),
            })
            .await
            .expect("sponsor WIP proposal A");
        let wip_proposed_b = ticket_store
            .submit_ticket_proposal(&SubmitTicketProposal {
                command_id: unique("wip-propose-b"),
                proposal_artifact_id: wip_proposal_b_artifact.artifact_id,
                reproducer_artifact_id: wip_reproducer_b_artifact.artifact_id,
                ..proposal.clone()
            })
            .await
            .expect("WIP proposal B");
        let wip_sponsored_b = ticket_store
            .sponsor_ticket_revision(&SponsorTicketRevision {
                principal: "architect".to_owned(),
                command_id: unique("wip-sponsor-b"),
                ticket_revision_id: wip_proposed_b.ticket_revision_id,
                expected_ticket_revision: ExpectedRevision::new(wip_proposed_b.resulting_revision),
                reason: "second racing ready ticket".to_owned(),
            })
            .await
            .expect("sponsor WIP proposal B");

        let wip_proposal_c_artifact = store
            .register_artifact(&build.cas, &artifact_command(&build, "wip-proposal-c"))
            .await
            .expect("WIP proposal C artifact");
        let wip_reproducer_c_artifact = store
            .register_artifact(&build.cas, &artifact_command(&build, "wip-reproducer-c"))
            .await
            .expect("WIP reproducer C artifact");
        let wip_proposed_c = ticket_store
            .submit_ticket_proposal(&SubmitTicketProposal {
                command_id: unique("wip-propose-c"),
                proposal_artifact_id: wip_proposal_c_artifact.artifact_id,
                reproducer_artifact_id: wip_reproducer_c_artifact.artifact_id,
                ..proposal.clone()
            })
            .await
            .expect("WIP proposal C");
        assert!(matches!(
            ticket_store
                .sponsor_ticket_revision(&SponsorTicketRevision {
                    principal: "architect".to_owned(),
                    command_id: unique("wip-ready-cap"),
                    ticket_revision_id: wip_proposed_c.ticket_revision_id,
                    expected_ticket_revision: ExpectedRevision::new(
                        wip_proposed_c.resulting_revision
                    ),
                    reason: "third ready ticket must exceed the hard maximum".to_owned(),
                })
                .await,
            Err(StoreError::ReadyTicketBufferFull)
        ));

        let wip_claim_a_command_id = unique("wip-claim-a");
        let wip_claim_b_command_id = unique("wip-claim-b");
        let wip_claim_a = ClaimSponsoredTicket {
            principal: "scheduler".to_owned(),
            command_id: wip_claim_a_command_id.clone(),
            campaign_id: campaign.campaign_id,
            expected_campaign_revision: ExpectedRevision::new(campaign.resulting_revision),
            ticket_revision_id: wip_proposed_a.ticket_revision_id,
            expected_ticket_revision: ExpectedRevision::new(wip_sponsored_a.resulting_revision),
            requalification: CurrentHeadRequalification {
                current_head_commit: "wip-current-commit-a".to_owned(),
                current_head_tree: "wip-current-tree-a".to_owned(),
                first_actual_observation_artifact_id: observed_artifact.artifact_id,
                second_actual_observation_artifact_id: observed_artifact.artifact_id,
            },
        };
        let wip_claim_b = ClaimSponsoredTicket {
            principal: "scheduler".to_owned(),
            command_id: wip_claim_b_command_id.clone(),
            campaign_id: campaign.campaign_id,
            expected_campaign_revision: ExpectedRevision::new(campaign.resulting_revision),
            ticket_revision_id: wip_proposed_b.ticket_revision_id,
            expected_ticket_revision: ExpectedRevision::new(wip_sponsored_b.resulting_revision),
            requalification: CurrentHeadRequalification {
                current_head_commit: "wip-current-commit-b".to_owned(),
                current_head_tree: "wip-current-tree-b".to_owned(),
                first_actual_observation_artifact_id: observed_artifact.artifact_id,
                second_actual_observation_artifact_id: observed_artifact.artifact_id,
            },
        };
        let (wip_a, wip_b) = smol::future::zip(
            ticket_store.claim_sponsored_ticket(&wip_claim_a),
            ticket_store.claim_sponsored_ticket(&wip_claim_b),
        )
        .await;
        let (winning_command, losing_ticket_revision_id) = match (wip_a, wip_b) {
            (Ok(receipt), Err(StoreError::EngineeringTicketAlreadyInFlight)) => {
                assert!(matches!(receipt.outcome, ClaimOutcome::Claimed { .. }));
                (&wip_claim_a, wip_proposed_b.ticket_revision_id)
            }
            (Err(StoreError::EngineeringTicketAlreadyInFlight), Ok(receipt)) => {
                assert!(matches!(receipt.outcome, ClaimOutcome::Claimed { .. }));
                (&wip_claim_b, wip_proposed_a.ticket_revision_id)
            }
            (first, second) => {
                panic!("WIP race admitted an invalid outcome: {first:?}, {second:?}")
            }
        };
        assert!(
            ticket_store
                .claim_sponsored_ticket(winning_command)
                .await
                .expect("exact winning claim retry")
                .was_idempotent_retry
        );
        let inspection = sqlx::PgPool::connect(&test_database_url())
            .await
            .expect("read-only WIP inspection pool");
        let losing_state = sqlx::query_scalar!(
            "SELECT lifecycle FROM factory.ticket_revisions WHERE id = $1",
            losing_ticket_revision_id.get(),
        )
        .fetch_one(&inspection)
        .await
        .expect("losing ticket state");
        assert_eq!(losing_state, 1, "losing racing ticket remained sponsored");
        let claim_audits = sqlx::query_scalar!(
            "SELECT count(*)::BIGINT AS \"count!\" FROM factory.audit_log
             WHERE principal = $1 AND command_id IN ($2, $3)",
            "scheduler",
            &wip_claim_a_command_id,
            &wip_claim_b_command_id,
        )
        .fetch_one(&inspection)
        .await
        .expect("racing claim audits");
        assert_eq!(
            claim_audits, 1,
            "losing racing claim wrote no audit receipt"
        );
        inspection.close().await;
        assert!(
            store
                .admit_compiled_application(&build.cas, &command)
                .await
                .expect("idempotent application retry")
                .was_idempotent_retry
        );
        let stale = AdmitCompiledApplication {
            command_id: unique("stale-application"),
            ..command.clone()
        };
        assert!(matches!(
            store.admit_compiled_application(&build.cas, &stale).await,
            Err(StoreError::RevisionConflict { .. })
        ));
        let predecessor_mismatch = AdmitCompiledApplication {
            command_id: unique("predecessor-mismatch"),
            expected_revision: ExpectedRevision::new(AggregateRevision::from_persisted(1)),
            ..command.clone()
        };
        assert!(matches!(
            store
                .admit_compiled_application(&build.cas, &predecessor_mismatch)
                .await,
            Err(StoreError::BundlePredecessorMismatch)
        ));
        fs::write(source_root.join("mission.md"), b"changed same path").expect("mutate template");
        let digest_mismatch = AdmitCompiledApplication {
            command_id: unique("template-digest-mismatch"),
            ..command
        };
        assert!(matches!(
            store
                .admit_compiled_application(&build.cas, &digest_mismatch)
                .await,
            Err(StoreError::ApplicationTemplateDigestMismatch { .. })
        ));
        assert!(repository.repository_id.get() > 0);
        store.close().await;
    });
}

#[test]
#[ignore = "requires FACTORY_TEST_DATABASE_URL for a disposable PostgreSQL 18 database"]
fn singleton_lock_releases_for_orderly_and_restart_paths() {
    smol::block_on(async {
        let first = store().await;
        first.migrate_and_verify().await.expect("migrate");
        let second = store().await;
        let lock = first.acquire_daemon_lock().await.expect("first lock");
        assert!(matches!(
            second.acquire_daemon_lock().await,
            Err(StoreError::DaemonAlreadyRunning)
        ));
        lock.release().await.expect("orderly release");
        let lock = second.acquire_daemon_lock().await.expect("reacquire");
        drop(lock);
        first.close().await;
        second.close().await;

        let restarted = store().await;
        restarted
            .verify_schema_identity()
            .await
            .expect("restart identity");
        restarted
            .acquire_daemon_lock()
            .await
            .expect("restart lock")
            .release()
            .await
            .expect("restart release");
        restarted.close().await;
    });
}

struct InstalledBuild {
    cas: CasStore,
    command: InstallKernelBuild,
    receipt: factory_kernel::storage::KernelBuildReceipt,
    kernel_build_id: factory_protocol::KernelBuildId,
}

async fn install_build(store: &KernelStore) -> InstalledBuild {
    let expected_revision = store
        .kernel_build_status()
        .await
        .expect("status")
        .aggregate_revision;
    let cas = CasStore::new_with_seed(
        std::env::temp_dir().join(unique("factory-storage-cas")),
        4 * 1024 * 1024,
        unique_number(),
    )
    .expect("CAS");
    let staging = cas.runtime_root().join("staging");
    fs::create_dir_all(&staging).expect("staging");
    fs::write(staging.join("qualification.json"), b"qualified").expect("qualification");
    let qualification_receipt = cas
        .adopt(&staging, "qualification.json")
        .expect("seal qualification");
    let command = InstallKernelBuild {
        principal: "operator".to_owned(),
        command_id: unique("install-build"),
        expected_revision: ExpectedRevision::new(expected_revision),
        build_id: factory_protocol::KernelBuildId::new(digest(unique_number())),
        source_digest: digest(unique_number()),
        binary_digest: digest(unique_number()),
        schema_identity: SCHEMA_IDENTITY.to_owned(),
        deno_executable_path: "/opt/factory/deno".to_owned(),
        deno_version: "deno 2.9.4".to_owned(),
        deno_lock_digest: digest(unique_number()),
        qualification_receipt,
    };
    let receipt = store
        .install_kernel_build(&cas, &command)
        .await
        .expect("install build");
    InstalledBuild {
        cas,
        kernel_build_id: receipt.kernel_build_id,
        command,
        receipt,
    }
}

fn artifact_command(build: &InstalledBuild, label: &str) -> RegisterArtifact {
    let serial = unique_number();
    let staging = build.cas.runtime_root().join("staging");
    fs::write(
        staging.join(format!("{label}-{serial}.bin")),
        [serial as u8],
    )
    .expect("artifact source");
    let sealed = build
        .cas
        .adopt(&staging, format!("{label}-{serial}.bin"))
        .expect("seal artifact");
    RegisterArtifact {
        principal: "operator".to_owned(),
        command_id: format!("{label}-{serial}"),
        expected_kernel_build_revision: ExpectedRevision::new(build.receipt.resulting_revision),
        kernel_build_id: build.kernel_build_id,
        sealed,
    }
}

fn application_bundle_json(
    application_key: &str,
    repository_key: &str,
    repository_path: &str,
    templates: &[(&str, ContentDigest)],
) -> String {
    use factory_protocol::{
        ApplicationBundleWireV1, AssignmentRoleWireV1, CommandWireV1, CommitMessageWireV1,
        ExecutableWireV1, GitWireV1, LimitsWireV1, ModelWireV1, RepositoryWireV1,
        RequiredReadWireV1, TemplateWireV1, TicketBoundsWireV1, TicketPolicyWireV1,
        ValidationWireV1, canonical_application_bundle_json_v1,
    };

    let template = |index: usize| TemplateWireV1 {
        source_path: templates[index].0.to_owned(),
        digest: templates[index].1.to_hex(),
        placeholders: Vec::new(),
        rendered_byte_limit: 4096,
    };
    let command = |name: &str| CommandWireV1 {
        name: name.to_owned(),
        executable: ExecutableWireV1 {
            approved_tool: Some("cargo".to_owned()),
            repository_path: None,
        },
        argv: vec!["test".to_owned()],
        working_directory: ".".to_owned(),
        environment: Vec::new(),
        timeout_millis: 1,
        stdout_byte_limit: 4096,
        stderr_byte_limit: 4096,
        expected_exit_status: 0,
    };
    let assignment_role_profile =
        |assignment_role: &str, system_index: usize, assignment_index: usize| {
            AssignmentRoleWireV1 {
                assignment_role: assignment_role.to_owned(),
                system_template: template(system_index),
                assignment_template: template(assignment_index),
                tools: vec!["workspace_read".to_owned()],
                model: ModelWireV1 {
                    provider: "test".to_owned(),
                    model_id: "test-model".to_owned(),
                    thinking_level: "none".to_owned(),
                    context_token_limit: 1,
                    output_token_limit: 1,
                    price_input_micro_usd_per_million_tokens: 0,
                    price_output_micro_usd_per_million_tokens: 0,
                    price_cache_read_micro_usd_per_million_tokens: 0,
                    price_cache_write_micro_usd_per_million_tokens: 0,
                    capability_flags: Vec::new(),
                },
                limits: LimitsWireV1 {
                    turn_limit: 1,
                    wall_limit_millis: 1,
                    output_byte_limit: 4096,
                },
            }
        };
    canonical_application_bundle_json_v1(&ApplicationBundleWireV1 {
        format_version: 1,
        application_key: application_key.to_owned(),
        predecessor_bundle: None,
        repository: RepositoryWireV1 {
            repository_key: repository_key.to_owned(),
            canonical_local_path: repository_path.to_owned(),
            default_branch: "main".to_owned(),
            delivery_mode: "local_fast_forward_only".to_owned(),
        },
        mission_template: template(0),
        assignment_role_profiles: vec![
            assignment_role_profile("product_research", 1, 2),
            assignment_role_profile("engineering", 3, 4),
            assignment_role_profile("quality", 5, 6),
        ],
        ticket_policy: TicketPolicyWireV1 {
            low_water: 1,
            target: 1,
            maximum: 2,
            proposal_maximum: 1,
            ticket_bounds: TicketBoundsWireV1 {
                narrative_byte_limit: 1,
                acceptance_criteria_limit: 1,
                contract_read_limit: 1,
            },
        },
        required_reads: vec![RequiredReadWireV1 {
            path: "AGENTS.md".to_owned(),
            reason: "test".to_owned(),
        }],
        reproducer_profiles: Vec::new(),
        validation_profiles: ValidationWireV1 {
            focused: vec![command("focused")],
            full: vec![command("full")],
        },
        git_policy: GitWireV1 {
            forbidden_paths: Vec::new(),
            delivery_mode: "local_fast_forward_only".to_owned(),
            provenance_trailers_required: true,
        },
        commit_message_policy: CommitMessageWireV1 {
            subject_byte_limit: 1,
            body_byte_limit: 1,
        },
    })
    .expect("test bundle uses the canonical V1 serializer")
}

async fn store() -> KernelStore {
    KernelStore::connect(&test_database_url())
        .await
        .expect("connect disposable PostgreSQL database")
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
