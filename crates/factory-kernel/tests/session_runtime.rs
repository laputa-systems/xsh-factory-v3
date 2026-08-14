//! Provider-free end-to-end judges for the daemon-owned Deno session boundary.
//!
//! These judges deliberately use a real, pinned Deno executable but no Pi SDK
//! import and no provider credential.  The generated actor speaks the inherited
//! FD-0 protocol itself, which keeps this test focused on the authority path:
//! a canonical packet is verified, an exact workspace read is observed, a
//! gzip transcript is sealed, and one typed terminal claim becomes durable.

use std::{
    ffi::OsString,
    fs,
    path::{Path, PathBuf},
    sync::atomic::{AtomicU64, Ordering},
    time::Duration,
};

use factory_kernel::{
    cas::{CasArtifact, CasStore},
    command_supervision::{ApprovedToolExecutables, CommandRunner, ExactExecutable},
    forum_store::ForumStore,
    local_transport::{LocalDaemon, LocalTransportConfig, OperatorClient},
    process::{CancelCampaign, CreateAssignment, ProcessStore, StartCampaign},
    process_custody::{PiHostSpawnSpec, ProcessSupervisionSpec},
    publication_store::PublicationStore,
    session_runtime::{
        SessionLaunchRequest, SessionRuntimeError, SessionRuntimeVerifier, launch_session,
    },
    storage::{
        ActivateApplicationRevision, AdmitCompiledApplication, InstallKernelBuild, KernelStore,
        RegisterArtifact, RegisterRepository, SCHEMA_IDENTITY,
    },
    ticket_store::TicketStore,
};
use factory_protocol::{
    ASSIGNMENT_PACKET_V1_FORMAT, AbsoluteHostPath, AggregateRevision, ApplicationKey,
    ApplicationRevisionId, ArchitectPrincipalV1, ArtifactId, AssignmentPacketV1, AssignmentRole,
    ContentDigest, CredentialDescriptorV1, ExpectedRevision, MicroUsd, ModelProfileV1,
    OperatorCancelCampaignRequest, PROTOCOL_VERSION_V1, ReadExactFileV1, ReadObservationV1,
    RepositoryRelativePath, RuntimeIdentityV1, SealedArtifactReferenceV1, SessionLimitsV1,
    TerminalCostV1, TerminalOperationV1,
};

static NEXT_TEST: AtomicU64 = AtomicU64::new(1);

#[test]
#[ignore = "requires FACTORY_TEST_DATABASE_URL for a disposable PostgreSQL 18 database"]
fn real_deno_fake_actor_commits_complete_known_cost_session_provenance() {
    smol::block_on(async {
        let fixture = RuntimeFixture::new(FakeTerminalMode::Completed).await;
        let before = fixture
            .process
            .process_fact_counts()
            .await
            .expect("facts before launch");

        let outcome = fixture.launch().await.expect("fake actor launch");
        assert_eq!(
            outcome.terminal.session_state,
            factory_protocol::SessionState::Succeeded
        );
        assert_eq!(
            outcome.terminal.cost,
            TerminalCostV1::Known(MicroUsd::new(7))
        );
        assert_eq!(
            outcome.process.reason,
            factory_kernel::process_custody::ProcessStopReason::Cancelled
        );

        let status = fixture
            .process
            .session_status(outcome.session.session_id)
            .await
            .expect("durable session status");
        assert_eq!(status.state, factory_protocol::SessionState::Succeeded);
        assert_eq!(status.cost, Some(TerminalCostV1::Known(MicroUsd::new(7))));
        let campaign = fixture
            .process
            .campaign_status(fixture.campaign_id)
            .await
            .expect("durable campaign status");
        assert_eq!(
            campaign.measured_cost,
            TerminalCostV1::Known(MicroUsd::new(7))
        );
        assert_eq!(campaign.state, factory_protocol::CampaignState::Running);
        let audits_before_breakdown = fixture
            .process
            .process_audit_count()
            .await
            .expect("audit count before cost breakdown");
        let cost_rows = fixture
            .process
            .campaign_session_costs(fixture.campaign_id, None, 20)
            .await
            .expect("bounded session cost breakdown");
        assert_eq!(cost_rows.len(), 1);
        assert_eq!(
            cost_rows[0].assignment_role,
            AssignmentRole::ProductResearch
        );
        assert_eq!(cost_rows[0].model_provider, "fake");
        assert_eq!(
            cost_rows[0].cost,
            Some(TerminalCostV1::Known(MicroUsd::new(7)))
        );
        assert_eq!(
            fixture
                .process
                .process_audit_count()
                .await
                .expect("audit count after cost breakdown"),
            audits_before_breakdown
        );

        // The host prints the artifact receipt before proposing terminal
        // state. That output is daemon-owned capture evidence, not an actor
        // supplied database reference.
        let stdout = fs::read_to_string(&fixture.stdout_path).expect("captured actor stdout");
        let transcript_id = stdout
            .lines()
            .find_map(|line| line.strip_prefix("FAKE_TRANSCRIPT_ARTIFACT_ID="))
            .and_then(|value| value.parse::<i64>().ok())
            .and_then(|value| ArtifactId::new(value).ok())
            .expect("fake actor transcript receipt in stdout capture");
        let transcript = fixture
            .process
            .registered_artifact(&fixture.build.cas, transcript_id)
            .await
            .expect("registered transcript");
        let transcript_bytes = fixture
            .build
            .cas
            .read(transcript.digest())
            .expect("sealed transcript bytes");
        assert!(transcript_bytes.starts_with(&[0x1f, 0x8b]));

        // One actor-directed staging seal plus terminal reconciliation may
        // add at most five durable artifact facts—proposal evidence, gzip
        // transcript, stdout, stderr, and the one read assertion. Identical
        // content reuses an existing artifact row. The session-start and
        // session-terminal process audit records remain exact; the actor's
        // event stream never becomes PostgreSQL rows.
        let after = fixture
            .process
            .process_fact_counts()
            .await
            .expect("facts after launch");
        assert_eq!(after.0, before.0);
        assert_eq!(after.1, before.1 + 1);
        assert!(
            (1..=5).contains(&(after.2 - before.2)),
            "only the bounded named transcript/stream/assertion artifacts may be added: before={before:?}, after={after:?}"
        );
        assert_eq!(after.3, before.3 + 2);

        // A completed session alone does not finish a campaign in the MVP.
        // Close this test's otherwise-running campaign through the same typed
        // operator transition so later independent PostgreSQL judges have no
        // leaked singleton campaign state.
        fixture
            .process
            .cancel_campaign(&CancelCampaign {
                principal: "architect".to_owned(),
                command_id: unique("runtime-cleanup-campaign"),
                expected_revision: ExpectedRevision::new(campaign.revision),
                campaign_id: fixture.campaign_id,
            })
            .await
            .expect("typed test campaign cancellation");

        fixture.close().await;
    });
}

