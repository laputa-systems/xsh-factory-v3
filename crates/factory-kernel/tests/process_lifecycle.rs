//! Provider-free PostgreSQL judges for the Tranche 5 process authority.
//!
//! These tests deliberately use the daemon-created actor binding and its
//! workspace-read authority.  A test must not manufacture a
//! `ReadObservationV2` and pass it directly to terminal admission.

use std::sync::atomic::{AtomicU64, Ordering};
use std::{fs, time::Duration};

use factory_kernel::cas::{CasArtifact, CasStore};
use factory_kernel::local_transport::{LocalDaemon, LocalTransportConfig};
use factory_kernel::process::{
    CancelCampaign, CreateAssignment, FailCampaign, StartCampaign, StartSession,
};
use factory_kernel::restart_recovery::{RestartRecoveryPolicy, reconcile_daemon_restart};
use factory_kernel::storage::{
    ActivateApplicationRevision, AdmitCompiledApplication, InstallKernelBuild, KernelStore,
    RegisterArtifact, RegisterRepository, SCHEMA_IDENTITY,
};
use factory_protocol::{
    ASSIGNMENT_PACKET_V2_FORMAT, AbsoluteHostPath, AggregateRevision, ApplicationKey,
    ApplicationRevisionId, ArchitectPrincipalV2, ArtifactId, AssignmentPacketV2, AssignmentRole,
    ContentDigest, ExpectedRevision, MicroUsd, ModelProfileV2, PolicyEntrypointV2, ReadExactFileV2,
    RepositoryRelativePath, RuntimeIdentityV2, SealedArtifactReferenceV2, SessionLimitsV2,
    StopReasonV2, TerminalOperationV2, TerminalReportV2, UsageTotalsV2,
};

static NEXT_TEST: AtomicU64 = AtomicU64::new(1);
const POLICY_BYTES: &[u8] = b"return { factory_policy = function() end }\n";

#[test]
#[ignore = "requires FACTORY_TEST_DATABASE_URL for a disposable PostgreSQL 18 database"]
fn failed_product_campaign_persists_one_bounded_reason_and_retries_idempotently() {
    smol::block_on(async {
        let fixture = Fixture::new().await;
        let process = fixture.store.process_store();
        let campaign = process
            .start_campaign(&StartCampaign {
                principal: "architect".to_owned(),
                command_id: unique("product-fault-campaign"),
                expected_application_revision: ExpectedRevision::new(
                    AggregateRevision::from_persisted(1),
                ),
                application_revision_id: fixture.application,
                aggregate_budget: MicroUsd::new(10),
                deadline_unix_millis: 4_000_000_000_000,
                delivery_target: 1,
            })
            .await
            .expect("running Product campaign");
        let command = FailCampaign {
            principal: "factoryd-campaign-driver".to_owned(),
            command_id: unique("product-materialization-fault"),
            expected_revision: ExpectedRevision::new(campaign.resulting_revision),
            campaign_id: campaign.campaign_id,
            reason: "daemon product assignment fault: packet rejected".to_owned(),
        };
        let failed = process
            .fail_campaign(&command)
            .await
            .expect("terminal Product fault");
        let status = process
            .campaign_status(campaign.campaign_id)
            .await
            .expect("failed campaign remains diagnosable");
        assert_eq!(status.state, factory_protocol::CampaignState::Failed);
        assert_eq!(
            status.failure_reason.as_deref(),
            Some("daemon product assignment fault: packet rejected")
        );
        let retry = process
            .fail_campaign(&command)
            .await
            .expect("idempotent fault retry");
        assert!(retry.was_idempotent_retry);
        assert_eq!(retry.resulting_revision, failed.resulting_revision);
        assert_eq!(
            process
                .campaign_status(campaign.campaign_id)
                .await
                .expect("failure reason after retry")
                .failure_reason,
            status.failure_reason
        );

        // A distinct operator cancellation is terminal but not a daemon
        // fault, so the structural lifecycle invariant leaves it null.
        let cancelled_campaign = process
            .start_campaign(&StartCampaign {
                principal: "architect".to_owned(),
                command_id: unique("cancelled-campaign"),
                expected_application_revision: ExpectedRevision::new(
                    AggregateRevision::from_persisted(1),
                ),
                application_revision_id: fixture.application,
                aggregate_budget: MicroUsd::new(10),
                deadline_unix_millis: 4_000_000_000_000,
                delivery_target: 1,
            })
            .await
            .expect("second running campaign");
        process
            .cancel_campaign(&CancelCampaign {
                principal: "architect".to_owned(),
                command_id: unique("cancelled-campaign-transition"),
                expected_revision: ExpectedRevision::new(cancelled_campaign.resulting_revision),
                campaign_id: cancelled_campaign.campaign_id,
            })
            .await
            .expect("cancel without failure reason");
        assert_eq!(
            process
                .campaign_status(cancelled_campaign.campaign_id)
                .await
                .expect("cancelled campaign status")
                .failure_reason,
            None
        );
        fixture.daemon.shutdown().await.expect("daemon shutdown");
        fixture.store.close().await;
    });
}

