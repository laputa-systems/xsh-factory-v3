//! Provider-free end-to-end judge for the generic one-ticket MVP circuit.
//!
//! This is intentionally an isolated integration harness. It owns one exact
//! disposable database, one local Git repository, and three small Deno actors;
//! it imports no Pi SDK and supplies no provider credential. It proves generic
//! resident composition, not an execution of the XSH application. The actors
//! may only use the inherited session socket, while the test moves between
//! offices through the public typed kernel authorities.

use std::{
    ffi::OsString,
    fs,
    path::{Path, PathBuf},
    process::Command,
    sync::{
        Arc,
        atomic::{AtomicU64, Ordering},
    },
};

use factory_kernel::installed_runtime::{
    InstalledApprovedToolsQualificationV1, InstalledKernelBuildReceiptV1, InstalledRuntimeManifest,
    InstalledRuntimeQualification, qualify_kernel_binary_v1, qualify_kernel_source_v1,
};
use factory_kernel::{
    assignment_runtime::AssignmentLaunchOutcome,
    campaign_driver::{CampaignDriver, CampaignDriverOutcome},
    cas::{CasArtifact, CasStore},
    decision_store::{DecideCandidate, SponsorTicket},
    durable_authority::DurableAuthorityResolver,
    git::GitCustody,
    local_transport::{LocalDaemon, LocalTransportConfig},
    operator_rpc::ArchitectTransitionResolver,
    process::{ProcessStore, StartCampaign},
    scheduler::SchedulerNextAction,
    storage::{
        ActivateApplicationRevision, AdmitCompiledApplication, InstallQualifiedKernelBuild,
        KernelStore, RegisterArtifact, RegisterRepository, SCHEMA_IDENTITY,
    },
};
use factory_protocol::{
    AggregateRevision, ApplicationBundleWireV1, ApplicationKey, ApplicationRevisionId,
    ArchitectPrincipalV1, CommandWireV1, CommitMessageWireV1, ContentDigest, ExecutableWireV1,
    ExpectedRevision, GitWireV1, LimitsWireV1, MicroUsd, ModelWireV1, OfficeWireV1,
    RepositoryWireV1, RuntimeRelativePath, SealedArtifactReferenceV1, SponsorshipDecisionV1,
    TemplateWireV1, TicketBoundsWireV1, TicketPolicyWireV1, ValidationWireV1,
    canonical_application_bundle_json_v1, canonical_command_profile_json_v1,
};
use sqlx::Row;

static NEXT: AtomicU64 = AtomicU64::new(1);
static FIXTURE_MIGRATOR: sqlx::migrate::Migrator = sqlx::migrate!("../../schema/migrations");

#[test]
#[ignore = "requires FACTORY_TEST_DATABASE_URL for one disposable PostgreSQL database"]
fn provider_free_generic_vertical_delivers_one_local_commit() {
    smol::block_on(async {
        let fixture = Fixture::new().await;
        fixture.run().await;
        fixture.close().await;
    });
}

#[test]
#[ignore = "requires FACTORY_TEST_DATABASE_URL for one disposable PostgreSQL database"]
fn candidate_attachment_requires_complete_successful_engineering_terminal_provenance() {
    smol::block_on(async {
        let fixture = Fixture::new().await;
        fixture
            .reject_invalid_engineering_terminal_provenance()
            .await;
        fixture.close().await;
    });
}

#[test]
#[ignore = "requires FACTORY_TEST_DATABASE_URL for one disposable PostgreSQL database"]
fn missing_provider_credential_terminalizes_product_without_a_retry_loop() {
    smol::block_on(async {
        let fixture = Fixture::new().await;
        let driver = CampaignDriver::with_credential_lookup(
            fixture.store.clone(),
            fixture.cas.clone(),
            fixture.installed.clone(),
            fixture
                .installed
                .execution_tools(&fixture.root.join("missing-credential-git-runtime"))
                .expect("installed execution tools"),
            Arc::clone(&fixture.resolver),
            |_| None,
        );
        assert!(matches!(
            driver
                .run_next(&fixture.daemon)
                .await
                .expect("credential failure becomes a durable outcome"),
            CampaignDriverOutcome::CampaignFailed { .. }
        ));
        assert!(matches!(
            driver
                .run_next(&fixture.daemon)
                .await
                .expect("terminal campaign is quiescent"),
            CampaignDriverOutcome::NoRunningCampaign
        ));
        let campaign = fixture
            .process
            .campaign_status(fixture.campaign.campaign_id)
            .await
            .expect("failed campaign status");
        assert_eq!(campaign.state, factory_protocol::CampaignState::Failed);
        assert!(
            campaign
                .failure_reason
                .as_deref()
                .is_some_and(|reason| reason.contains("credential environment"))
        );
        assert!(
            fixture
                .process
                .campaign_session_costs(fixture.campaign.campaign_id, None, 10)
                .await
                .expect("no paid sessions")
                .is_empty()
        );
        fixture.close().await;
    });
}

struct Fixture {
    root: PathBuf,
    repository: PathBuf,
    git_path: PathBuf,
    store: KernelStore,
    cas: CasStore,
    build: factory_kernel::storage::KernelBuildReceipt,
    kernel_build_id: factory_protocol::KernelBuildId,
    process: ProcessStore,
    git: Arc<GitCustody>,
    resolver: Arc<DurableAuthorityResolver>,
    installed: InstalledKernelBuildReceiptV1,
    driver: CampaignDriver,
    daemon: LocalDaemon,
    daemon_root: PathBuf,
    application: ApplicationRevisionId,
    campaign: factory_kernel::process::CampaignReceipt,
}