#[test]
#[ignore = "requires FACTORY_TEST_DATABASE_URL for a disposable PostgreSQL 18 database"]
fn operator_cancellation_stops_and_reconciles_the_exact_live_fake_actor() {
    smol::block_on(async {
        let fixture = RuntimeFixture::new(FakeTerminalMode::WaitForCancellation).await;
        let socket = fixture.daemon.operator_socket_path().to_owned();
        let expected_revision = fixture
            .process
            .campaign_status(fixture.campaign_id)
            .await
            .expect("running campaign")
            .revision;
        let launch = fixture.launch();
        let cancel = async {
            let mut running_session = None;
            for _ in 0..200 {
                if let Some(row) = fixture
                    .process
                    .campaign_session_costs(fixture.campaign_id, None, 1)
                    .await
                    .expect("active session observation")
                    .into_iter()
                    .next()
                    .filter(|row| row.outcome == factory_protocol::SessionState::Running)
                {
                    running_session = Some(row.session_id);
                    break;
                }
                smol::Timer::after(Duration::from_millis(5)).await;
            }
            let session_id = running_session.expect("live fake actor session");
            let serve = fixture.daemon.serve_one_operator();
            let client = OperatorClient::new(socket);
            let request = client.cancel_campaign(OperatorCancelCampaignRequest {
                protocol_version: PROTOCOL_VERSION_V1,
                request_id: unique("cancel-request"),
                operation: factory_protocol::OP_OPERATOR_CANCEL_CAMPAIGN.to_owned(),
                client_command_id: unique("cancel-command"),
                expected_revision: expected_revision.get(),
                campaign_id: fixture.campaign_id.get(),
                principal: "architect".to_owned(),
            });
            let (served, receipt) = smol::future::zip(serve, request).await;
            served.expect("operator cancellation served");
            let receipt = receipt.expect("operator cancellation accepted");
            assert_eq!(receipt.campaign_id, fixture.campaign_id.get());
            assert_eq!(receipt.aggregate_revision, expected_revision.get() + 2);
            session_id
        };
        let (outcome, session_id) = smol::future::zip(launch, cancel).await;
        let outcome = outcome.expect("cancelled fake actor reconciled");
        assert_eq!(outcome.session.session_id, session_id);
        assert_eq!(
            outcome.process.reason,
            factory_kernel::process_custody::ProcessStopReason::Cancelled
        );
        assert_eq!(
            outcome.terminal.session_state,
            factory_protocol::SessionState::Cancelled
        );
        assert_eq!(outcome.terminal.cost, TerminalCostV1::Unknown);
        let status = fixture
            .process
            .campaign_status(fixture.campaign_id)
            .await
            .expect("cancelled campaign status");
        assert_eq!(status.state, factory_protocol::CampaignState::Cancelled);
        assert_eq!(status.measured_cost, TerminalCostV1::Unknown);
        assert_eq!(status.failure_reason, None);
        assert!(
            Path::new(fixture.packet.staging_root.as_str())
                .join("session.ndjson")
                .is_file()
        );
        fixture.close().await;
    });
}

#[test]
#[ignore = "requires FACTORY_TEST_DATABASE_URL for a disposable PostgreSQL 18 database"]
fn real_deno_fake_actor_with_one_thousand_events_has_bounded_postgres_writes() {
    smol::block_on(async {
        let fixture = RuntimeFixture::new(FakeTerminalMode::CompletedWithThousandEvents).await;
        let before = fixture
            .process
            .process_fact_counts()
            .await
            .expect("facts before 1,000-event actor launch");

        let outcome = fixture
            .launch()
            .await
            .expect("1,000-event fake actor launch");
        assert_eq!(
            outcome.terminal.session_state,
            factory_protocol::SessionState::Succeeded
        );
        assert_eq!(
            outcome.terminal.cost,
            TerminalCostV1::Known(MicroUsd::new(7))
        );

        // The fixture builds exactly 1,000 separate NDJSON records before it
        // compresses and seals the ordinary transcript artifact. The daemon
        // need not parse or persist individual events to own the complete
        // transcript; the point of this judge is that there is no per-event
        // PostgreSQL write path.
        let stdout = fs::read_to_string(&fixture.stdout_path).expect("captured actor stdout");
        assert!(
            stdout
                .lines()
                .any(|line| line == "FAKE_TRANSCRIPT_EVENT_COUNT=1000"),
            "fixture did not create its exact 1,000-event transcript: {stdout}"
        );

        let after = fixture
            .process
            .process_fact_counts()
            .await
            .expect("facts after 1,000-event actor launch");
        assert_eq!(after.0, before.0, "an event must not create an assignment");
        assert_eq!(after.1, before.1 + 1, "one actor owns one session row");
        assert!(
            (1..=5).contains(&(after.2 - before.2)),
            "only the bounded named transcript/stream/assertion artifacts may be added: before={before:?}, after={after:?}"
        );
        assert_eq!(
            after.3,
            before.3 + 2,
            "only session-start and session-terminal receipts belong to the process fact count"
        );

        let campaign = fixture
            .process
            .campaign_status(fixture.campaign_id)
            .await
            .expect("campaign status before cleanup");
        fixture
            .process
            .cancel_campaign(&CancelCampaign {
                principal: "architect".to_owned(),
                command_id: unique("thousand-event-cleanup-campaign"),
                expected_revision: ExpectedRevision::new(campaign.revision),
                campaign_id: fixture.campaign_id,
            })
            .await
            .expect("typed test campaign cancellation");
        fixture.close().await;
    });
}

#[test]
#[ignore = "requires FACTORY_TEST_DATABASE_URL for a disposable PostgreSQL 18 database"]
fn real_deno_fake_actor_unknown_cost_fails_closed_without_a_resume() {
    smol::block_on(async {
        let fixture = RuntimeFixture::new(FakeTerminalMode::UnknownCost).await;
        let outcome = fixture.launch().await.expect("unknown-cost actor launch");

        assert_eq!(
            outcome.terminal.session_state,
            factory_protocol::SessionState::Failed
        );
        assert_eq!(outcome.terminal.cost, TerminalCostV1::Unknown);
        let campaign = fixture
            .process
            .campaign_status(fixture.campaign_id)
            .await
            .expect("failed campaign status");
        assert_eq!(campaign.measured_cost, TerminalCostV1::Unknown);
        assert_eq!(campaign.state, factory_protocol::CampaignState::Failed);

        // The first host was directly reaped after its one terminal claim.
        // The runtime has no resume path, and unknown cost closes further paid
        // admission at the campaign boundary.
        let mut second_command = fixture.start_command();
        second_command.expected_assignment_revision = ExpectedRevision::new(
            fixture
                .assignment
                .resulting_revision
                .next()
                .and_then(AggregateRevision::next)
                .expect("post-terminal assignment revision"),
        );
        let second = fixture.process.start_session(&second_command).await;
        assert!(
            matches!(
                &second,
                Err(
                    factory_kernel::storage::StoreError::AssignmentStateConflict { .. }
                        | factory_kernel::storage::StoreError::CampaignCostFrozen { .. }
                )
            ),
            "unexpected second-session result: {second:?}"
        );

        fixture.close().await;
    });
}