#[test]
#[ignore = "requires FACTORY_TEST_DATABASE_URL for a disposable PostgreSQL 18 database"]
fn tranche5_lifecycle_judges() {
    smol::block_on(async {
        let fixture = Fixture::new().await;
        let process = fixture.store.process_store();

        // A rejected start is a complete rollback: no campaign audit receipt
        // may appear for a deadline that was already elapsed.
        let before = process.process_fact_counts().await.expect("fact counts");
        let rejected = process
            .start_campaign(&StartCampaign {
                principal: "architect".to_owned(),
                command_id: unique("expired-campaign"),
                expected_application_revision: ExpectedRevision::new(
                    AggregateRevision::from_persisted(1),
                ),
                application_revision_id: fixture.application,
                aggregate_budget: MicroUsd::new(100),
                deadline_unix_millis: 1,
                delivery_target: 1,
            })
            .await;
        assert!(matches!(
            rejected,
            Err(factory_kernel::storage::StoreError::CampaignDeadlineElapsed)
        ));
        assert_eq!(
            process.process_fact_counts().await.expect("fact counts"),
            before
        );

        let mut campaign = process
            .start_campaign(&StartCampaign {
                principal: "architect".to_owned(),
                command_id: unique("campaign"),
                expected_application_revision: ExpectedRevision::new(
                    AggregateRevision::from_persisted(1),
                ),
                application_revision_id: fixture.application,
                aggregate_budget: MicroUsd::new(10),
                deadline_unix_millis: 4_000_000_000_000,
                delivery_target: 1,
            })
            .await
            .expect("campaign");

        let first = fixture
            .assignment(&campaign, 1, MicroUsd::new(10), unique("first"))
            .await;

        // Prepare an unconsumed identity whose final canonical packet exists
        // in CAS but has no artifact fact. `packet_only` registers a distinct
        // precursor packet; changing the target and resealing ensures this
        // judge cannot accidentally reuse that content-addressed row.
        let rollback = fixture
            .packet_only(&campaign, 99, MicroUsd::new(10), unique("rollback"))
            .await;
        let mut rollback_packet = rollback.packet;
        rollback_packet.target.push_str("-unregistered");
        let system_bytes = fixture
            .build
            .cas
            .read(fixture.system_prompt.sealed.digest())
            .unwrap();
        let assignment_prompt_bytes = fixture
            .build
            .cas
            .read(fixture.assignment_prompt.sealed.digest())
            .unwrap();
        let mut rollback_wire =
            wire_packet(&rollback_packet, &system_bytes, &assignment_prompt_bytes);
        let rollback_digest =
            factory_protocol::unsigned_assignment_packet_digest_v2(&rollback_wire).unwrap();
        rollback_wire.packet_digest = rollback_digest.to_hex();
        rollback_packet.packet_digest = rollback_digest;
        let rollback_packet_bytes =
            factory_protocol::canonical_assignment_packet_json_v2(&rollback_wire)
                .unwrap()
                .into_bytes();
        let unregistered_packet_artifact =
            unregistered_artifact(&fixture.build, &rollback_packet_bytes);

        // Stale admission and a failed artifact lookup leave neither the
        // assignment row nor the campaign revision/audit receipt behind.
        let before_rejected_assignment = process.process_fact_counts().await.expect("counts");
        let stale = process
            .create_assignment(
                &fixture.build.cas,
                &CreateAssignment {
                    principal: "architect".to_owned(),
                    command_id: unique("stale-assignment"),
                    expected_campaign_revision: ExpectedRevision::new(AggregateRevision::initial()),
                    packet: first.packet.clone(),
                    identity: first.command.identity,
                    packet_bytes: first.command.packet_bytes.clone(),
                    packet_artifact: first.command.packet_artifact,
                    required_read_manifest_artifact_id: fixture.expected_manifest.artifact_id,
                    attempt_ordinal: 1,
                    harness: None,
                },
            )
            .await;
        assert!(matches!(
            stale,
            Err(factory_kernel::storage::StoreError::RevisionConflict { .. })
        ));

        let missing_artifact = process
            .create_assignment(
                &fixture.build.cas,
                &CreateAssignment {
                    principal: "architect".to_owned(),
                    command_id: unique("rollback-assignment"),
                    expected_campaign_revision: ExpectedRevision::new(
                        first.assignment.resulting_campaign_revision,
                    ),
                    packet: rollback_packet,
                    identity: rollback.identity,
                    packet_bytes: rollback_packet_bytes,
                    packet_artifact: unregistered_packet_artifact,
                    required_read_manifest_artifact_id: fixture.expected_manifest.artifact_id,
                    attempt_ordinal: 99,
                    harness: None,
                },
            )
            .await;
        assert!(
            matches!(
                missing_artifact,
                Err(factory_kernel::storage::StoreError::UnregisteredTerminalArtifact)
            ),
            "unexpected missing-artifact result: {missing_artifact:?}"
        );
        assert_eq!(
            process.process_fact_counts().await.expect("counts"),
            before_rejected_assignment
        );

        // Exact command retry returns the original receipt; the same command
        // ID with changed immutable content is an explicit conflict.
        let first_retry = process
            .create_assignment(&fixture.build.cas, &first.command)
            .await
            .expect("assignment retry");
        assert!(first_retry.was_idempotent_retry);
        assert_eq!(first_retry.assignment_id, first.assignment.assignment_id);
        let mut changed_packet = first.packet.clone();
        changed_packet.target.push_str("-changed");
        let system_bytes = fixture
            .build
            .cas
            .read(fixture.system_prompt.sealed.digest())
            .unwrap();
        let assignment_prompt_bytes = fixture
            .build
            .cas
            .read(fixture.assignment_prompt.sealed.digest())
            .unwrap();
        let mut changed_wire =
            wire_packet(&changed_packet, &system_bytes, &assignment_prompt_bytes);
        let changed_digest =
            factory_protocol::unsigned_assignment_packet_digest_v2(&changed_wire).unwrap();
        changed_wire.packet_digest = changed_digest.to_hex();
        changed_packet.packet_digest = changed_digest;
        let changed_packet_bytes =
            factory_protocol::canonical_assignment_packet_json_v2(&changed_wire)
                .unwrap()
                .into_bytes();
        let changed_packet_artifact = seal_and_register(
            &fixture.store,
            &fixture.build,
            "changed-packet",
            &changed_packet_bytes,
        )
        .await;
        let changed = process
            .create_assignment(
                &fixture.build.cas,
                &CreateAssignment {
                    principal: first.command.principal.clone(),
                    command_id: first.command.command_id.clone(),
                    expected_campaign_revision: first.command.expected_campaign_revision,
                    packet: changed_packet,
                    identity: first.command.identity,
                    packet_bytes: changed_packet_bytes,
                    packet_artifact: changed_packet_artifact.sealed,
                    required_read_manifest_artifact_id: first
                        .command
                        .required_read_manifest_artifact_id,
                    attempt_ordinal: first.command.attempt_ordinal,
                    harness: None,
                },
            )
            .await;
        assert!(matches!(
            changed,
            Err(factory_kernel::storage::StoreError::IdempotencyConflict { .. })
        ));

        campaign.resulting_revision = first.assignment.resulting_campaign_revision;

        // Prepare a second assignment before starting the first session so
        // the database partial unique index is exercised as the global paid
        // WIP gate, not merely a scheduler convention.
        let second = fixture
            .assignment(&campaign, 2, MicroUsd::new(10), unique("second"))
            .await;
        let first_session = process
            .start_session(&first.start_command())
            .await
            .expect("first session");
        let second_while_running = process.start_session(&second.start_command()).await;
        assert!(matches!(
            second_while_running,
            Err(factory_kernel::storage::StoreError::PaidSessionAlreadyRunning)
        ));
        let first_evidence = fixture
            .evidence(&process, &first, first_session.session_id, 1)
            .await;
        let before_invalid_terminal = process.process_fact_counts().await.expect("counts");
        let no_operation = process
            .terminal_session(
                "architect",
                &unique("completed-without-operation"),
                first_session.session_id,
                &TerminalReportV2 {
                    packet_digest: first.packet.packet_digest,
                    expected_session_revision: ExpectedRevision::new(
                        first_session.resulting_revision,
                    ),
                    operation: None,
                    stop_reason: StopReasonV2::Completed,
                    report_digest: digest(2_001),
                },
                first_evidence.clone(),
            )
            .await;
        assert!(matches!(
            no_operation,
            Err(factory_kernel::storage::StoreError::TerminalOperationNotAllowed)
        ));
        let illegal_operation = process
            .terminal_session(
                "architect",
                &unique("illegal-operation"),
                first_session.session_id,
                &TerminalReportV2 {
                    packet_digest: first.packet.packet_digest,
                    expected_session_revision: ExpectedRevision::new(
                        first_session.resulting_revision,
                    ),
                    operation: Some(TerminalOperationV2::CandidateSubmit),
                    stop_reason: StopReasonV2::Completed,
                    report_digest: digest(2_002),
                },
                first_evidence.clone(),
            )
            .await;
        assert!(matches!(
            illegal_operation,
            Err(factory_kernel::storage::StoreError::TerminalOperationNotAllowed)
        ));
        assert_eq!(
            process.process_fact_counts().await.expect("counts"),
            before_invalid_terminal
        );

        let first_report = TerminalReportV2 {
            packet_digest: first.packet.packet_digest,
            expected_session_revision: ExpectedRevision::new(first_session.resulting_revision),
            operation: Some(TerminalOperationV2::WorkComplete),
            stop_reason: StopReasonV2::Completed,
            report_digest: digest(2_003),
        };
        let first_terminal = process
            .terminal_session(
                "architect",
                "terminal-first",
                first_session.session_id,
                &first_report,
                first_evidence.clone(),
            )
            .await
            .expect("completed terminal");
        assert_eq!(
            first_terminal.cost,
            factory_protocol::TerminalCostV2::Known(MicroUsd::new(7))
        );
        campaign.resulting_revision = first_terminal.campaign_revision;

        // A byte-for-byte terminal retry is idempotent. Reusing its command ID
        // for a changed report is not.
        let retry = process
            .terminal_session(
                "architect",
                "terminal-first",
                first_session.session_id,
                &first_report,
                first_evidence.clone(),
            )
            .await
            .expect("terminal retry");
        assert!(retry.was_idempotent_retry);
        let mut changed_report = first_report.clone();
        changed_report.report_digest = digest(2_004);
        let changed_terminal = process
            .terminal_session(
                "architect",
                "terminal-first",
                first_session.session_id,
                &changed_report,
                first_evidence.clone(),
            )
            .await;
        assert!(matches!(
            changed_terminal,
            Err(factory_kernel::storage::StoreError::IdempotencyConflict { .. })
        ));
        let stale_terminal = process
            .terminal_session(
                "architect",
                &unique("stale-terminal"),
                first_session.session_id,
                &first_report,
                first_evidence.clone(),
            )
            .await;
        assert!(matches!(
            stale_terminal,
            Err(factory_kernel::storage::StoreError::RevisionConflict { .. })
        ));

        // The infrastructure terminal may omit an actor operation but still
        // records a known provider cost and leaves the campaign admissible.
        let second_session = process
            .start_session(&second.start_command())
            .await
            .expect("second session after first terminal");
        let second_evidence = fixture
            .evidence(&process, &second, second_session.session_id, 2)
            .await;
        let second_terminal = process
            .terminal_session(
                "architect",
                &unique("disconnect"),
                second_session.session_id,
                &TerminalReportV2 {
                    packet_digest: second.packet.packet_digest,
                    expected_session_revision: ExpectedRevision::new(
                        second_session.resulting_revision,
                    ),
                    operation: None,
                    stop_reason: StopReasonV2::DaemonDisconnected,
                    report_digest: digest(2_005),
                },
                second_evidence,
            )
            .await
            .expect("infrastructure terminal");
        assert_eq!(
            second_terminal.session_state,
            factory_protocol::SessionState::Interrupted
        );
        assert_eq!(
            second_terminal.cost,
            factory_protocol::TerminalCostV2::Known(MicroUsd::new(2))
        );
        campaign.resulting_revision = second_terminal.campaign_revision;

        // A final known terminal pushes the aggregate over the ten micro-USD
        // cap. The measured total includes the overshooting terminal and the
        // campaign becomes failed rather than silently clipping at the cap.
        let third = fixture
            .assignment(&campaign, 3, MicroUsd::new(1), unique("third"))
            .await;
        let third_session = process
            .start_session(&third.start_command())
            .await
            .expect("third session");
        let third_evidence = fixture
            .evidence(&process, &third, third_session.session_id, 3)
            .await;
        let third_terminal = process
            .terminal_session(
                "architect",
                &unique("overshoot"),
                third_session.session_id,
                &TerminalReportV2 {
                    packet_digest: third.packet.packet_digest,
                    expected_session_revision: ExpectedRevision::new(
                        third_session.resulting_revision,
                    ),
                    operation: Some(TerminalOperationV2::WorkComplete),
                    stop_reason: StopReasonV2::Completed,
                    report_digest: digest(2_006),
                },
                third_evidence,
            )
            .await
            .expect("overshooting terminal");
        assert_eq!(
            third_terminal.cost,
            factory_protocol::TerminalCostV2::Exceeded(MicroUsd::new(7))
        );

        // The failed campaign is no longer admissible, and a new campaign can
        // be opened after the terminal budget decision.
        let closed_packet = fixture
            .packet_only(&campaign, 4, MicroUsd::new(1), unique("closed"))
            .await;
        let after_overshoot = process
            .create_assignment(
                &fixture.build.cas,
                &CreateAssignment {
                    principal: "architect".to_owned(),
                    command_id: unique("closed-campaign"),
                    expected_campaign_revision: ExpectedRevision::new(
                        third_terminal.campaign_revision,
                    ),
                    identity: closed_packet.identity,
                    packet: closed_packet.packet,
                    packet_bytes: closed_packet.packet_bytes,
                    packet_artifact: closed_packet.packet_artifact.sealed,
                    required_read_manifest_artifact_id: fixture.expected_manifest.artifact_id,
                    attempt_ordinal: 4,
                    harness: None,
                },
            )
            .await;
        assert!(matches!(
            after_overshoot,
            Err(factory_kernel::storage::StoreError::CampaignClosed { .. })
        ));

        let mut frozen_campaign = process
            .start_campaign(&StartCampaign {
                principal: "architect".to_owned(),
                command_id: unique("unknown-cost-campaign"),
                expected_application_revision: ExpectedRevision::new(
                    AggregateRevision::from_persisted(1),
                ),
                application_revision_id: fixture.application,
                aggregate_budget: MicroUsd::new(100),
                deadline_unix_millis: 4_000_000_000_000,
                delivery_target: 1,
            })
            .await
            .expect("unknown-cost campaign");
        let frozen_assignment = fixture
            .assignment(
                &frozen_campaign,
                1,
                MicroUsd::new(100),
                unique("unknown-cost"),
            )
            .await;
        let frozen_session = process
            .start_session(&frozen_assignment.start_command())
            .await
            .expect("unknown-cost session");
        let frozen_evidence = fixture
            .evidence(&process, &frozen_assignment, frozen_session.session_id, 4)
            .await;
        let unknown = process
            .terminal_session(
                "architect",
                &unique("unknown-cost"),
                frozen_session.session_id,
                &TerminalReportV2 {
                    packet_digest: frozen_assignment.packet.packet_digest,
                    expected_session_revision: ExpectedRevision::new(
                        frozen_session.resulting_revision,
                    ),
                    operation: None,
                    stop_reason: StopReasonV2::UnknownCost,
                    report_digest: digest(2_007),
                },
                frozen_evidence,
            )
            .await
            .expect("unknown-cost terminal");
        assert_eq!(unknown.cost, factory_protocol::TerminalCostV2::Unknown);
        frozen_campaign.resulting_revision = unknown.campaign_revision;
        let campaign_status = process
            .campaign_status(frozen_campaign.campaign_id)
            .await
            .expect("unknown-cost campaign status");
        assert_eq!(
            campaign_status.state,
            factory_protocol::CampaignState::Failed
        );
        assert_eq!(
            campaign_status.measured_cost,
            factory_protocol::TerminalCostV2::Unknown
        );
        let frozen_next = fixture
            .packet_only(
                &frozen_campaign,
                2,
                MicroUsd::new(100),
                unique("frozen-next"),
            )
            .await;
        let frozen_admission = process
            .create_assignment(
                &fixture.build.cas,
                &CreateAssignment {
                    principal: "architect".to_owned(),
                    command_id: unique("frozen-next-assignment"),
                    expected_campaign_revision: ExpectedRevision::new(
                        frozen_campaign.resulting_revision,
                    ),
                    identity: frozen_next.identity,
                    packet: frozen_next.packet,
                    packet_bytes: frozen_next.packet_bytes,
                    packet_artifact: frozen_next.packet_artifact.sealed,
                    required_read_manifest_artifact_id: fixture.expected_manifest.artifact_id,
                    attempt_ordinal: 2,
                    harness: None,
                },
            )
            .await;
        assert!(matches!(
            frozen_admission,
            Err(factory_kernel::storage::StoreError::CampaignClosed { .. })
        ));

        // Read-only status polling does not produce audit or domain rows.
        let before_status = process.process_fact_counts().await.expect("counts");
        for _ in 0..1_000 {
            assert!(
                process
                    .campaign_status(frozen_campaign.campaign_id)
                    .await
                    .is_ok()
            );
        }
        assert_eq!(
            process.process_fact_counts().await.expect("counts"),
            before_status
        );

        fixture.daemon.shutdown().await.expect("daemon shutdown");
        fixture.store.close().await;
    });
}