impl Fixture {
    async fn new() -> Self {
        // `make provider-free-vertical` deliberately runs all three scenarios
        // against one caller-created disposable database. Each scenario owns a
        // live campaign while it proves its boundary, so reset only the
        // explicitly name-guarded fixture schema before starting the next one.
        // This keeps the production one-running-campaign invariant intact
        // instead of teaching the fixture to bypass it.
        reset_fixture_schema().await;
        let root = std::env::temp_dir().join(unique("factory-v3-full-vertical"));
        fs::create_dir_all(&root).expect("fixture root");
        // macOS exposes its temporary root through `/var`, while Git reports
        // the same directory through canonical `/private/var`. Keep the
        // application binding and repository custody on one exact path.
        let root = fs::canonicalize(root).expect("canonical fixture root");
        let repository = root.join("product");
        let git_path = system_git();
        git(
            &root,
            &git_path,
            &["init", "--initial-branch=main", "product"],
        );
        git(
            &repository,
            &git_path,
            &["config", "user.name", "Synthetic Test"],
        );
        git(
            &repository,
            &git_path,
            &["config", "user.email", "factory@example.test"],
        );
        fs::write(repository.join("AGENTS.md"), b"read this exact contract\n")
            .expect("agents contract");
        fs::write(
            repository.join("CONTRACT.md"),
            b"read this exact product contract\n",
        )
        .expect("product contract");
        write_script(
            &repository.join("reproduce.sh"),
            "#!/bin/sh\nprintf 'actual\\n'\nprintf 'none\\n' >&2\n",
        );
        write_script(
            &repository.join("validate.sh"),
            "#!/bin/sh\n./reproduce.sh > /tmp/factory-v3-reproduce.out 2>/tmp/factory-v3-reproduce.err\ntest \"$(cat /tmp/factory-v3-reproduce.out)\" = expected\ntest \"$(cat /tmp/factory-v3-reproduce.err)\" = none\n",
        );
        git(&repository, &git_path, &["add", "--all"]);
        git(&repository, &git_path, &["commit", "-m", "synthetic base"]);

        let store = KernelStore::connect(&test_database_url())
            .await
            .expect("test database");
        store.migrate_and_verify().await.expect("fresh migration");
        let credential_environment =
            format!("FACTORY_PROVIDER_FREE_CREDENTIAL_{}", unique_number());
        let (cas, build, installed) = install_build(&store, &root, &credential_environment).await;
        let kernel_build_id = installed.kernel_build_id();
        let process = store.process_store();
        let execution = installed
            .execution_tools(&root.join("git-runtime"))
            .expect("installed execution tools");
        let git = execution.git_custody();
        let repository_key = unique("synthetic-repository");
        store
            .register_repository(&RegisterRepository {
                principal: "architect".to_owned(),
                command_id: unique("register-repository"),
                expected_revision: ExpectedRevision::new(AggregateRevision::initial()),
                repository_key: repository_key.clone(),
                canonical_local_path: repository.to_string_lossy().into_owned(),
                default_branch: "main".to_owned(),
            })
            .await
            .expect("repository registration");
        let application = admit_application(
            &store,
            &cas,
            &build,
            kernel_build_id,
            &repository_key,
            &repository,
        )
        .await;
        let campaign = process
            .start_campaign(&StartCampaign {
                principal: "architect".to_owned(),
                command_id: unique("start-campaign"),
                expected_application_revision: ExpectedRevision::new(application.revision),
                application_revision_id: application.id,
                aggregate_budget: MicroUsd::new(500_000),
                deadline_unix_millis: 4_000_000_000_000,
                delivery_target: 1,
            })
            .await
            .expect("active application campaign");
        // Keep the Unix socket below macOS's SUN_LEN even when the system
        // temporary root itself is verbose.
        let daemon_root = std::env::temp_dir().join(format!("fv3d-{}", unique_number()));
        let daemon = LocalDaemon::bind(LocalTransportConfig::new(daemon_root.clone()), &store)
            .await
            .expect("local daemon");
        let resolver = Arc::new(DurableAuthorityResolver::new(
            store.clone(),
            cas.clone(),
            execution.command_runner().clone(),
            Arc::clone(&git),
        ));
        let driver = CampaignDriver::with_credential_lookup(
            store.clone(),
            cas.clone(),
            installed.clone(),
            execution,
            Arc::clone(&resolver),
            |_| Some(OsString::from("provider-free-inert")),
        );
        Self {
            root,
            repository,
            git_path,
            store,
            cas,
            build,
            kernel_build_id,
            process,
            git,
            resolver,
            installed,
            driver,
            daemon,
            daemon_root,
            application: application.id,
            campaign,
        }
    }