#[test]
#[ignore = "requires FACTORY_TEST_DATABASE_URL for a disposable PostgreSQL 18 database"]
fn oversized_unsealed_partial_transcript_is_bounded_and_terminally_reconciled() {
    smol::block_on(async {
        let fixture = RuntimeFixture::new(FakeTerminalMode::OversizedUnsealedTranscript).await;
        let outcome = fixture
            .launch()
            .await
            .expect("oversized unsealed transcript is reconciled");

        assert_eq!(
            outcome.terminal.session_state,
            factory_protocol::SessionState::Interrupted
        );
        let status = fixture
            .process
            .campaign_status(fixture.campaign_id)
            .await
            .expect("campaign after oversized transcript");
        assert_eq!(status.state, factory_protocol::CampaignState::Failed);
        assert_eq!(status.measured_cost, TerminalCostV1::Unknown);
        fixture.close().await;
    });
}

#[test]
#[ignore = "requires FACTORY_TEST_DATABASE_URL for a disposable PostgreSQL 18 database"]
fn product_mutation_before_daemon_read_writes_no_ticket_authority() {
    smol::block_on(async {
        let fixture = RuntimeFixture::new(FakeTerminalMode::ProductMutationBeforeRead).await;
        let before = fixture
            .tickets
            .ticket_buffer_status(fixture.campaign_id)
            .await
            .expect("buffer before rejected Product mutation");

        let outcome = fixture
            .launch()
            .await
            .expect("actor exits after rejected mutation");
        assert_eq!(
            outcome.terminal.session_state,
            factory_protocol::SessionState::Interrupted,
            "missing exact reads must not become a successful actor terminal"
        );
        assert_eq!(outcome.terminal.cost, TerminalCostV1::Unknown);
        let after = fixture
            .tickets
            .ticket_buffer_status(fixture.campaign_id)
            .await
            .expect("buffer after rejected Product mutation");
        assert_eq!(
            before.proposed_count, after.proposed_count,
            "rejected pre-read mutation created ticket authority"
        );

        let campaign = fixture
            .process
            .campaign_status(fixture.campaign_id)
            .await
            .expect("campaign after rejected actor");
        assert_eq!(campaign.state, factory_protocol::CampaignState::Failed);
        assert_eq!(campaign.measured_cost, TerminalCostV1::Unknown);
        assert_eq!(
            campaign.failure_reason.as_deref(),
            Some("terminal session cost is unknown")
        );
        fixture.close().await;
    });
}

#[derive(Clone, Copy)]
enum FakeTerminalMode {
    Completed,
    CompletedWithThousandEvents,
    UnknownCost,
    ProductMutationBeforeRead,
    OversizedUnsealedTranscript,
    WaitForCancellation,
}

impl FakeTerminalMode {
    const fn source_value(self) -> &'static str {
        match self {
            Self::Completed => "completed",
            Self::CompletedWithThousandEvents => "completed_thousand_events",
            Self::UnknownCost => "unknown_cost",
            Self::ProductMutationBeforeRead => "product_mutation_before_read",
            Self::OversizedUnsealedTranscript => "oversized_unsealed_transcript",
            Self::WaitForCancellation => "wait_for_cancellation",
        }
    }
}

struct RuntimeFixture {
    store: KernelStore,
    build: InstalledBuild,
    process: ProcessStore,
    forum: ForumStore,
    publications: PublicationStore,
    tickets: TicketStore,
    command_runner: CommandRunner,
    daemon: LocalDaemon,
    daemon_root: PathBuf,
    campaign_id: factory_protocol::CampaignId,
    assignment: factory_kernel::process::AssignmentReceipt,
    packet: AssignmentPacketV1,
    packet_bytes: Vec<u8>,
    packet_artifact: CasArtifact,
    stdout_path: PathBuf,
    request: SessionLaunchRequest,
}