#[test]
#[ignore = "requires FACTORY_TEST_DATABASE_URL for a disposable PostgreSQL 18 database"]
fn daemon_restart_reconciles_exact_group_and_freezes_unknown_cost_without_resume() {
    smol::block_on(async {
        let fixture = Fixture::new().await;
        let process = fixture.store.process_store();
        let campaign = process
            .start_campaign(&StartCampaign {
                principal: "architect".to_owned(),
                command_id: unique("restart-campaign"),
                expected_application_revision: ExpectedRevision::new(
                    AggregateRevision::from_persisted(1),
                ),
                application_revision_id: fixture.application,
                aggregate_budget: MicroUsd::new(100),
                deadline_unix_millis: 4_000_000_000_000,
                delivery_target: 1,
            })
            .await
            .expect("campaign");
        let assignment = fixture
            .assignment(
                &campaign,
                1,
                MicroUsd::new(100),
                unique("restart-assignment"),
            )
            .await;

        // These are the only code-owned stream locations recovery will
        // inspect. The transcript has complete newline-delimited JSON records
        // so it is retained as partial provenance rather than discarded.
        fs::write(fixture.staging_root.join("stdout.log"), b"partial stdout\n").unwrap();
        fs::write(fixture.staging_root.join("stderr.log"), b"partial stderr\n").unwrap();
        fs::write(
            fixture.staging_root.join("session.ndjson"),
            b"{\"sequence\":0,\"event\":{\"kind\":\"fake\"}}\n",
        )
        .unwrap();

        // This database judge exercises durable restart provenance with an
        // exact, already-absent PID/PGID. The lib judge owns the portable
        // live-group TERM/KILL assertion; keeping that OS-facing behavior out
        // of a PostgreSQL transaction test prevents a failure from leaving a
        // disposable database with an unreconciled paid session.
        let absent_pid = i32::MAX as u32;
        let session = process
            .start_session(&StartSession {
                principal: "architect".to_owned(),
                command_id: unique("restart-session"),
                expected_assignment_revision: ExpectedRevision::new(
                    assignment.assignment.resulting_revision,
                ),
                assignment_id: assignment.assignment.assignment_id,
                packet_digest: assignment.packet.packet_digest,
                custody: factory_protocol::ProcessCustodyV2 {
                    pid: absent_pid,
                    pgid: absent_pid,
                    started_at_unix_millis: unique_number(),
                },
            })
            .await
            .expect("durably started session");

        let report = reconcile_daemon_restart(
            &process,
            &fixture.build.cas,
            RestartRecoveryPolicy::new(Duration::from_secs(1), Duration::from_millis(10)).unwrap(),
        )
        .await
        .expect("restart recovery");
        assert_eq!(report.recovered.len(), 1);
        assert_eq!(report.recovered[0].session_id, session.session_id);
        assert_eq!(
            report.recovered[0].process_group_observation,
            factory_kernel::restart_recovery::ProcessGroupObservation::Absent
        );
        assert_eq!(
            report.recovered[0].terminal.session_state,
            factory_protocol::SessionState::Interrupted
        );
        assert_eq!(
            report.recovered[0].terminal.cost,
            factory_protocol::TerminalCostV2::Unknown
        );
        assert!(
            process
                .restart_recovery_sessions(&fixture.build.cas)
                .await
                .expect("no session is resumed")
                .is_empty()
        );
        let status = process
            .campaign_status(campaign.campaign_id)
            .await
            .expect("campaign status");
        assert_eq!(status.state, factory_protocol::CampaignState::Failed);
        assert_eq!(
            status.measured_cost,
            factory_protocol::TerminalCostV2::Unknown
        );
        assert_eq!(
            status.failure_reason.as_deref(),
            Some("terminal session cost is unknown")
        );

        fixture.daemon.shutdown().await.expect("daemon shutdown");
        fixture.store.close().await;
    });
}