    async fn run(&self) {
        let product = self
            .expect_assignment("Product", self.driver.run_next(&self.daemon).await)
            .await;
        assert_eq!(
            product.outcome.session.terminal.cost,
            factory_protocol::TerminalCostV1::Known(MicroUsd::new(10))
        );
        assert_eq!(
            product.outcome.session.terminal.session_state,
            factory_protocol::SessionState::Succeeded,
            "Product terminal proves its packet/required-read gate before mutation",
        );
        assert!(matches!(
            self.store
                .ticket_scheduler()
                .next_action(self.campaign.campaign_id)
                .await
                .expect("scheduler after Product"),
            SchedulerNextAction::AwaitArchitectDecision {
                proposed_count: 1,
                ..
            }
        ));
        self.cleanup_actor(product);

        // This bounded application-side read is the sole route from the
        // Product terminal to an external Architect decision. The actor never
        // supplied a ticket revision ID in its socket request.
        let ticket = self
            .store
            .ticket_store()
            .live_ticket_proposal_artifacts(self.application)
            .await
            .expect("live product proposal")
            .into_iter()
            .next()
            .expect("one unseeded ticket");
        let sponsorship_rationale = self
            .seal_kernel("architect-sponsor-rationale", b"valuable product fix\n")
            .await;
        let sponsored = self
            .store
            .decision_store()
            .sponsor_ticket(&SponsorTicket {
                command_id: unique("architect-sponsor"),
                expected_ticket_revision: ExpectedRevision::new(ticket_revision(&ticket)),
                decision: SponsorshipDecisionV1 {
                    ticket_revision_id: ticket.ticket_revision_id,
                    rationale: sponsorship_rationale.reference(),
                    principal: ArchitectPrincipalV1::parse("architect").expect("architect"),
                },
            })
            .await
            .expect("external sponsor");
        assert_eq!(
            sponsored.resulting_ticket_revision,
            ticket_revision(&ticket).next().unwrap()
        );

        // The resident driver—not the judge—requalifies the sponsored ticket,
        // claims it, materializes Engineering, and launches the actor.
        let engineering = self
            .expect_assignment(
                "claim and Engineering",
                self.driver.run_next(&self.daemon).await,
            )
            .await;
        assert_eq!(
            engineering.outcome.session.terminal.cost,
            factory_protocol::TerminalCostV1::Known(MicroUsd::new(10))
        );
        assert_eq!(
            engineering.outcome.session.terminal.session_state,
            factory_protocol::SessionState::Succeeded,
            "Engineering terminal proves its packet/required-read gate before mutation",
        );
        let engineering_transcript_digest = ContentDigest::of_bytes(
            &fs::read(engineering.outcome.staging_root.join("session.ndjson.gz"))
                .expect("Engineering terminal transcript staged before exact seal"),
        )
        .to_hex();

        // Hard validation has persisted, but the actor's terminal transcript
        // is the required commit-provenance input.  No ref/commit may exist
        // before the explicit scheduler attachment pass consumes that sealed
        // terminal evidence.
        let before_attach = self
            .store
            .ticket_store()
            .ticket_buffer_status(self.campaign.campaign_id)
            .await
            .expect("candidate commit attachment status");
        let attach_action = before_attach
            .downstream_action
            .expect("candidate attach action");
        assert_eq!(
            attach_action.stage,
            factory_kernel::ticket_store::DownstreamActionStage::CandidateCommitAttachRequired
        );
        assert_eq!(
            before_attach
                .downstream_evidence
                .expect("candidate evidence")
                .candidate_commit,
            None
        );
        let candidate_ref = format!(
            "refs/heads/factory/{}/{}",
            ticket.ticket_id.get(),
            attach_action.candidate_id.get()
        );
        let ref_before_terminal_provenance = Command::new(&self.git_path)
            .current_dir(&self.repository)
            .args(["rev-parse", "--verify", &candidate_ref])
            .output()
            .expect("inspect candidate ref before terminal-provenance attach");
        assert!(
            !ref_before_terminal_provenance.status.success(),
            "candidate.submit must not create a local candidate ref before its terminal transcript"
        );
        self.cleanup_actor(engineering);

        // A durable recovery action may be needed before Quality. Bound the
        // resident passes so a future scheduling loop cannot hide here.
        let quality = {
            let mut launched_quality = None;
            for _ in 0..4 {
                match self
                    .driver
                    .run_next(&self.daemon)
                    .await
                    .expect("resident downstream pass")
                {
                    CampaignDriverOutcome::Assignment(outcome) => {
                        launched_quality = Some(ActorLaunch { outcome });
                        break;
                    }
                    CampaignDriverOutcome::HardValidationResumed
                    | CampaignDriverOutcome::CandidateCommitAttached => {}
                    _ => panic!("resident driver did not reach Quality"),
                }
            }
            launched_quality.expect("resident driver must launch Quality within four passes")
        };
        let attached = self
            .store
            .ticket_store()
            .ticket_buffer_status(self.campaign.campaign_id)
            .await
            .expect("candidate attachment status after Engineering terminal");
        let candidate_commit = attached
            .downstream_evidence
            .and_then(|evidence| evidence.candidate_commit)
            .expect("post-terminal recovery attached exactly one candidate commit");
        let message = git_stdout(
            &self.repository,
            &self.git_path,
            &["show", "-s", "--format=%B", &candidate_commit],
        );
        assert!(
            message.contains(&format!(
                "Factory-Engineering-Session-BLAKE3: {engineering_transcript_digest}"
            )),
            "candidate commit must bind the actual sealed Engineering transcript, not packet bytes: {message}"
        );
        assert_eq!(
            quality.outcome.session.terminal.cost,
            factory_protocol::TerminalCostV1::Known(MicroUsd::new(10))
        );
        assert_eq!(
            quality.outcome.session.terminal.session_state,
            factory_protocol::SessionState::Succeeded,
            "Quality terminal proves its packet/required-read gate before mutation",
        );
        assert_eq!(
            fs::read_to_string(quality.outcome.workspace.path().join("reproduce.sh"))
                .expect("Quality exploratory edit remains in its disposable workspace"),
            "#!/bin/sh\nprintf 'review-only edit\\n'\n",
            "the review actor must actually dirty its isolated workspace",
        );
        let quality_workspace = quality.outcome.workspace.path().to_owned();
        self.cleanup_actor(quality);
        assert!(
            !quality_workspace.exists(),
            "Quality's exploratory edit must be discarded with its exact disposable workspace",
        );

        assert!(matches!(
            self.driver
                .run_next(&self.daemon)
                .await
                .expect("resident Architect gate pass"),
            CampaignDriverOutcome::AwaitingArchitect { .. }
        ));
        // This is a read of the driver's durable next action, not an actor
        // supplied candidate identity. It supplies the external Architect's
        // public transition endpoint with the candidate selected by custody.
        let (candidate_status, review_id) =
            awaiting_architect_action(&self.store, self.campaign.campaign_id).await;
        let candidate = candidate_status.candidate_id;
        let decision = self
            .resolver
            .resolve_candidate_decision(
                candidate,
                review_id,
                ExpectedRevision::new(candidate_status.candidate_revision),
            )
            .await
            .expect("durable decision revisions");
        let architect_rationale = self
            .seal_kernel(
                "architect-deliver-rationale",
                b"independent Quality accepted\n",
            )
            .await;
        self.store
            .decision_store()
            .decide_candidate(&DecideCandidate {
                command_id: unique("architect-deliver"),
                expected_candidate_revision: decision.expected_candidate_revision,
                expected_attempt_revision: decision.expected_attempt_revision,
                expected_ticket_revision: decision.expected_ticket_revision,
                request: factory_protocol::CandidateDecisionRequestV1 {
                    candidate_id: candidate,
                    review_id,
                    decision: factory_protocol::CandidateDecisionV1::Deliver,
                    rationale: architect_rationale.reference(),
                    quality_rejection_override: None,
                    principal: ArchitectPrincipalV1::parse("architect").expect("architect"),
                },
            })
            .await
            .expect("Architect delivers accepted candidate");
        assert!(matches!(
            self.driver
                .run_next(&self.daemon)
                .await
                .expect("resident guarded local delivery"),
            CampaignDriverOutcome::Delivered
        ));
        assert!(matches!(
            self.driver
                .run_next(&self.daemon)
                .await
                .expect("resident post-delivery quiescence"),
            CampaignDriverOutcome::NoRunningCampaign
        ));

        let campaign = self
            .process
            .campaign_status(self.campaign.campaign_id)
            .await
            .expect("completed campaign status");
        assert_eq!(campaign.state, factory_protocol::CampaignState::Completed);
        assert_eq!(campaign.aggregate_budget, MicroUsd::new(500_000));
        assert_eq!(
            campaign.measured_cost,
            factory_protocol::TerminalCostV1::Known(MicroUsd::new(30))
        );
        let costs = self
            .process
            .campaign_session_costs(self.campaign.campaign_id, None, 10)
            .await
            .expect("bounded cost breakdown");
        assert_eq!(costs.len(), 3);
        assert_eq!(
            costs.iter().map(|row| row.office).collect::<Vec<_>>(),
            vec![
                factory_protocol::Office::ProductResearch,
                factory_protocol::Office::Engineering,
                factory_protocol::Office::Quality,
            ],
        );
        assert!(costs.iter().all(|row| {
            row.cost == Some(factory_protocol::TerminalCostV1::Known(MicroUsd::new(10)))
        }));
        assert_eq!(
            git_stdout(&self.repository, &self.git_path, &["remote"]).trim(),
            ""
        );
        assert_eq!(
            git_stdout(&self.repository, &self.git_path, &["status", "--porcelain"]).trim(),
            ""
        );
        assert_eq!(
            git_stdout(
                &self.repository,
                &self.git_path,
                &["show", "HEAD:reproduce.sh"]
            ),
            "#!/bin/sh\nprintf 'expected\\n'\nprintf 'none\\n' >&2\n"
        );
        assert!(
            self.store
                .audit_is_consistent()
                .await
                .expect("durable audit/material consistency"),
            "all retained CAS and audit facts must remain mutually consistent",
        );
    }