impl RuntimeFixture {
    async fn new(mode: FakeTerminalMode) -> Self {
        let store = store().await;
        store.migrate_and_verify().await.expect("migration");
        let build = install_build(&store).await;
        let process = store.process_store();
        let forum = store.forum_store();
        let publications = store.publication_store();
        let tickets = store.ticket_store();
        let command_runner = CommandRunner::new(
            ApprovedToolExecutables::new(
                ExactExecutable::discover(std::env::var_os("CARGO").map_or_else(
                    || PathBuf::from("/opt/homebrew/opt/rustup/bin/cargo"),
                    PathBuf::from,
                ))
                .expect("exact Cargo executable"),
                ExactExecutable::discover("/usr/bin/git").expect("exact Git executable"),
                ExactExecutable::discover(deno_executable()).expect("exact Deno executable"),
            ),
            Duration::from_millis(100),
        )
        .expect("command runner");
        let repository_key = unique("runtime-repository");
        let repository_path = format!("/tmp/{}", unique("runtime-product"));
        store
            .register_repository(&RegisterRepository {
                principal: "architect".to_owned(),
                command_id: unique("runtime-repository"),
                expected_revision: ExpectedRevision::new(AggregateRevision::initial()),
                repository_key: repository_key.clone(),
                canonical_local_path: repository_path.clone(),
                default_branch: "main".to_owned(),
            })
            .await
            .expect("repository");
        let application =
            admit_application(&store, &build, &repository_key, &repository_path).await;
        let campaign = process
            .start_campaign(&StartCampaign {
                principal: "architect".to_owned(),
                command_id: unique("runtime-campaign"),
                expected_application_revision: ExpectedRevision::new(
                    AggregateRevision::from_persisted(1),
                ),
                application_revision_id: application,
                aggregate_budget: MicroUsd::new(100),
                deadline_unix_millis: 4_000_000_000_000,
                delivery_target: 1,
            })
            .await
            .expect("campaign");

        let workspace_root = build.cas.runtime_root().join(unique("runtime-workspace"));
        let staging_root = build.cas.runtime_root().join(unique("runtime-staging"));
        fs::create_dir_all(&workspace_root).expect("workspace root");
        fs::create_dir_all(&staging_root).expect("staging root");
        fs::write(
            workspace_root.join("AGENTS.md"),
            b"exact required workspace bytes",
        )
        .expect("required workspace file");
        let read_path = RepositoryRelativePath::parse("AGENTS.md").expect("required path");
        let observed = ReadObservationV1 {
            path: read_path.clone(),
            digest: ContentDigest::of_bytes(b"exact required workspace bytes"),
        };
        let required_manifest = seal_and_register(
            &store,
            &build,
            "runtime-required-manifest",
            &canonical_manifest(std::slice::from_ref(&observed)),
        )
        .await;
        let system_prompt =
            seal_and_register(&store, &build, "runtime-system-prompt", b"system prompt").await;
        let assignment_prompt = seal_and_register(
            &store,
            &build,
            "runtime-assignment-prompt",
            b"assignment prompt",
        )
        .await;

        let identity = process
            .reserve_assignment_identity()
            .await
            .expect("assignment identity");
        let deno = deno_executable();
        let mut packet = AssignmentPacketV1 {
            format_version: ASSIGNMENT_PACKET_V1_FORMAT,
            campaign_id: campaign.campaign_id,
            assignment_id: identity.assignment_id(),
            kernel_build_id: build.kernel_build_id,
            application_revision_id: application,
            assignment_role: AssignmentRole::ProductResearch,
            target: "provider-free fake actor".to_owned(),
            ticket_attempt_id: None,
            candidate_id: None,
            system_prompt_artifact_id: system_prompt.artifact_id,
            assignment_prompt_artifact_id: assignment_prompt.artifact_id,
            required_read_manifest_artifact_id: required_manifest.artifact_id,
            workspace_root: AbsoluteHostPath::parse(
                workspace_root.to_str().expect("utf8 workspace"),
            )
            .expect("absolute workspace"),
            staging_root: AbsoluteHostPath::parse(staging_root.to_str().expect("utf8 staging"))
                .expect("absolute staging"),
            model: model(),
            limits: SessionLimitsV1 {
                turn_limit: 1,
                wall_limit: factory_protocol::DurationMillis::new(10_000),
                output_byte_limit: 64 * 1024,
            },
            runtime: RuntimeIdentityV1 {
                deno_executable: AbsoluteHostPath::parse(deno.to_str().expect("utf8 deno"))
                    .expect("absolute deno"),
                deno_version: "2.9.4".to_owned(),
                source_graph_digest: digest(7_000),
                resolved_dependency_graph_digest: digest(7_001),
                deno_json_digest: digest(7_002),
                deno_lock_digest: digest(7_003),
                pi_version: "fake-provider-free".to_owned(),
                credential: CredentialDescriptorV1::Environment {
                    name: "FACTORY_FAKE_PROVIDER_KEY".to_owned(),
                },
            },
            required_reads: vec![ReadExactFileV1 {
                path: read_path,
                digest: observed.digest,
                reason: "authority contract".to_owned(),
            }],
            // Product has no upstream assignment evidence; the empty closure
            // is part of its exact office-specific packet contract.
            assignment_evidence: Vec::new(),
            terminal_operations: vec![TerminalOperationV1::WorkComplete],
            remaining_campaign_allowance: MicroUsd::new(100),
            revision: AggregateRevision::initial(),
            packet_digest: digest(7_004),
        };
        let mut wire = wire_packet(&packet, b"system prompt", b"assignment prompt");
        let packet_digest = factory_protocol::unsigned_assignment_packet_digest_v1(&wire)
            .expect("unsigned packet digest");
        wire.packet_digest = packet_digest.to_hex();
        packet.packet_digest = packet_digest;
        let packet_bytes = factory_protocol::canonical_assignment_packet_json_v1(&wire)
            .expect("canonical packet")
            .into_bytes();
        let packet_artifact =
            seal_and_register(&store, &build, "runtime-assignment-packet", &packet_bytes).await;
        let assignment = process
            .create_assignment(
                &build.cas,
                &CreateAssignment {
                    principal: "architect".to_owned(),
                    command_id: unique("runtime-assignment"),
                    expected_campaign_revision: ExpectedRevision::new(campaign.resulting_revision),
                    identity,
                    packet: packet.clone(),
                    packet_bytes: packet_bytes.clone(),
                    packet_artifact: packet_artifact.sealed,
                    required_read_manifest_artifact_id: required_manifest.artifact_id,
                    attempt_ordinal: 1,
                },
            )
            .await
            .expect("assignment");

        let host = staging_root.join("provider-free-fake-host.ts");
        fs::write(
            &host,
            format!(
                "const FAKE_TERMINAL_MODE = {:?};\n{FAKE_HOST_SOURCE}",
                mode.source_value()
            ),
        )
        .expect("fake Deno host source");
        let config = staging_root.join("deno.json");
        let lock = staging_root.join("deno.lock");
        fs::write(
            &config,
            "{\"lock\":{\"path\":\"./deno.lock\",\"frozen\":true},\"nodeModulesDir\":\"none\"}\n",
        )
        .expect("fake frozen Deno config");
        fs::write(
            &lock,
            "{\"version\":\"5\",\"specifiers\":{},\"jsr\":{},\"npm\":{},\"remote\":{}}\n",
        )
        .expect("fake frozen Deno lock");
        let deno_dir = staging_root.join("deno-cache");
        fs::create_dir_all(&deno_dir).expect("fake Deno cache");
        // These names are an explicit session-runtime custody contract: a
        // restart can find partial transcript and daemon captures without a
        // caller-selected path convention.
        let stdout_path = staging_root.join("stdout.log");
        let stderr_path = staging_root.join("stderr.log");
        let supervision = ProcessSupervisionSpec::new(
            stdout_path.clone(),
            stderr_path,
            64 * 1024,
            64 * 1024,
            Duration::from_secs(10),
            Duration::from_millis(100),
        )
        .expect("supervision");
        let spawn = PiHostSpawnSpec::new_for_assignment(
            deno.clone(),
            host,
            config,
            lock,
            workspace_root.clone(),
            0,
            deno_dir,
            vec![(
                OsString::from("FACTORY_FAKE_PROVIDER_KEY"),
                OsString::from("provider-free-test-value"),
            )],
        )
        .expect("Deno spawn spec");
        let request = SessionLaunchRequest {
            principal: "architect".to_owned(),
            command_id: unique("runtime-session"),
            expected_assignment_revision: ExpectedRevision::new(assignment.resulting_revision),
            assignment_id: assignment.assignment_id,
            packet_digest,
            packet: packet.clone(),
            canonical_packet_bytes: packet_bytes.clone(),
            packet_artifact: packet_artifact.sealed,
            spawn,
            supervision,
            workspace_root,
            expected_read_manifest_artifact_id: required_manifest.artifact_id,
            required_reads: packet.required_reads.clone(),
            candidate_quality_runtime: None,
        };
        // AF_UNIX has a small platform path ceiling (104 bytes on macOS).
        // Keep daemon control paths independent of the intentionally verbose
        // CAS fixture root so this judge exercises transport, not path length.
        let daemon_root = std::env::temp_dir().join(format!("fv3d-{}", unique_number()));
        let daemon = LocalDaemon::bind(LocalTransportConfig::new(daemon_root.clone()), &store)
            .await
            .expect("daemon")
            .with_campaign_control(process.clone(), tickets.clone());
        Self {
            store,
            build,
            process,
            forum,
            publications,
            tickets,
            command_runner,
            daemon,
            daemon_root,
            campaign_id: campaign.campaign_id,
            assignment,
            packet,
            packet_bytes,
            packet_artifact: packet_artifact.sealed,
            stdout_path,
            request,
        }
    }