#[derive(Clone)]
struct SealedArtifact {
    artifact_id: ArtifactId,
    sealed: CasArtifact,
}

struct InstalledBuild {
    cas: CasStore,
    receipt: factory_kernel::storage::KernelBuildReceipt,
    kernel_build_id: factory_protocol::KernelBuildId,
}

struct Fixture {
    store: KernelStore,
    build: InstalledBuild,
    application: ApplicationRevisionId,
    daemon: LocalDaemon,
    workspace_root: std::path::PathBuf,
    staging_root: std::path::PathBuf,
    read_path: RepositoryRelativePath,
    read_digest: ContentDigest,
    expected_manifest: SealedArtifact,
    system_prompt: SealedArtifact,
    assignment_prompt: SealedArtifact,
}

struct AssignmentFixture {
    packet: AssignmentPacketV2,
    command: CreateAssignment,
    assignment: factory_kernel::process::AssignmentReceipt,
    packet_artifact: SealedArtifact,
}

struct PacketFixture {
    packet: AssignmentPacketV2,
    packet_bytes: Vec<u8>,
    packet_artifact: SealedArtifact,
    identity: factory_kernel::process::AssignmentIdentityCapability,
}

impl AssignmentFixture {
    fn start_command(&self) -> StartSession {
        StartSession {
            principal: "architect".to_owned(),
            command_id: unique("session"),
            expected_assignment_revision: ExpectedRevision::new(self.assignment.resulting_revision),
            assignment_id: self.assignment.assignment_id,
            packet_digest: self.packet.packet_digest,
            custody: factory_protocol::ProcessCustodyV2 {
                pid: std::process::id(),
                pgid: std::process::id(),
                started_at_unix_millis: unique_number(),
            },
        }
    }
}