    async fn reject_invalid_engineering_terminal_provenance(&self) {
        let product = self
            .expect_assignment("Product", self.driver.run_next(&self.daemon).await)
            .await;
        self.cleanup_actor(product);

        let ticket = self
            .store
            .ticket_store()
            .live_ticket_proposal_artifacts(self.application)
            .await
            .expect("live product proposal")
            .into_iter()
            .next()
            .expect("one unseeded ticket");
        let sponsorship_rationale = self
            .seal_kernel(
                "negative-architect-sponsor-rationale",
                b"valuable product fix\n",
            )
            .await;
        self.store
            .decision_store()
            .sponsor_ticket(&SponsorTicket {
                command_id: unique("negative-architect-sponsor"),
                expected_ticket_revision: ExpectedRevision::new(ticket_revision(&ticket)),
                decision: SponsorshipDecisionV1 {
                    ticket_revision_id: ticket.ticket_revision_id,
                    rationale: sponsorship_rationale.reference(),
                    principal: ArchitectPrincipalV1::parse("architect").expect("architect"),
                },
            })
            .await
            .expect("external sponsor");

        let engineering = self
            .expect_assignment(
                "claim and Engineering",
                self.driver.run_next(&self.daemon).await,
            )
            .await;
        let engineering_session_id = engineering.outcome.session.session.session_id;
        let status = self
            .store
            .ticket_store()
            .ticket_buffer_status(self.campaign.campaign_id)
            .await
            .expect("candidate attachment status");
        let action = status.downstream_action.expect("candidate attach action");
        assert_eq!(
            action.stage,
            factory_kernel::ticket_store::DownstreamActionStage::CandidateCommitAttachRequired
        );
        let candidate_ref = format!(
            "refs/heads/factory/{}/{}",
            ticket.ticket_id.get(),
            action.candidate_id.get()
        );
        self.assert_candidate_unattached(&candidate_ref).await;
        self.cleanup_actor(engineering);

        let inspection = sqlx::PgPool::connect(&test_database_url())
            .await
            .expect("focused terminal provenance inspection connection");
        let original = sqlx::query(
            "SELECT lifecycle, cost_state, cost_micro_usd,
                    required_read_expected_count, required_read_satisfied_count
               FROM factory.sessions WHERE id = $1",
        )
        .bind(engineering_session_id.get())
        .fetch_one(&inspection)
        .await
        .expect("successful Engineering terminal row");
        let lifecycle: i16 = original.get("lifecycle");
        let cost_state: i16 = original.get("cost_state");
        let cost_micro_usd: Option<i64> = original.get("cost_micro_usd");
        let expected_reads: Option<i32> = original.get("required_read_expected_count");
        let satisfied_reads: Option<i32> = original.get("required_read_satisfied_count");
        assert_eq!(lifecycle, 2, "fixture begins with a succeeded terminal");
        assert_eq!(cost_state, 0, "fixture begins with known cost");
        assert_eq!(expected_reads, Some(2));
        assert_eq!(satisfied_reads, Some(2));

        sqlx::query("UPDATE factory.sessions SET lifecycle = 3 WHERE id = $1")
            .bind(engineering_session_id.get())
            .execute(&inspection)
            .await
            .expect("simulate a non-succeeded Engineering terminal");
        self.assert_attachment_refused(
            action,
            "non-succeeded Engineering terminal",
            &candidate_ref,
        )
        .await;

        sqlx::query(
            "UPDATE factory.sessions
                SET lifecycle = $1, cost_state = 1, cost_micro_usd = NULL
              WHERE id = $2",
        )
        .bind(lifecycle)
        .bind(engineering_session_id.get())
        .execute(&inspection)
        .await
        .expect("simulate unknown Engineering cost");
        self.assert_attachment_refused(action, "unknown Engineering cost", &candidate_ref)
            .await;

        sqlx::query(
            "UPDATE factory.sessions
                SET cost_state = $1, cost_micro_usd = $2,
                    required_read_expected_count = NULL,
                    required_read_satisfied_count = NULL
              WHERE id = $3",
        )
        .bind(cost_state)
        .bind(cost_micro_usd)
        .bind(engineering_session_id.get())
        .execute(&inspection)
        .await
        .expect("simulate a missing required-read assertion summary");
        self.assert_attachment_refused(action, "missing required-read assertion", &candidate_ref)
            .await;

        sqlx::query(
            "UPDATE factory.sessions
                SET required_read_expected_count = $1,
                    required_read_satisfied_count = $2
              WHERE id = $3",
        )
        .bind(expected_reads)
        .bind(satisfied_reads)
        .bind(engineering_session_id.get())
        .execute(&inspection)
        .await
        .expect("restore the valid terminal summary");
        inspection.close().await;
    }

    async fn assert_attachment_refused(
        &self,
        action: factory_kernel::ticket_store::DownstreamActionContext,
        condition: &str,
        candidate_ref: &str,
    ) {
        let error = self
            .resolver
            .resume_candidate_commit_attach(action)
            .await
            .unwrap_err();
        assert!(
            error.contains(
                "candidate commit attachment requires a succeeded Engineering candidate terminal with known cost and complete required reads"
            ),
            "{condition} must fail at the terminal provenance gate, got: {error}",
        );
        self.assert_candidate_unattached(candidate_ref).await;
    }

    async fn assert_candidate_unattached(&self, candidate_ref: &str) {
        let status = self
            .store
            .ticket_store()
            .ticket_buffer_status(self.campaign.campaign_id)
            .await
            .expect("candidate status remains inspectable");
        assert_eq!(
            status
                .downstream_evidence
                .expect("candidate evidence")
                .candidate_commit,
            None,
            "refusal must not attach a candidate commit",
        );
        let ref_probe = Command::new(&self.git_path)
            .current_dir(&self.repository)
            .args(["rev-parse", "--verify", candidate_ref])
            .output()
            .expect("inspect candidate ref after refused attachment");
        assert!(
            !ref_probe.status.success(),
            "refusal must not create the candidate ref {candidate_ref}",
        );
    }

    fn cleanup_actor(&self, launched: ActorLaunch) {
        self.git
            .cleanup_worktree(launched.outcome.workspace)
            .expect("actor worktree cleanup");
        let _ = fs::remove_dir_all(launched.outcome.staging_root);
    }