    async fn launch(
        &self,
    ) -> Result<factory_kernel::session_runtime::SessionRuntimeOutcome, SessionRuntimeError> {
        let verifier = ExactFixtureVerifier {
            packet: self.packet.clone(),
            bytes: self.packet_bytes.clone(),
            packet_artifact: self.packet_artifact,
        };
        launch_session(
            &self.process,
            &self.forum,
            &self.publications,
            &self.tickets,
            &self.command_runner,
            &self.daemon,
            &self.build.cas,
            self.request.clone(),
            &verifier,
        )
        .await
    }

    fn start_command(&self) -> factory_kernel::process::StartSession {
        factory_kernel::process::StartSession {
            principal: "architect".to_owned(),
            command_id: unique("unexpected-second-session"),
            expected_assignment_revision: ExpectedRevision::new(self.assignment.resulting_revision),
            assignment_id: self.assignment.assignment_id,
            packet_digest: self.packet.packet_digest,
            custody: factory_protocol::ProcessCustodyV1 {
                pid: std::process::id(),
                pgid: std::process::id(),
                started_at_unix_millis: 1,
            },
        }
    }

    async fn close(self) {
        self.daemon.shutdown().await.expect("daemon shutdown");
        self.store.close().await;
        fs::remove_dir_all(self.daemon_root).expect("remove daemon test root");
    }
}

/// Test double for the installed runtime authority. It deliberately checks
/// every runtime path passed to the launcher, while the live RPC independently
/// reparses the canonical packet and verifies its registered CAS bytes.
struct ExactFixtureVerifier {
    packet: AssignmentPacketV1,
    bytes: Vec<u8>,
    packet_artifact: CasArtifact,
}

impl SessionRuntimeVerifier for ExactFixtureVerifier {
    fn verify_packet(
        &self,
        packet: &AssignmentPacketV1,
        canonical_packet_bytes: &[u8],
    ) -> Result<(), factory_kernel::session_runtime::RuntimeVerificationError> {
        if packet != &self.packet || canonical_packet_bytes != self.bytes {
            return Err(
                factory_kernel::session_runtime::RuntimeVerificationError::PacketSealMismatch,
            );
        }
        factory_protocol::verify_assignment_packet_v1(
            canonical_packet_bytes,
            &packet.packet_digest.to_hex(),
        )
        .map_err(|error| {
            factory_kernel::session_runtime::RuntimeVerificationError::PacketContract(
                error.to_string(),
            )
        })?;
        Ok(())
    }

    fn verify_runtime(
        &self,
        packet: &AssignmentPacketV1,
        spawn: &PiHostSpawnSpec,
    ) -> Result<(), factory_kernel::session_runtime::RuntimeVerificationError> {
        if packet.runtime != self.packet.runtime
            || spawn.executable() != Path::new(packet.runtime.deno_executable.as_str())
            || spawn.deno_dir().is_none()
            || spawn
                .host_entrypoint()
                .file_name()
                .and_then(|name| name.to_str())
                != Some("provider-free-fake-host.ts")
            || spawn
                .deno_config()
                .file_name()
                .and_then(|name| name.to_str())
                != Some("deno.json")
            || spawn.deno_lock().file_name().and_then(|name| name.to_str()) != Some("deno.lock")
            || self.packet_artifact.byte_length() == 0
        {
            return Err(
                factory_kernel::session_runtime::RuntimeVerificationError::RuntimeIdentity(
                    "fixture Deno runtime differs from its admitted assignment".to_owned(),
                ),
            );
        }
        Ok(())
    }
}

struct InstalledBuild {
    cas: CasStore,
    receipt: factory_kernel::storage::KernelBuildReceipt,
    kernel_build_id: factory_protocol::KernelBuildId,
}