impl Fixture {
    async fn new() -> Self {
        let store = KernelStore::connect(&test_database_url())
            .await
            .expect("connect");
        store.migrate_and_verify().await.expect("migration");
        let build = install_build(&store).await;
        let repository_key = unique("repository-key");
        let repository_path = format!("/tmp/{}", unique("product"));
        store
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
        let application =
            admit_application(&store, &build, &repository_key, &repository_path).await;

        let workspace_root = build.cas.runtime_root().join(unique("workspace"));
        fs::create_dir_all(&workspace_root).expect("workspace root");
        fs::write(workspace_root.join("AGENTS.md"), b"read bytes").expect("required read");
        let staging_root = build.cas.runtime_root().join(unique("staging"));
        fs::create_dir_all(&staging_root).expect("staging root");
        let read_path = RepositoryRelativePath::parse("AGENTS.md").unwrap();
        let read_digest = ContentDigest::of_bytes(b"read bytes");
        let expected_manifest = seal_and_register(
            &store,
            &build,
            "expected-manifest",
            &canonical_manifest(&[ReadExactFileV2 {
                path: read_path.clone(),
                digest: read_digest,
                reason: "required contract".to_owned(),
            }]),
        )
        .await;
        let system_prompt =
            seal_and_register(&store, &build, "system-prompt", b"system prompt").await;
        let assignment_prompt =
            seal_and_register(&store, &build, "assignment-prompt", b"assignment prompt").await;
        let daemon_root = std::env::temp_dir().join(unique("daemon"));
        let daemon = LocalDaemon::bind(LocalTransportConfig::new(daemon_root), &store)
            .await
            .expect("daemon");
        Self {
            store,
            build,
            application,
            daemon,
            workspace_root,
            staging_root,
            read_path,
            read_digest,
            expected_manifest,
            system_prompt,
            assignment_prompt,
        }
    }

    async fn packet(
        &self,
        campaign: &factory_kernel::process::CampaignReceipt,
        ordinal: u32,
        allowance: MicroUsd,
        label: String,
    ) -> AssignmentFixture {
        let packet_fixture = self.packet_only(campaign, ordinal, allowance, label).await;
        let command = CreateAssignment {
            principal: "architect".to_owned(),
            command_id: unique("assignment"),
            expected_campaign_revision: ExpectedRevision::new(campaign.resulting_revision),
            identity: packet_fixture.identity,
            packet: packet_fixture.packet.clone(),
            packet_bytes: packet_fixture.packet_bytes.clone(),
            packet_artifact: packet_fixture.packet_artifact.sealed,
            required_read_manifest_artifact_id: self.expected_manifest.artifact_id,
            attempt_ordinal: ordinal,
            harness: None,
        };
        let assignment = self
            .store
            .process_store()
            .create_assignment(&self.build.cas, &command)
            .await
            .expect("assignment");
        AssignmentFixture {
            packet: packet_fixture.packet,
            command,
            assignment,
            packet_artifact: packet_fixture.packet_artifact,
        }
    }