    async fn seal_kernel(&self, label: &str, bytes: &[u8]) -> Sealed {
        let staging = self.cas.runtime_root().join(unique("kernel-artifact"));
        fs::create_dir_all(&staging).expect("kernel artifact staging");
        let name = "artifact";
        fs::write(staging.join(name), bytes).expect("kernel artifact bytes");
        let sealed = self
            .cas
            .adopt(&staging, Path::new(name))
            .expect("kernel artifact seal");
        let receipt = self
            .store
            .register_artifact(
                &self.cas,
                &RegisterArtifact {
                    principal: "kernel-test".to_owned(),
                    command_id: unique(label),
                    expected_kernel_build_revision: ExpectedRevision::new(
                        self.build.resulting_revision,
                    ),
                    kernel_build_id: self.kernel_build_id,
                    sealed,
                },
            )
            .await
            .expect("kernel artifact registration");
        Sealed {
            artifact_id: receipt.artifact_id,
            sealed,
        }
    }

    async fn close(self) {
        self.daemon.shutdown().await.expect("daemon shutdown");
        self.store.close().await;
        let _ = fs::remove_dir_all(&self.daemon_root);
        let _ = fs::remove_dir_all(&self.root);
    }
}

#[derive(Clone, Copy)]
struct Application {
    id: ApplicationRevisionId,
    revision: AggregateRevision,
}

#[derive(Clone, Copy)]
struct Sealed {
    artifact_id: factory_protocol::ArtifactId,
    sealed: CasArtifact,
}

impl Sealed {
    const fn reference(self) -> SealedArtifactReferenceV1 {
        SealedArtifactReferenceV1 {
            artifact_id: self.artifact_id,
            digest: self.sealed.digest(),
            byte_length: self.sealed.byte_length(),
        }
    }
}

struct ActorLaunch {
    outcome: AssignmentLaunchOutcome,
}

const ACTOR_SOURCE: &str = r#"
const io = await Deno.open('/dev/fd/0', { read: true, write: true });
const enc = new TextEncoder(); const dec = new TextDecoder(); let sequence = 0;
async function exact(n: number): Promise<Uint8Array> { const out = new Uint8Array(n); let at = 0; while (at < n) { const got = await io.read(out.subarray(at)); if (got === null) throw new Error('closed'); at += got; } return out; }
async function write(bytes: Uint8Array): Promise<void> { let at = 0; while (at < bytes.length) at += await io.write(bytes.subarray(at)); }
async function line(): Promise<string> { const bytes: number[] = []; for (;;) { const one = await exact(1); if (one[0] === 10) return dec.decode(new Uint8Array(bytes)); bytes.push(one[0]); } }
async function frame(): Promise<any> { const p = await exact(4); const n = new DataView(p.buffer).getUint32(0, false); return JSON.parse(dec.decode(await exact(n))); }
async function call(operation: string, fields: Record<string, unknown>): Promise<any> { const request_id = `actor-${++sequence}`; const body = enc.encode(JSON.stringify({protocol_version:1, request_id, operation, ...fields})); const p = new Uint8Array(4); new DataView(p.buffer).setUint32(0, body.length, false); await write(p); await write(body); const response = await frame(); if (response.request_id !== request_id || response.operation !== operation || response.error_code) throw new Error(`${operation}: ${response.error_code ?? 'identity'}: ${response.message ?? ''}`); return response; }
const b64 = (text: string) => btoa(text);
const admission = JSON.parse(await line()); const packetBytes = Uint8Array.from(atob(admission.packet_b64), c => c.charCodeAt(0)); const packet = JSON.parse(dec.decode(packetBytes));
const ROLE = packet.office === 'product_research' ? 'product' : packet.office === 'engineering' ? 'engineering' : 'quality';
const ATTEMPT = packet.ticket_attempt_id ?? 0;
await call('session.verify_packet', {packet_digest: admission.packet_digest, packet_bytes_b64: admission.packet_b64});
for (const required of packet.required_reads) { const read = await call('workspace.read', {repository_relative_path: required.path}); if (read.blake3 !== required.digest) throw new Error('required read mismatch'); }
const assignmentEvidence = new Map();
for (const evidence of packet.assignment_evidence) {
  const read = await call('artifact.read', {artifact_id:evidence.artifact_id, expected_digest:evidence.digest});
  if (read.digest !== evidence.digest || read.byte_length !== evidence.byte_length) throw new Error('assignment evidence mismatch');
  assignmentEvidence.set(evidence.role, dec.decode(Uint8Array.from(atob(read.content_base64), c => c.charCodeAt(0))));
}
async function seal(name: string, text: string) { await Deno.writeTextFile(`${packet.workspace_root}/${name}`, text); const r = await call('artifact.seal_workspace_file', {client_command_id:`seal-${name}`, expected_revision:admission.session_revision, workspace_relative_path:name, byte_limit:131072}); return {artifact_id:r.artifact_id,digest:r.digest,byte_length:r.byte_length}; }
if (ROLE === 'product') {
  const narrative = await seal('narrative.md', 'synthetic behavior remains incorrect\n');
  const evidence = await seal('evidence.md', 'reproducer proves observable divergence\n');
  const command = await seal('reproducer.json', REPRODUCER);
  const expectedOut = await seal('expected.out', 'expected\n'); const expectedErr = await seal('expected.err', 'none\n');
  const actualOne = await seal('actual-one.out', 'actual\n'); const actualOneErr = await seal('actual-one.err', 'none\n');
  const actualTwo = await seal('actual-two.out', 'actual\n'); const actualTwoErr = await seal('actual-two.err', 'none\n');
  await call('product.submit_ticket', {client_command_id:'submit-ticket', expected_revision:admission.session_revision, title:'Repair exact synthetic output', mission_value:'prove one correct local change', scope:'reproduce.sh only', contract_owner:'product', risk:'low', narrative, evidence, acceptance_criteria:['reproducer prints expected'], contract_reads:[{path:'CONTRACT.md',reason:'repository product contract'}], duplicate_search:{query:'synthetic exact output',limit:5}, reproducer_profile:'reproduce', reproducer:{comparison_rule_version:1, command, stdin:null, expected_observation:{exit_status:0,stdout:expectedOut,stderr:expectedErr}, first_observation:{exit_status:0,stdout:actualOne,stderr:actualOneErr}, second_observation:{exit_status:0,stdout:actualTwo,stderr:actualTwoErr}}});
}
if (ROLE === 'engineering') {
  const ticketProposalBytes = assignmentEvidence.get('ticket_proposal');
  if (ticketProposalBytes === undefined) throw new Error('Engineering packet omitted the ticket proposal evidence');
  const ticketProposal = JSON.parse(ticketProposalBytes);
  if (typeof ticketProposal.reproducer_profile !== 'string') throw new Error('Engineering ticket proposal lacks reproducer semantics');
  const regressionCommand = ticketProposal.reproducer_profile;
  const expectedFailure = `ticket-attempt-${ATTEMPT}-${regressionCommand}`;
  await Deno.writeTextFile(`${packet.workspace_root}/regression-expected.txt`, 'expected\n');
  await call('candidate.checkpoint_regression', {client_command_id:'checkpoint', expected_revision:admission.session_revision, regression_command:regressionCommand, expected_failure:expectedFailure});
  await Deno.writeTextFile(`${packet.workspace_root}/reproduce.sh`, '#!/bin/sh\nprintf \'expected\\n\'\nprintf \'none\\n\' >&2\n'); await Deno.chmod(`${packet.workspace_root}/reproduce.sh`, 0o755);
  const report = await seal('engineering-report.md', 'changed reproducer after kernel checkpoint\n'); const risks = await seal('engineering-risks.md', 'none\n');
  await call('candidate.submit', {client_command_id:'candidate-submit', expected_revision:admission.session_revision, engineering_report:report, commit_subject:'Repair synthetic reproducer output', commit_body:'', regression_test_identity:'reproduce', risks});
}
if (ROLE === 'quality') {
  const suite = await call('quality.run_full_suite', {client_command_id:'quality-suite', expected_revision:admission.session_revision, validation_profile:'full'});
  await Deno.writeTextFile(`${packet.workspace_root}/reproduce.sh`, '#!/bin/sh\nprintf \'review-only edit\\n\'\n');
  const rationale = await seal('quality-rationale.md', 'independent complete suite passed\n'); const risks = await seal('quality-risks.md', 'none\n'); const probes = await seal('quality-probes.md', 'fresh exact candidate tree\n');
  await call('quality.submit_review', {client_command_id:'quality-review', expected_revision:admission.session_revision, full_suite_validation_id:suite.validation_id, verdict:'accept', rationale, risks, additional_probes:probes});
}
const transcriptBytes = enc.encode(JSON.stringify({provider:'none',role:ROLE}) + '\n'); const gzip = new Blob([transcriptBytes]).stream().pipeThrough(new CompressionStream('gzip')); await Deno.writeFile(`${packet.staging_root}/session.ndjson.gz`, new Uint8Array(await new Response(gzip).arrayBuffer()));
const transcript = await call('session.seal_artifact', {client_command_id:'seal-transcript', expected_revision:admission.session_revision, staging_relative_path:'session.ndjson.gz', role:'pi_transcript_gzip', byte_limit:131072});
const terminal = ROLE === 'product' ? 'work_complete' : ROLE === 'engineering' ? 'candidate_submit' : 'quality_submit_review';
await call('session.submit_terminal', {client_command_id:'terminal', expected_revision:admission.session_revision, terminal_operation:terminal, terminal_payload_b64:b64('{"outcome":"complete"}'), transcript_artifact_id:transcript.artifact_id, input_tokens:1, output_tokens:1, cache_read_tokens:0, cache_write_tokens:0, reasoning_tokens:null, reported_cost_micro_usd:10, stop_reason:'completed'});
"#;