async fn store() -> KernelStore {
    KernelStore::connect(&test_database_url())
        .await
        .expect("connect test database")
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

async fn install_build(store: &KernelStore) -> InstalledBuild {
    let cas = CasStore::new_with_seed(
        std::env::temp_dir().join(unique("session-runtime-cas")),
        4 * 1024 * 1024,
        unique_number(),
    )
    .expect("CAS");
    let staging = cas.runtime_root().join("build-staging");
    fs::create_dir_all(&staging).expect("build staging");
    fs::write(
        staging.join("qualification"),
        b"provider-free qualification",
    )
    .expect("qualification receipt");
    let qualification = cas
        .adopt(&staging, Path::new("qualification"))
        .expect("seal qualification receipt");
    let status = store.kernel_build_status().await.expect("build status");
    let build_id = factory_protocol::KernelBuildId::new(digest(unique_number()));
    let receipt = store
        .install_kernel_build(
            &cas,
            &InstallKernelBuild {
                principal: "operator".to_owned(),
                command_id: unique("runtime-build"),
                expected_revision: ExpectedRevision::new(status.aggregate_revision),
                build_id,
                source_digest: digest(unique_number()),
                binary_digest: digest(unique_number()),
                schema_identity: SCHEMA_IDENTITY.to_owned(),
                deno_executable_path: deno_executable().to_string_lossy().into_owned(),
                deno_version: "2.9.4".to_owned(),
                deno_lock_digest: digest(unique_number()),
                qualification_receipt: qualification,
            },
        )
        .await
        .expect("install build");
    InstalledBuild {
        cas,
        receipt,
        kernel_build_id: build_id,
    }
}

struct SealedArtifact {
    artifact_id: ArtifactId,
    sealed: CasArtifact,
}

async fn seal_and_register(
    store: &KernelStore,
    build: &InstalledBuild,
    label: &str,
    bytes: &[u8],
) -> SealedArtifact {
    let path = build
        .cas
        .runtime_root()
        .join("build-staging")
        .join(format!("{label}-{}", unique_number()));
    fs::write(&path, bytes).expect("artifact fixture bytes");
    let sealed = build
        .cas
        .adopt(
            path.parent().expect("artifact parent"),
            path.file_name().expect("artifact name"),
        )
        .expect("seal artifact");
    let receipt = store
        .register_artifact(
            &build.cas,
            &RegisterArtifact {
                principal: "operator".to_owned(),
                command_id: unique(label),
                expected_kernel_build_revision: ExpectedRevision::new(
                    build.receipt.resulting_revision,
                ),
                kernel_build_id: build.kernel_build_id,
                sealed,
            },
        )
        .await
        .expect("register artifact");
    SealedArtifact {
        artifact_id: receipt.artifact_id,
        sealed,
    }
}

async fn admit_application(
    store: &KernelStore,
    build: &InstalledBuild,
    repository_key: &str,
    repository_path: &str,
) -> ApplicationRevisionId {
    let root = build.cas.runtime_root().join(unique("runtime-application"));
    fs::create_dir_all(&root).expect("application root");
    let paths = [
        "mission.md",
        "product-system.md",
        "product-assignment.md",
        "engineering-system.md",
        "engineering-assignment.md",
        "quality-system.md",
        "quality-assignment.md",
    ];
    let mut templates = Vec::new();
    for (index, path) in paths.iter().enumerate() {
        let bytes = format!("runtime-template-{index}");
        fs::write(root.join(path), &bytes).expect("template");
        templates.push((*path, ContentDigest::of_bytes(bytes.as_bytes())));
    }
    let application_key = unique("runtime-application");
    let bundle = minimal_bundle_json(
        &application_key,
        repository_key,
        repository_path,
        &templates,
    );
    fs::write(root.join("bundle.json"), bundle).expect("bundle");
    let admitted = store
        .admit_compiled_application(
            &build.cas,
            &AdmitCompiledApplication {
                principal: "architect".to_owned(),
                command_id: unique("runtime-application"),
                expected_revision: ExpectedRevision::new(AggregateRevision::initial()),
                expected_kernel_build_revision: ExpectedRevision::new(
                    build.receipt.resulting_revision,
                ),
                kernel_build_id: build.kernel_build_id,
                source_root: root,
                bundle_relative_path: "bundle.json".into(),
            },
        )
        .await
        .expect("application admission");
    let rationale = seal_and_register(store, build, "application-activation", b"activate").await;
    store
        .activate_application_revision(&ActivateApplicationRevision {
            principal: ArchitectPrincipalV1::parse("architect").expect("principal"),
            command_id: unique("application-activate"),
            expected_revision: ExpectedRevision::new(admitted.resulting_revision),
            application_key: ApplicationKey::parse(application_key).expect("application key"),
            application_revision_id: admitted.application_revision_id,
            rationale: SealedArtifactReferenceV1 {
                artifact_id: rationale.artifact_id,
                digest: rationale.sealed.digest(),
                byte_length: rationale.sealed.byte_length(),
            },
        })
        .await
        .expect("activate application");
    admitted.application_revision_id
}

fn canonical_manifest(observed: &[ReadObservationV1]) -> Vec<u8> {
    let mut value = b"factory-read-manifest-v1\0".to_vec();
    value.extend_from_slice(&(observed.len() as u32).to_be_bytes());
    for item in observed {
        value.extend_from_slice(&(item.path.as_str().len() as u32).to_be_bytes());
        value.extend_from_slice(item.path.as_str().as_bytes());
        value.extend_from_slice(&item.digest.as_bytes());
        let reason = b"authority contract";
        value.extend_from_slice(&(reason.len() as u32).to_be_bytes());
        value.extend_from_slice(reason);
    }
    value
}

fn model() -> ModelProfileV1 {
    ModelProfileV1 {
        provider: "fake".to_owned(),
        model_id: "provider-free-model".to_owned(),
        thinking_level: factory_protocol::ThinkingLevelV1::None,
        context_token_limit: 10,
        output_token_limit: 10,
        price_input_micro_usd_per_million_tokens: MicroUsd::new(1),
        price_output_micro_usd_per_million_tokens: MicroUsd::new(1),
        price_cache_read_micro_usd_per_million_tokens: MicroUsd::new(1),
        price_cache_write_micro_usd_per_million_tokens: MicroUsd::new(1),
        capability_flags: Vec::new(),
    }
}

fn wire_packet(
    packet: &AssignmentPacketV1,
    system_prompt: &[u8],
    assignment_prompt: &[u8],
) -> factory_protocol::AssignmentPacketWireV1 {
    factory_protocol::AssignmentPacketWireV1 {
        format_version: ASSIGNMENT_PACKET_V1_FORMAT,
        campaign_id: packet.campaign_id.get(),
        assignment_id: packet.assignment_id.get(),
        application_revision_id: packet.application_revision_id.get(),
        kernel_build_id: packet.kernel_build_id.digest().to_hex(),
        assignment_role: match packet.assignment_role {
            AssignmentRole::ProductResearch => "product_research",
            AssignmentRole::Engineering => "engineering",
            AssignmentRole::Quality => "quality",
        }
        .to_owned(),
        target: packet.target.clone(),
        repository_base_identity: digest(8_100).to_hex(),
        factory_base_identity: digest(8_101).to_hex(),
        ticket_attempt_id: packet
            .ticket_attempt_id
            .map(factory_protocol::TicketAttemptId::get),
        candidate_id: packet.candidate_id.map(factory_protocol::CandidateId::get),
        system_prompt_artifact_id: packet.system_prompt_artifact_id.get(),
        assignment_prompt_artifact_id: packet.assignment_prompt_artifact_id.get(),
        required_read_manifest_artifact_id: packet.required_read_manifest_artifact_id.get(),
        system_prompt_digest: ContentDigest::of_bytes(system_prompt).to_hex(),
        assignment_prompt_digest: ContentDigest::of_bytes(assignment_prompt).to_hex(),
        system_prompt_bytes_b64: base64(system_prompt),
        assignment_prompt_bytes_b64: base64(assignment_prompt),
        workspace_root: packet.workspace_root.as_str().to_owned(),
        staging_root: packet.staging_root.as_str().to_owned(),
        model: factory_protocol::AssignmentModelWireV1 {
            provider: packet.model.provider.clone(),
            model_id: packet.model.model_id.clone(),
            thinking_level: "none".to_owned(),
            context_token_limit: packet.model.context_token_limit,
            output_token_limit: packet.model.output_token_limit,
            price_input_micro_usd_per_million_tokens: packet
                .model
                .price_input_micro_usd_per_million_tokens
                .get(),
            price_output_micro_usd_per_million_tokens: packet
                .model
                .price_output_micro_usd_per_million_tokens
                .get(),
            price_cache_read_micro_usd_per_million_tokens: packet
                .model
                .price_cache_read_micro_usd_per_million_tokens
                .get(),
            price_cache_write_micro_usd_per_million_tokens: packet
                .model
                .price_cache_write_micro_usd_per_million_tokens
                .get(),
            capability_flags: Vec::new(),
        },
        limits: factory_protocol::AssignmentLimitsWireV1 {
            turn_limit: packet.limits.turn_limit,
            wall_limit_millis: packet.limits.wall_limit.get(),
            output_byte_limit: packet.limits.output_byte_limit,
        },
        runtime: factory_protocol::AssignmentRuntimeWireV1 {
            deno_executable: packet.runtime.deno_executable.as_str().to_owned(),
            deno_version: packet.runtime.deno_version.clone(),
            source_graph_digest: packet.runtime.source_graph_digest.to_hex(),
            resolved_dependency_graph_digest: packet
                .runtime
                .resolved_dependency_graph_digest
                .to_hex(),
            deno_json_digest: packet.runtime.deno_json_digest.to_hex(),
            deno_lock_digest: packet.runtime.deno_lock_digest.to_hex(),
            pi_version: packet.runtime.pi_version.clone(),
            credential_source: factory_protocol::AssignmentCredentialWireV1 {
                kind: "environment".to_owned(),
                name: Some("FACTORY_FAKE_PROVIDER_KEY".to_owned()),
                path: None,
            },
        },
        required_reads: packet
            .required_reads
            .iter()
            .map(|read| factory_protocol::AssignmentReadWireV1 {
                path: read.path.as_str().to_owned(),
                digest: read.digest.to_hex(),
                reason: read.reason.clone(),
            })
            .collect(),
        assignment_evidence: packet
            .assignment_evidence
            .iter()
            .map(|evidence| factory_protocol::AssignmentEvidenceWireV1 {
                role: evidence.role.wire_name().to_owned(),
                artifact_id: evidence.artifact_id.get(),
                digest: evidence.digest.to_hex(),
                byte_length: evidence.byte_length,
            })
            .collect(),
        tools: vec![
            "workspace_read".to_owned(),
            "forum_list_topics".to_owned(),
            "artifact_seal".to_owned(),
            "product_submit_ticket".to_owned(),
            "work_complete".to_owned(),
        ],
        terminal_operations: vec!["work_complete".to_owned()],
        remaining_campaign_allowance_micro_usd: packet.remaining_campaign_allowance.get(),
        aggregate_revision: packet.revision.get(),
        packet_digest: String::new(),
    }
}

fn deno_executable() -> PathBuf {
    for candidate in ["/opt/homebrew/bin/deno", "/usr/local/bin/deno"] {
        let path = PathBuf::from(candidate);
        if path.is_file() {
            return path;
        }
    }
    panic!("a real Deno executable is required for the session runtime integration judge");
}

fn base64(bytes: &[u8]) -> String {
    const ALPHABET: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut output = String::with_capacity(bytes.len().div_ceil(3) * 4);
    for chunk in bytes.chunks(3) {
        let first = chunk[0];
        let second = chunk.get(1).copied().unwrap_or(0);
        let third = chunk.get(2).copied().unwrap_or(0);
        output.push(ALPHABET[(first >> 2) as usize] as char);
        output.push(ALPHABET[(((first & 0x03) << 4) | (second >> 4)) as usize] as char);
        output.push(if chunk.len() > 1 {
            ALPHABET[(((second & 0x0f) << 2) | (third >> 6)) as usize] as char
        } else {
            '='
        });
        output.push(if chunk.len() > 2 {
            ALPHABET[(third & 0x3f) as usize] as char
        } else {
            '='
        });
    }
    output
}

fn unique(prefix: &str) -> String {
    format!("{prefix}-{}", unique_number())
}

fn unique_number() -> u64 {
    (u64::from(std::process::id()) << 32) | NEXT_TEST.fetch_add(1, Ordering::Relaxed)
}

fn digest(value: u64) -> ContentDigest {
    let mut bytes = [0_u8; 32];
    for chunk in bytes.as_chunks_mut::<8>().0 {
        chunk.copy_from_slice(&value.to_be_bytes());
    }
    ContentDigest::from_bytes(bytes)
}

fn minimal_bundle_json(
    application: &str,
    repository: &str,
    path: &str,
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
        |name: &str, system: usize, assignment: usize| AssignmentRoleWireV1 {
            assignment_role: name.to_owned(),
            system_template: template(system),
            assignment_template: template(assignment),
            tools: if name == "product_research" {
                vec![
                    "workspace_read".to_owned(),
                    "product_submit_ticket".to_owned(),
                ]
            } else {
                vec!["workspace_read".to_owned()]
            },
            model: ModelWireV1 {
                provider: "test".to_owned(),
                model_id: "test".to_owned(),
                thinking_level: "none".to_owned(),
                context_token_limit: 1,
                output_token_limit: 1,
                price_input_micro_usd_per_million_tokens: 1,
                price_output_micro_usd_per_million_tokens: 1,
                price_cache_read_micro_usd_per_million_tokens: 1,
                price_cache_write_micro_usd_per_million_tokens: 1,
                capability_flags: Vec::new(),
            },
            limits: LimitsWireV1 {
                turn_limit: 1,
                wall_limit_millis: 10_000,
                output_byte_limit: 4096,
            },
        };
    canonical_application_bundle_json_v1(&ApplicationBundleWireV1 {
        format_version: 1,
        application_key: application.to_owned(),
        predecessor_bundle: None,
        repository: RepositoryWireV1 {
            repository_key: repository.to_owned(),
            canonical_local_path: path.to_owned(),
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
            maximum: 1,
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
    .expect("canonical bundle")
}

const FAKE_HOST_SOURCE: &str = r#"
const io = await Deno.open("/dev/fd/0", { read: true, write: true });
const encoder = new TextEncoder();
const decoder = new TextDecoder();
let request = 0;

async function readExact(length: number): Promise<Uint8Array> {
  const output = new Uint8Array(length);
  let offset = 0;
  while (offset < length) {
    const read = await io.read(output.subarray(offset));
    if (read === null) throw new Error("daemon descriptor closed");
    offset += read;
  }
  return output;
}

async function writeAll(value: Uint8Array): Promise<void> {
  let offset = 0;
  while (offset < value.length) offset += await io.write(value.subarray(offset));
}

async function readAdmissionLine(): Promise<string> {
  const bytes: number[] = [];
  while (true) {
    const one = await readExact(1);
    if (one[0] === 10) return decoder.decode(new Uint8Array(bytes));
    bytes.push(one[0]);
    if (bytes.length > 1024 * 1024) throw new Error("oversized admission line");
  }
}

async function readFrame(): Promise<Record<string, unknown>> {
  const prefix = await readExact(4);
  const length = (prefix[0] << 24) | (prefix[1] << 16) | (prefix[2] << 8) | prefix[3];
  if (length < 1 || length > 4 * 1024 * 1024) throw new Error("invalid response frame");
  return JSON.parse(decoder.decode(await readExact(length))) as Record<string, unknown>;
}

async function call(operation: string, fields: Record<string, unknown>): Promise<Record<string, unknown>> {
  const request_id = `fake-request-${++request}`;
  const bytes = encoder.encode(JSON.stringify({
    protocol_version: 1,
    request_id,
    operation,
    ...fields,
  }));
  const prefix = new Uint8Array(4);
  new DataView(prefix.buffer).setUint32(0, bytes.length, false);
  await writeAll(prefix);
  await writeAll(bytes);
  const response = await readFrame();
  if (response.protocol_version !== 1 || response.request_id !== request_id || response.operation !== operation) {
    throw new Error(`bad ${operation} response identity`);
  }
  if (typeof response.error_code === "string") throw new Error(`${response.error_code}: ${String(response.message)}`);
  return response;
}

function bytesFromBase64(value: string): Uint8Array {
  const decoded = atob(value);
  const bytes = new Uint8Array(decoded.length);
  for (let index = 0; index < decoded.length; index++) bytes[index] = decoded.charCodeAt(index);
  return bytes;
}

function base64(value: string): string {
  return btoa(value);
}

const admission = JSON.parse(await readAdmissionLine()) as Record<string, unknown>;
if (admission.type !== "session.admitted" || typeof admission.packet_b64 !== "string" ||
    typeof admission.packet_digest !== "string" || typeof admission.session_revision !== "number") {
  throw new Error("invalid daemon admission");
}
const packetBytes = bytesFromBase64(admission.packet_b64);
const packet = JSON.parse(decoder.decode(packetBytes)) as Record<string, any>;
if (packet.packet_digest !== admission.packet_digest || packet.required_reads.length !== 1) {
  throw new Error("packet/admission mismatch");
}
const verified = await call("session.verify_packet", {
  packet_digest: admission.packet_digest,
  packet_bytes_b64: admission.packet_b64,
});
if (verified.verified !== true || verified.packet_digest !== admission.packet_digest) {
  throw new Error("packet verification was not accepted");
}
if (FAKE_TERMINAL_MODE === "wait_for_cancellation") {
  await new Promise(() => {});
}
if (FAKE_TERMINAL_MODE === "product_mutation_before_read") {
  let rejected = false;
  try {
    await call("product.submit_ticket", {});
  } catch (error) {
    rejected = String(error).includes("all assigned exact reads are required before mutation");
  }
  if (!rejected) throw new Error("Product mutation was not rejected before the daemon read ledger was complete");
  Deno.exit(0);
}
const required = packet.required_reads[0];
const read = await call("workspace.read", { repository_relative_path: required.path });
if (read.blake3 !== required.digest || read.canonical_path !== required.path) {
  throw new Error("wrapped required read did not return the pinned bytes");
}
if (FAKE_TERMINAL_MODE === "oversized_unsealed_transcript") {
  await Deno.writeFile(
    `${packet.staging_root}/session.ndjson`,
    new Uint8Array(4 * 1024 * 1024 + 1).fill(65),
  );
  Deno.exit(0);
}
const completed = FAKE_TERMINAL_MODE === "completed" ||
  FAKE_TERMINAL_MODE === "completed_thousand_events";
if (completed) {
  const evidenceName = "proposal-evidence.txt";
  await Deno.writeTextFile(`${packet.workspace_root}/${evidenceName}`, "sealed proposal evidence\n");
  const evidence = await call("artifact.seal_workspace_file", {
    client_command_id: "fake-seal-proposal-evidence",
    expected_revision: admission.session_revision,
    workspace_relative_path: evidenceName,
    byte_limit: 1024,
  });
  if (!Number.isSafeInteger(evidence.artifact_id) || evidence.byte_length !== 25) {
    throw new Error("staging artifact was not durably adopted");
  }
}
if (FAKE_TERMINAL_MODE === "completed") {
  let rejected = false;
  try {
    await call("forum.create_topic", {
      client_command_id: "fake-forum-topic",
      expected_revision: 0,
      name: "Provider-free actor topic",
      description: "This unanchored Forum write must be retired.",
    });
  } catch (_) {
    rejected = true;
  }
  if (!rejected) throw new Error("retired unanchored Forum mutation was accepted");
  const topics = await call("forum.list_topics", { cursor: "", limit: 20 });
  if (!Array.isArray(topics.items) || topics.items.some((item) => item.name === "Provider-free actor topic")) {
    throw new Error("retired Forum mutation changed legacy Forum state");
  }
}
const transcriptEventCount = FAKE_TERMINAL_MODE === "completed_thousand_events" ? 1000 : 1;
const event = encoder.encode(Array.from(
  { length: transcriptEventCount },
  (_, sequence) => JSON.stringify({ type: "fake_provider_event", provider: "none", sequence }) + "\n",
).join(""));
const gzip = new Blob([event]).stream().pipeThrough(new CompressionStream("gzip"));
const gzipBytes = new Uint8Array(await new Response(gzip).arrayBuffer());
const transcriptName = "session.ndjson.gz";
await Deno.writeFile(`${packet.staging_root}/${transcriptName}`, gzipBytes);
const sealed = await call("session.seal_artifact", {
  client_command_id: "fake-seal-transcript",
  expected_revision: admission.session_revision,
  staging_relative_path: transcriptName,
  role: "pi_transcript_gzip",
  byte_limit: 1048576,
});
if (!Number.isSafeInteger(sealed.artifact_id) || sealed.artifact_id < 1) throw new Error("missing transcript receipt");
console.log(`FAKE_TRANSCRIPT_ARTIFACT_ID=${sealed.artifact_id}`);
console.log(`FAKE_TRANSCRIPT_EVENT_COUNT=${transcriptEventCount}`);
const unknown = FAKE_TERMINAL_MODE === "unknown_cost";
await call("session.submit_terminal", {
  client_command_id: "fake-terminal",
  expected_revision: admission.session_revision,
  terminal_operation: unknown ? null : "work_complete",
  terminal_payload_b64: base64(unknown ? "{\"outcome\":\"unknown\"}" : "{\"outcome\":\"complete\"}"),
  transcript_artifact_id: sealed.artifact_id,
  input_tokens: 2,
  output_tokens: 3,
  cache_read_tokens: 0,
  cache_write_tokens: 0,
  reasoning_tokens: null,
  reported_cost_micro_usd: unknown ? null : 7,
  stop_reason: unknown ? "unknown_cost" : "completed",
});
"#;