    async fn packet_only(
        &self,
        campaign: &factory_kernel::process::CampaignReceipt,
        _ordinal: u32,
        allowance: MicroUsd,
        label: String,
    ) -> PacketFixture {
        let identity = self
            .store
            .process_store()
            .reserve_assignment_identity()
            .await
            .expect("assignment identity");
        let system_bytes = self
            .build
            .cas
            .read(self.system_prompt.sealed.digest())
            .unwrap();
        let assignment_prompt_bytes = self
            .build
            .cas
            .read(self.assignment_prompt.sealed.digest())
            .unwrap();
        let mut packet = AssignmentPacketV2 {
            format_version: ASSIGNMENT_PACKET_V2_FORMAT,
            campaign_id: campaign.campaign_id,
            assignment_id: identity.assignment_id(),
            kernel_build_id: self.build.kernel_build_id,
            application_revision_id: self.application,
            assignment_role: AssignmentRole::ProductResearch,
            target: label,
            ticket_attempt_id: None,
            candidate_id: None,
            assignment_evidence: Vec::new(),
            system_prompt_artifact_id: self.system_prompt.artifact_id,
            assignment_prompt_artifact_id: self.assignment_prompt.artifact_id,
            required_read_manifest_artifact_id: self.expected_manifest.artifact_id,
            policy_digest: ContentDigest::of_bytes(POLICY_BYTES),
            policy_byte_limit: POLICY_BYTES.len() as u32,
            policy_bytes: POLICY_BYTES.to_vec(),
            policy_entrypoint: PolicyEntrypointV2::FactoryPolicy,
            workspace_root: AbsoluteHostPath::parse(self.workspace_root.to_str().unwrap()).unwrap(),
            staging_root: AbsoluteHostPath::parse(self.staging_root.to_str().unwrap()).unwrap(),
            model: ModelProfileV2 {
                provider: "fake".to_owned(),
                model_id: "fake-model".to_owned(),
                thinking_level: factory_protocol::ThinkingLevelV2::None,
                context_token_limit: 10,
                output_token_limit: 10,
                price_input_micro_usd_per_million_tokens: MicroUsd::new(1),
                price_output_micro_usd_per_million_tokens: MicroUsd::new(1),
                price_cache_read_micro_usd_per_million_tokens: MicroUsd::new(1),
                price_cache_write_micro_usd_per_million_tokens: MicroUsd::new(1),
                capability_flags: Vec::new(),
            },
            limits: SessionLimitsV2 {
                turn_limit: 1,
                wall_limit: factory_protocol::DurationMillis::new(10_000),
                output_byte_limit: 10_000,
            },
            runtime: RuntimeIdentityV2 {
                host_executable: AbsoluteHostPath::parse("/opt/factory/factory-pi-host").unwrap(),
                core_head: "0123456789abcdef0123456789abcdef01234567".to_owned(),
                core_source_digest: digest(700),
                rust_toolchain: "nightly-2026-07-24".to_owned(),
                credential_env: "FAKE_PROVIDER_KEY".to_owned(),
            },
            required_reads: vec![ReadExactFileV2 {
                path: self.read_path.clone(),
                digest: self.read_digest,
                reason: "required contract".to_owned(),
            }],
            terminal_operations: vec![TerminalOperationV2::WorkComplete],
            remaining_campaign_allowance: allowance,
            revision: AggregateRevision::initial(),
            packet_digest: digest(703),
        };
        let mut wire = wire_packet(&packet, &system_bytes, &assignment_prompt_bytes);
        let packet_digest =
            factory_protocol::unsigned_assignment_packet_digest_v2(&wire).expect("packet digest");
        wire.packet_digest = packet_digest.to_hex();
        packet.packet_digest = packet_digest;
        let packet_bytes = factory_protocol::canonical_assignment_packet_json_v2(&wire)
            .expect("canonical packet")
            .into_bytes();
        let packet_artifact =
            seal_and_register(&self.store, &self.build, "packet", &packet_bytes).await;
        PacketFixture {
            packet,
            packet_bytes,
            packet_artifact,
            identity,
        }
    }

    async fn assignment(
        &self,
        campaign: &factory_kernel::process::CampaignReceipt,
        ordinal: u32,
        allowance: MicroUsd,
        label: String,
    ) -> AssignmentFixture {
        self.packet(campaign, ordinal, allowance, label).await
    }

    async fn evidence(
        &self,
        process: &factory_kernel::process::ProcessStore,
        assignment: &AssignmentFixture,
        session_id: factory_protocol::SessionId,
        index: u64,
    ) -> factory_kernel::process::VerifiedTerminalEvidence {
        let (_descriptor, connection) = self
            .daemon
            .create_admitted_actor_socketpair(process, session_id, &assignment.packet)
            .await
            .expect("admitted actor socketpair");
        let mut authority = connection
            .workspace_read_authority(
                &self.workspace_root,
                self.expected_manifest.artifact_id,
                assignment.packet.required_reads.clone(),
            )
            .expect("workspace read authority");
        let read = authority
            .read_exact(self.read_path.clone())
            .expect("exact workspace read");
        assert_eq!(read.blake3, self.read_digest.to_hex());
        let assertion_staging = self
            .build
            .cas
            .runtime_root()
            .join(format!("assertion-staging-{index}-{}", unique_number()));
        fs::create_dir_all(&assertion_staging).expect("assertion staging");
        let assertion = authority
            .seal_assertion(&self.build.cas, &assertion_staging)
            .expect("read assertion");
        self.store
            .register_artifact(
                &self.build.cas,
                &RegisterArtifact {
                    principal: "operator".to_owned(),
                    command_id: unique("assertion"),
                    expected_kernel_build_revision: ExpectedRevision::new(
                        self.build.receipt.resulting_revision,
                    ),
                    kernel_build_id: self.build.kernel_build_id,
                    sealed: assertion.artifact(),
                },
            )
            .await
            .expect("register assertion");
        let transcript = seal_and_register(
            &self.store,
            &self.build,
            "transcript",
            format!("transcript-{session_id}").as_bytes(),
        )
        .await;
        let stdout = seal_and_register(
            &self.store,
            &self.build,
            "stdout",
            format!("stdout-{session_id}").as_bytes(),
        )
        .await;
        let stderr = seal_and_register(
            &self.store,
            &self.build,
            "stderr",
            format!("stderr-{session_id}").as_bytes(),
        )
        .await;
        let usage = if index == 4 {
            None
        } else {
            Some(UsageTotalsV2 {
                input_tokens: 1,
                output_tokens: 1,
                reported_cost_micro_usd: Some(MicroUsd::new(if index == 1 {
                    7
                } else if index == 2 {
                    2
                } else {
                    7
                })),
                ..UsageTotalsV2::default()
            })
        };
        process
            .verify_terminal_evidence_with_packet_bytes(
                &self.build.cas,
                session_id,
                &assignment.packet,
                assignment.packet_artifact.sealed,
                &assignment.command.packet_bytes,
                factory_kernel::process::TerminalArtifactSeals {
                    transcript: transcript.sealed,
                    stdout: stdout.sealed,
                    stderr: stderr.sealed,
                    partial_transcript: None,
                },
                assertion,
                usage,
            )
            .await
            .expect("verified evidence")
    }
}