fn reproducer_wire() -> CommandWireV1 {
    CommandWireV1 {
        name: "reproduce".to_owned(),
        executable: ExecutableWireV1 {
            approved_tool: None,
            repository_path: Some("reproduce.sh".to_owned()),
        },
        argv: Vec::new(),
        working_directory: ".".to_owned(),
        environment: Vec::new(),
        timeout_millis: 10_000,
        stdout_byte_limit: 4096,
        stderr_byte_limit: 4096,
        expected_exit_status: 0,
    }
}
fn validation_wire() -> CommandWireV1 {
    CommandWireV1 {
        name: "full".to_owned(),
        executable: ExecutableWireV1 {
            approved_tool: None,
            repository_path: Some("validate.sh".to_owned()),
        },
        argv: Vec::new(),
        working_directory: ".".to_owned(),
        environment: Vec::new(),
        timeout_millis: 10_000,
        stdout_byte_limit: 4096,
        stderr_byte_limit: 4096,
        expected_exit_status: 0,
    }
}

async fn admit_application(
    store: &KernelStore,
    cas: &CasStore,
    build: &factory_kernel::storage::KernelBuildReceipt,
    kernel_build_id: factory_protocol::KernelBuildId,
    repository_key: &str,
    repository: &Path,
) -> Application {
    let root = cas.runtime_root().join(unique("application"));
    fs::create_dir_all(&root).unwrap();
    let names = [
        "mission.md",
        "product-system.md",
        "product-assignment.md",
        "engineering-system.md",
        "engineering-assignment.md",
        "quality-system.md",
        "quality-assignment.md",
    ];
    let mut templates = Vec::new();
    for name in names {
        let bytes = format!("{name}\n");
        fs::write(root.join(name), &bytes).unwrap();
        templates.push((name, ContentDigest::of_bytes(bytes.as_bytes())));
    }
    let key = unique("full-vertical-app");
    let bundle = application_bundle(&key, repository_key, repository, &templates);
    fs::write(root.join("bundle.json"), bundle).unwrap();
    let admitted = store
        .admit_compiled_application(
            cas,
            &AdmitCompiledApplication {
                principal: "architect".to_owned(),
                command_id: unique("admit-application"),
                expected_revision: ExpectedRevision::new(AggregateRevision::initial()),
                expected_kernel_build_revision: ExpectedRevision::new(build.resulting_revision),
                kernel_build_id,
                source_root: root,
                bundle_relative_path: "bundle.json".into(),
            },
        )
        .await
        .unwrap();
    let rationale = register_bytes(
        store,
        cas,
        build,
        kernel_build_id,
        "activation-rationale",
        b"activate exact application\n",
    )
    .await;
    store
        .activate_application_revision(&ActivateApplicationRevision {
            principal: ArchitectPrincipalV1::parse("architect").unwrap(),
            command_id: unique("activate-application"),
            expected_revision: ExpectedRevision::new(admitted.resulting_revision),
            application_key: ApplicationKey::parse(key).unwrap(),
            application_revision_id: admitted.application_revision_id,
            rationale: SealedArtifactReferenceV1 {
                artifact_id: rationale.artifact_id,
                digest: rationale.sealed.digest(),
                byte_length: rationale.sealed.byte_length(),
            },
        })
        .await
        .unwrap();
    Application {
        id: admitted.application_revision_id,
        revision: admitted.resulting_revision,
    }
}