fn wire_packet(
    packet: &AssignmentPacketV2,
    system_prompt: &[u8],
    assignment_prompt: &[u8],
) -> factory_protocol::AssignmentPacketWireV2 {
    factory_protocol::AssignmentPacketWireV2 {
        format_version: 2,
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
        repository_base_identity: digest(900).to_hex(),
        factory_base_identity: digest(901).to_hex(),
        ticket_attempt_id: packet
            .ticket_attempt_id
            .map(factory_protocol::TicketAttemptId::get),
        candidate_id: packet.candidate_id.map(factory_protocol::CandidateId::get),
        assignment_evidence: Vec::new(),
        system_prompt_artifact_id: packet.system_prompt_artifact_id.get(),
        assignment_prompt_artifact_id: packet.assignment_prompt_artifact_id.get(),
        required_read_manifest_artifact_id: packet.required_read_manifest_artifact_id.get(),
        system_prompt_digest: ContentDigest::of_bytes(system_prompt).to_hex(),
        assignment_prompt_digest: ContentDigest::of_bytes(assignment_prompt).to_hex(),
        system_prompt_bytes_b64: base64(system_prompt),
        assignment_prompt_bytes_b64: base64(assignment_prompt),
        policy_digest: packet.policy_digest.to_hex(),
        policy_byte_limit: packet.policy_byte_limit,
        policy_bytes_b64: base64(&packet.policy_bytes),
        policy_entrypoint: packet.policy_entrypoint.as_str().to_owned(),
        workspace_root: packet.workspace_root.as_str().to_owned(),
        staging_root: packet.staging_root.as_str().to_owned(),
        model: factory_protocol::AssignmentModelWireV2 {
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
        limits: factory_protocol::AssignmentLimitsWireV2 {
            turn_limit: packet.limits.turn_limit,
            wall_limit_millis: packet.limits.wall_limit.get(),
            output_byte_limit: packet.limits.output_byte_limit,
        },
        runtime: factory_protocol::AssignmentRuntimeWireV2 {
            host_executable: packet.runtime.host_executable.as_str().to_owned(),
            core_head: packet.runtime.core_head.clone(),
            core_source_digest: packet.runtime.core_source_digest.to_hex(),
            rust_toolchain: packet.runtime.rust_toolchain.clone(),
            credential_env: packet.runtime.credential_env.clone(),
        },
        required_reads: packet
            .required_reads
            .iter()
            .map(|read| factory_protocol::AssignmentReadWireV2 {
                path: read.path.as_str().to_owned(),
                digest: read.digest.to_hex(),
                reason: read.reason.clone(),
            })
            .collect(),
        tools: vec!["workspace_read".to_owned(), "work_complete".to_owned()],
        terminal_operations: vec!["work_complete".to_owned()],
        remaining_campaign_allowance_micro_usd: packet.remaining_campaign_allowance.get(),
        aggregate_revision: packet.revision.get(),
        packet_digest: String::new(),
    }
}

fn base64(bytes: &[u8]) -> String {
    const ALPHABET: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut output = String::new();
    for chunk in bytes.chunks(3) {
        output.push(ALPHABET[(chunk[0] >> 2) as usize] as char);
        output.push(
            ALPHABET[((chunk[0] & 3) << 4 | chunk.get(1).copied().unwrap_or(0) >> 4) as usize]
                as char,
        );
        if let Some(second) = chunk.get(1) {
            output.push(
                ALPHABET[((second & 15) << 2 | chunk.get(2).copied().unwrap_or(0) >> 6) as usize]
                    as char,
            );
        } else {
            output.push('=');
        }
        if let Some(third) = chunk.get(2) {
            output.push(ALPHABET[(third & 63) as usize] as char);
        } else {
            output.push('=');
        }
    }
    output
}

fn test_database_url() -> String {
    let url = std::env::var("FACTORY_TEST_DATABASE_URL").expect("FACTORY_TEST_DATABASE_URL");
    let name = url
        .rsplit('/')
        .next()
        .and_then(|part| part.split('?').next())
        .unwrap();
    assert!(
        name.strip_prefix("factory_test_v3_").is_some_and(
            |suffix| !suffix.is_empty() && suffix.bytes().all(|byte| byte.is_ascii_digit())
        ),
        "FACTORY_TEST_DATABASE_URL must name exactly factory_test_v3_<digits>"
    );
    url
}

async fn install_build(store: &KernelStore) -> InstalledBuild {
    let cas = CasStore::new_with_seed(
        std::env::temp_dir().join(unique("process-lifecycle-cas")),
        4 * 1024 * 1024,
        unique_number(),
    )
    .unwrap();
    let staging = cas.runtime_root().join("staging");
    fs::create_dir_all(&staging).unwrap();
    fs::write(staging.join("qualification"), b"qualified").unwrap();
    let qualification = cas.adopt(&staging, "qualification").unwrap();
    let status = store.kernel_build_status().await.unwrap();
    // One fixture build identity must be unique across separately spawned
    // serial test binaries sharing the same disposable PostgreSQL database.
    // The three subordinate build-material identities derive from that same
    // serial but use distinct values, so they cannot accidentally alias it or
    // one another in a uniqueness/lineage judge.
    let build_serial = unique_number();
    let build_id = factory_protocol::KernelBuildId::new(digest(build_serial));
    let source_digest = digest(build_serial.wrapping_add(1));
    let binary_digest = digest(build_serial.wrapping_add(2));
    let core_source_digest = digest(build_serial.wrapping_add(3));
    let receipt = store
        .install_kernel_build(
            &cas,
            &InstallKernelBuild {
                principal: "operator".to_owned(),
                command_id: unique("build"),
                expected_revision: ExpectedRevision::new(status.aggregate_revision),
                build_id,
                source_digest,
                binary_digest,
                schema_identity: SCHEMA_IDENTITY.to_owned(),
                host_executable_path: "/opt/factory/factory-pi-host".to_owned(),
                core_head: "0123456789abcdef0123456789abcdef01234567".to_owned(),
                rust_toolchain: "nightly-2026-07-24".to_owned(),
                core_source_digest,
                qualification_receipt: qualification,
            },
        )
        .await
        .unwrap();
    InstalledBuild {
        cas,
        receipt,
        kernel_build_id: build_id,
    }
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
        .join("staging")
        .join(format!("{label}-{}", unique_number()));
    fs::write(&path, bytes).unwrap();
    let sealed = build
        .cas
        .adopt(path.parent().unwrap(), path.file_name().unwrap())
        .unwrap();
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
        .unwrap();
    SealedArtifact {
        artifact_id: receipt.artifact_id,
        sealed,
    }
}

fn unregistered_artifact(build: &InstalledBuild, bytes: &[u8]) -> CasArtifact {
    let path = build
        .cas
        .runtime_root()
        .join("staging")
        .join(format!("unregistered-{}", unique_number()));
    fs::write(&path, bytes).unwrap();
    build
        .cas
        .adopt(path.parent().unwrap(), path.file_name().unwrap())
        .unwrap()
}

fn canonical_manifest(required: &[ReadExactFileV2]) -> Vec<u8> {
    let mut bytes = b"factory-read-manifest-v1\0".to_vec();
    bytes.extend_from_slice(&(required.len() as u32).to_be_bytes());
    for item in required {
        bytes.extend_from_slice(&(item.path.as_str().len() as u32).to_be_bytes());
        bytes.extend_from_slice(item.path.as_str().as_bytes());
        bytes.extend_from_slice(&item.digest.as_bytes());
        bytes.extend_from_slice(&(item.reason.len() as u32).to_be_bytes());
        bytes.extend_from_slice(item.reason.as_bytes());
    }
    bytes
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

async fn admit_application(
    store: &KernelStore,
    build: &InstalledBuild,
    repository_key: &str,
    repository_path: &str,
) -> ApplicationRevisionId {
    let root = build.cas.runtime_root().join(unique("application-source"));
    fs::create_dir_all(&root).unwrap();
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
        let bytes = format!("template-{index}");
        fs::write(root.join(path), &bytes).unwrap();
        templates.push((*path, ContentDigest::of_bytes(bytes.as_bytes())));
    }
    fs::create_dir_all(root.join("policies")).unwrap();
    fs::write(root.join("policies/test.luau"), b"return {}\n").unwrap();
    let application_key = unique("application");
    fs::write(
        root.join("bundle.json"),
        minimal_bundle_json(
            &application_key,
            repository_key,
            repository_path,
            &templates,
        ),
    )
    .unwrap();
    let admitted = store
        .admit_compiled_application(
            &build.cas,
            &AdmitCompiledApplication {
                principal: "architect".to_owned(),
                command_id: unique("application"),
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
            principal: ArchitectPrincipalV2::parse("architect").expect("principal"),
            command_id: unique("application-activate"),
            expected_revision: ExpectedRevision::new(admitted.resulting_revision),
            application_key: ApplicationKey::parse(application_key).expect("application key"),
            application_revision_id: admitted.application_revision_id,
            rationale: SealedArtifactReferenceV2 {
                artifact_id: rationale.artifact_id,
                digest: rationale.sealed.digest(),
                byte_length: rationale.sealed.byte_length(),
            },
        })
        .await
        .expect("activate application");
    admitted.application_revision_id
}

fn minimal_bundle_json(
    application: &str,
    repository: &str,
    path: &str,
    templates: &[(&str, ContentDigest)],
) -> String {
    use factory_protocol::{
        ApplicationBundleWireV2, AssignmentRoleWireV2, CommandWireV2, CommitMessageWireV2,
        ExecutableWireV2, GitWireV2, LimitsWireV2, ModelWireV2, PolicyWireV2, RepositoryWireV2,
        RequiredReadWireV2, TemplateWireV2, TicketBoundsWireV2, TicketPolicyWireV2,
        ValidationWireV2, canonical_application_bundle_json_v2,
    };
    let template = |index: usize| TemplateWireV2 {
        source_path: templates[index].0.to_owned(),
        digest: templates[index].1.to_hex(),
        placeholders: Vec::new(),
        rendered_byte_limit: 4096,
    };
    let command = |name: &str| CommandWireV2 {
        name: name.to_owned(),
        executable: ExecutableWireV2 {
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
        |name: &str, system: usize, assignment: usize| AssignmentRoleWireV2 {
            assignment_role: name.to_owned(),
            system_template: template(system),
            assignment_template: template(assignment),
            policy: PolicyWireV2 {
                source_path: "policies/test.luau".to_owned(),
                digest: ContentDigest::of_bytes(b"return {}\n").to_hex(),
                byte_limit: 1024,
                entrypoint: "factory_policy".to_owned(),
            },
            tools: vec!["workspace_read".to_owned()],
            model: ModelWireV2 {
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
            limits: LimitsWireV2 {
                turn_limit: 1,
                wall_limit_millis: 10_000,
                output_byte_limit: 4096,
            },
        };
    canonical_application_bundle_json_v2(&ApplicationBundleWireV2 {
        format_version: 2,
        application_key: application.to_owned(),
        predecessor_bundle: None,
        repository: RepositoryWireV2 {
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
        ticket_policy: TicketPolicyWireV2 {
            low_water: 1,
            target: 1,
            maximum: 1,
            proposal_maximum: 1,
            ticket_bounds: TicketBoundsWireV2 {
                narrative_byte_limit: 1,
                acceptance_criteria_limit: 1,
                contract_read_limit: 1,
            },
        },
        required_reads: vec![RequiredReadWireV2 {
            path: "AGENTS.md".to_owned(),
            reason: "test".to_owned(),
        }],
        reproducer_profiles: Vec::new(),
        validation_profiles: ValidationWireV2 {
            focused: vec![command("focused")],
            full: vec![command("full")],
        },
        git_policy: GitWireV2 {
            forbidden_paths: Vec::new(),
            delivery_mode: "local_fast_forward_only".to_owned(),
            provenance_trailers_required: true,
        },
        commit_message_policy: CommitMessageWireV2 {
            subject_byte_limit: 1,
            body_byte_limit: 1,
        },
    })
    .unwrap()
}