fn application_bundle(
    key: &str,
    repository_key: &str,
    repository: &Path,
    templates: &[(&str, ContentDigest)],
) -> String {
    let template = |i: usize| TemplateWireV1 {
        source_path: templates[i].0.to_owned(),
        digest: templates[i].1.to_hex(),
        placeholders: Vec::new(),
        rendered_byte_limit: 4096,
    };
    let office = |office: &str, system: usize, assignment: usize, tools: Vec<&str>| OfficeWireV1 {
        office: office.to_owned(),
        system_template: template(system),
        assignment_template: template(assignment),
        tools: tools.into_iter().map(str::to_owned).collect(),
        model: ModelWireV1 {
            // The installed receipt deliberately exercises the MVP's one
            // provider descriptor. The value below is an inert test string;
            // the qualified local host never calls a provider.
            provider: "openrouter".to_owned(),
            model_id: "provider-free".to_owned(),
            thinking_level: "none".to_owned(),
            context_token_limit: 100,
            output_token_limit: 100,
            price_input_micro_usd_per_million_tokens: 1,
            price_output_micro_usd_per_million_tokens: 1,
            price_cache_read_micro_usd_per_million_tokens: 1,
            price_cache_write_micro_usd_per_million_tokens: 1,
            capability_flags: Vec::new(),
        },
        limits: LimitsWireV1 {
            turn_limit: 5,
            wall_limit_millis: 30_000,
            output_byte_limit: 128 * 1024,
        },
    };
    canonical_application_bundle_json_v1(&ApplicationBundleWireV1 {
        format_version: 1,
        application_key: key.to_owned(),
        predecessor_bundle: None,
        repository: RepositoryWireV1 {
            repository_key: repository_key.to_owned(),
            canonical_local_path: repository.to_string_lossy().into_owned(),
            default_branch: "main".to_owned(),
            delivery_mode: "local_fast_forward_only".to_owned(),
        },
        mission_template: template(0),
        office_profiles: vec![
            office(
                "product_research",
                1,
                2,
                vec![
                    "workspace_read",
                    "artifact_seal",
                    "artifact_read",
                    "product_submit_ticket",
                ],
            ),
            office(
                "engineering",
                3,
                4,
                vec![
                    "workspace_read",
                    "artifact_seal",
                    "artifact_read",
                    "candidate_checkpoint_regression",
                    "candidate_submit",
                ],
            ),
            office(
                "quality",
                5,
                6,
                vec![
                    "workspace_read",
                    "artifact_seal",
                    "artifact_read",
                    "quality_run_full_suite",
                    "quality_submit_review",
                ],
            ),
        ],
        ticket_policy: TicketPolicyWireV1 {
            low_water: 1,
            target: 1,
            maximum: 1,
            proposal_maximum: 1,
            ticket_bounds: TicketBoundsWireV1 {
                narrative_byte_limit: 4096,
                acceptance_criteria_limit: 4,
                contract_read_limit: 2,
            },
        },
        required_reads: vec![factory_protocol::RequiredReadWireV1 {
            path: "AGENTS.md".to_owned(),
            reason: "application contract".to_owned(),
        }],
        reproducer_profiles: vec![reproducer_wire()],
        validation_profiles: ValidationWireV1 {
            focused: vec![reproducer_wire()],
            full: vec![validation_wire()],
        },
        git_policy: GitWireV1 {
            forbidden_paths: Vec::new(),
            delivery_mode: "local_fast_forward_only".to_owned(),
            provenance_trailers_required: true,
        },
        commit_message_policy: CommitMessageWireV1 {
            subject_byte_limit: 120,
            body_byte_limit: 8192,
        },
    })
    .unwrap()
}

async fn install_build(
    store: &KernelStore,
    root: &Path,
    credential_environment: &str,
) -> (
    CasStore,
    factory_kernel::storage::KernelBuildReceipt,
    InstalledKernelBuildReceiptV1,
) {
    let cas = CasStore::new_with_seed(root.join("cas"), 4 * 1024 * 1024, unique_number()).unwrap();
    let source_root = root.join("kernel-source");
    fs::create_dir_all(&source_root).unwrap();
    fs::write(
        source_root.join("kernel.rs"),
        b"provider-free synthetic kernel source\n",
    )
    .unwrap();

    // This installed host is deliberately one static, fully qualified Deno
    // graph. It behaves as the fake Pi provider only after the materializer
    // has verified its frozen runtime identity; no assignment chooses a host.
    let host_root = root.join("installed-fake-host");
    // The closed host root contains executable local modules only. Config,
    // lock and cache remain separate installation
    // material so the source inventory cannot silently omit any host import.
    let runtime_material = root.join("runtime-material");
    let cache = runtime_material.join("deno-cache");
    fs::create_dir_all(&host_root).unwrap();
    fs::create_dir_all(&cache).unwrap();
    fs::create_dir_all(&runtime_material).unwrap();
    fs::write(
        host_root.join("provider-free-host.ts"),
        format!(
            "const REPRODUCER = {};\n{ACTOR_SOURCE}",
            js_string(&canonical_command_profile_json_v1(&reproducer_wire()).unwrap())
        ),
    )
    .unwrap();
    fs::write(
        runtime_material.join("deno.json"),
        "{\"lock\":{\"path\":\"./deno.lock\",\"frozen\":true},\"nodeModulesDir\":\"none\"}\n",
    )
    .unwrap();
    fs::write(
        runtime_material.join("deno.lock"),
        "{\"version\":\"5\",\"specifiers\":{},\"jsr\":{},\"npm\":{},\"remote\":{}}\n",
    )
    .unwrap();
    // Installed runtime qualification needs a safe, non-actor module for its
    // frozen cache/typecheck probe. Keeping it in the same closed source
    // inventory prevents the test fixture from bypassing that production
    // preflight.
    fs::write(host_root.join("cache-probe.ts"), "export {};\n").unwrap();
    let runtime = InstalledRuntimeManifest::qualify(InstalledRuntimeQualification {
        deno_executable: deno(),
        host_source_root: host_root.clone(),
        host_entrypoint: host_root.join("provider-free-host.ts"),
        deno_config: runtime_material.join("deno.json"),
        deno_lock: runtime_material.join("deno.lock"),
        deno_dir: cache,
        host_source_files: vec![
            RuntimeRelativePath::parse("provider-free-host.ts").expect("fake host source path"),
            RuntimeRelativePath::parse("cache-probe.ts").expect("fake cache probe path"),
        ],
        cache_probe_module: RuntimeRelativePath::parse("cache-probe.ts")
            .expect("fake cache probe module"),
        pi_version: "0.84.1".to_owned(),
    })
    .expect("qualify static provider-free host graph");
    let source = qualify_kernel_source_v1(
        &source_root,
        &[RuntimeRelativePath::parse("kernel.rs").expect("kernel source path")],
    )
    .expect("qualify synthetic kernel source");
    let binary = qualify_kernel_binary_v1(&deno()).expect("qualify installed kernel binary");
    let cargo = std::env::var_os("CARGO")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("/opt/homebrew/opt/rustup/bin/cargo"));
    let approved_tools = InstalledApprovedToolsQualificationV1::qualify(&cargo, &system_git())
        .expect("qualify installed deterministic tools");
    let installed = InstalledKernelBuildReceiptV1::from_qualifications(
        SCHEMA_IDENTITY.to_owned(),
        source,
        binary,
        approved_tools,
        runtime,
        credential_environment.to_owned(),
    )
    .expect("construct qualified installed build receipt");
    let status = store.kernel_build_status().await.unwrap();
    let receipt = store
        .install_qualified_kernel_build(
            &cas,
            &InstallQualifiedKernelBuild {
                principal: "operator".to_owned(),
                command_id: unique("install-build"),
                expected_revision: ExpectedRevision::new(status.aggregate_revision),
                receipt: installed.clone(),
            },
        )
        .await
        .unwrap();
    let recovered = store
        .load_current_installed_runtime(&cas)
        .await
        .expect("load installed runtime")
        .expect("one installed runtime");
    assert_eq!(recovered.kernel_build_id(), installed.kernel_build_id());
    (cas, receipt, recovered)
}
async fn register_bytes(
    store: &KernelStore,
    cas: &CasStore,
    build: &factory_kernel::storage::KernelBuildReceipt,
    kernel_build_id: factory_protocol::KernelBuildId,
    label: &str,
    bytes: &[u8],
) -> Sealed {
    let staging = cas.runtime_root().join(unique("register-bytes"));
    fs::create_dir_all(&staging).unwrap();
    let name = "artifact";
    fs::write(staging.join(name), bytes).unwrap();
    let sealed = cas.adopt(&staging, Path::new(name)).unwrap();
    let receipt = store
        .register_artifact(
            cas,
            &RegisterArtifact {
                principal: "kernel-test".to_owned(),
                command_id: unique(label),
                expected_kernel_build_revision: ExpectedRevision::new(build.resulting_revision),
                kernel_build_id,
                sealed,
            },
        )
        .await
        .unwrap();
    Sealed {
        artifact_id: receipt.artifact_id,
        sealed,
    }
}
fn ticket_revision(
    ticket: &factory_kernel::ticket_store::LiveTicketProposalArtifact,
) -> AggregateRevision {
    match ticket.state {
        factory_protocol::TicketState::Proposed => AggregateRevision::initial(),
        _ => panic!("product ticket must be proposed"),
    }
}

impl Fixture {
    async fn expect_assignment(
        &self,
        action: &str,
        result: Result<CampaignDriverOutcome, factory_kernel::campaign_driver::CampaignDriverError>,
    ) -> ActorLaunch {
        match result.unwrap_or_else(|error| panic!("resident {action} pass failed: {error}")) {
            CampaignDriverOutcome::Assignment(outcome) => ActorLaunch { outcome },
            CampaignDriverOutcome::CampaignFailed {
                campaign_id,
                failure_detail,
            } => panic!(
                "resident {action} failed campaign {} before assignment: {failure_detail}",
                campaign_id.get()
            ),
            CampaignDriverOutcome::TicketAttemptFailed {
                campaign_id,
                ticket_attempt_id,
                failure_detail,
            } => panic!(
                "resident {action} failed campaign {} ticket attempt {} before assignment: {failure_detail}",
                campaign_id.get(),
                ticket_attempt_id.get()
            ),
            _ => panic!("resident {action} pass did not launch an assignment"),
        }
    }
}

async fn awaiting_architect_action(
    store: &KernelStore,
    campaign_id: factory_protocol::CampaignId,
) -> (
    factory_kernel::ticket_store::DownstreamActionContext,
    factory_protocol::ReviewId,
) {
    let scheduler = store.ticket_scheduler();
    match scheduler
        .next_action(campaign_id)
        .await
        .expect("scheduler after independent Quality")
    {
        SchedulerNextAction::ContinueDownstream(action)
            if action.stage.name() == "awaiting_architect" =>
        {
            let review = store
                .ticket_store()
                .ticket_buffer_status(campaign_id)
                .await
                .expect("durable Quality review evidence")
                .downstream_evidence
                .and_then(|evidence| evidence.review)
                .expect("accepted Quality review bound to candidate");
            assert_eq!(review.verdict.name(), "accept");
            (action, review.review_id)
        }
        other => panic!("expected durable Architect action, got {other:?}"),
    }
}
fn test_database_url() -> String {
    let url = std::env::var("FACTORY_TEST_DATABASE_URL").expect("FACTORY_TEST_DATABASE_URL");
    let name = url
        .rsplit('/')
        .next()
        .and_then(|part| part.split('?').next())
        .expect("database name");
    assert!(name.strip_prefix("factory_test_v3_").is_some_and(
        |suffix| !suffix.is_empty() && suffix.bytes().all(|byte| byte.is_ascii_digit())
    ));
    url
}

async fn reset_fixture_schema() {
    let pool = sqlx::PgPool::connect(&test_database_url())
        .await
        .expect("fixture database connection");
    sqlx::query("DROP SCHEMA IF EXISTS factory CASCADE")
        .execute(&pool)
        .await
        .expect("reset explicitly guarded fixture schema");
    // Reapply the canonical schema before `migrate_and_verify` checks its
    // identity; this is the same checked-in migration path as production.
    sqlx::query("DROP TABLE IF EXISTS _sqlx_migrations")
        .execute(&pool)
        .await
        .expect("reset fixture migration ledger");
    FIXTURE_MIGRATOR
        .run(&pool)
        .await
        .expect("restore fixture schema through canonical migration");
    pool.close().await;
}

fn deno() -> PathBuf {
    for candidate in ["/opt/homebrew/bin/deno", "/usr/local/bin/deno"] {
        let path = PathBuf::from(candidate);
        if path.is_file() {
            return path;
        }
    }
    panic!("real Deno required")
}
fn system_git() -> PathBuf {
    for candidate in ["/usr/bin/git", "/opt/homebrew/bin/git"] {
        let path = PathBuf::from(candidate);
        if path.is_file() {
            return path;
        }
    }
    panic!("git required")
}
fn git(cwd: &Path, git: &Path, args: &[&str]) {
    let status = Command::new(git)
        .args(args)
        .current_dir(cwd)
        .status()
        .unwrap();
    assert!(status.success(), "git {:?}", args)
}
fn git_stdout(cwd: &Path, git: &Path, args: &[&str]) -> String {
    let out = Command::new(git)
        .args(args)
        .current_dir(cwd)
        .output()
        .unwrap();
    assert!(out.status.success(), "git {:?}", args);
    String::from_utf8(out.stdout).unwrap()
}
fn write_script(path: &Path, contents: &str) {
    fs::write(path, contents).unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mut permissions = fs::metadata(path).unwrap().permissions();
        permissions.set_mode(0o755);
        fs::set_permissions(path, permissions).unwrap();
    }
}
fn js_string(value: &str) -> String {
    format!("{:?}", value)
}
fn unique(prefix: &str) -> String {
    format!("{prefix}-{}", unique_number())
}
fn unique_number() -> u64 {
    (u64::from(std::process::id()) << 32) | NEXT.fetch_add(1, Ordering::Relaxed)
}
