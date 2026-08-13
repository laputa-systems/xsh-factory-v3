//! Provider-free end-to-end judge for the one-ticket MVP circuit.
//!
//! This is intentionally an isolated integration harness. It owns one exact
//! disposable database, one local Git repository, and three small Deno actors;
//! it imports no Pi SDK and supplies no provider credential. The actors may
//! only use the inherited session socket, while the test moves between offices
//! through the public typed kernel authorities.

use std::{
    ffi::OsString,
    fs,
    path::{Path, PathBuf},
    process::Command,
    sync::{
        Arc,
        atomic::{AtomicU64, Ordering},
    },
    time::Duration,
};

use factory_kernel::installed_runtime::{
    InstalledApprovedToolsQualificationV1, InstalledKernelBuildReceiptV1,
    InstalledKernelExecutionTools, InstalledRuntimeManifest, InstalledRuntimeQualification,
    qualify_kernel_binary_v1, qualify_kernel_source_v1,
};
use factory_kernel::{
    assignment_runtime::{
        AssignmentLaunchOutcome, AssignmentMaterializationRequest,
        materialize_and_launch_assignment,
    },
    cas::{CasArtifact, CasStore},
    command_supervision::{
        CommandExpectation, CommandStdin, CommandWorkspace, ComparisonRevision,
        DeterministicCommand, ExactBytes,
    },
    decision_store::{DecideCandidate, SponsorTicket},
    durable_authority::{
        DeliverAcceptedCandidate, DurableAssignmentLaunchRequest, DurableAssignmentTarget,
        DurableAuthorityResolver,
    },
    forum_store::ForumStore,
    git::{DefaultBranchName, GitCustody, WorktreeKind, WorktreeName},
    local_transport::{LocalDaemon, LocalTransportConfig},
    operator_rpc::ArchitectTransitionResolver,
    process::{CreateAssignment, ProcessStore, StartCampaign},
    process_custody::{PiHostSpawnSpec, ProcessSupervisionSpec},
    session_runtime::SessionRuntimeVerifier,
    storage::{
        ActivateApplicationRevision, AdmitCompiledApplication, InstallQualifiedKernelBuild,
        KernelStore, RegisterArtifact, RegisterRepository, SCHEMA_IDENTITY,
    },
    ticket_store::{ClaimOutcome, CurrentHeadRequalification, TicketStore},
};
use factory_protocol::{
    ASSIGNMENT_PACKET_V1_FORMAT, AbsoluteHostPath, AggregateRevision, ApplicationBundleWireV1,
    ApplicationKey, ApplicationRevisionId, ArchitectPrincipalV1, AssignmentCredentialWireV1,
    CommandWireV1, CommitMessageWireV1, ContentDigest, CredentialDescriptorV1, DurationMillis,
    ExecutableWireV1, ExpectedRevision, GitWireV1, LimitsWireV1, MicroUsd, ModelProfileV1,
    ModelWireV1, Office, OfficeWireV1, ReadExactFileV1, ReadObservationV1, RepositoryRelativePath,
    RepositoryWireV1, RuntimeIdentityV1, RuntimeRelativePath, SealedArtifactReferenceV1,
    SessionLimitsV1, SponsorshipDecisionV1, TemplateWireV1, ThinkingLevelV1, TicketBoundsWireV1,
    TicketPolicyWireV1, ValidationWireV1, canonical_application_bundle_json_v1,
    canonical_command_profile_json_v1,
};

static NEXT: AtomicU64 = AtomicU64::new(1);

#[test]
#[ignore = "requires FACTORY_TEST_DATABASE_URL for one disposable PostgreSQL database"]
fn provider_free_full_vertical_delivers_one_local_commit() {
    smol::block_on(async {
        let fixture = Fixture::new().await;
        fixture.run().await;
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
    tickets: TicketStore,
    forum: ForumStore,
    runner: factory_kernel::command_supervision::CommandRunner,
    git: Arc<GitCustody>,
    installed: InstalledKernelBuildReceiptV1,
    execution: InstalledKernelExecutionTools,
    daemon: LocalDaemon,
    daemon_root: PathBuf,
    application: ApplicationRevisionId,
    campaign: factory_kernel::process::CampaignReceipt,
}

impl Fixture {
    async fn new() -> Self {
        let root = std::env::temp_dir().join(unique("factory-v3-full-vertical"));
        fs::create_dir_all(&root).expect("fixture root");
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
            &["config", "user.name", "Synthetic Factory"],
        );
        git(
            &repository,
            &git_path,
            &["config", "user.email", "factory@example.test"],
        );
        fs::write(repository.join("AGENTS.md"), b"read this exact contract\n")
            .expect("agents contract");
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
        let (cas, build, installed) = install_build(&store, &root).await;
        let kernel_build_id = installed.kernel_build_id();
        let process = store.process_store();
        let tickets = store.ticket_store();
        let forum = store.forum_store();
        let execution = installed
            .execution_tools(&root.join("git-runtime"))
            .expect("installed execution tools");
        let runner = execution.command_runner().clone();
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
                aggregate_budget: MicroUsd::new(50),
                deadline_unix_millis: 4_000_000_000_000,
                delivery_target: 1,
            })
            .await
            .expect("active application campaign");
        let daemon_root = std::env::temp_dir().join(unique("factory-v3-full-daemon"));
        let daemon = LocalDaemon::bind(LocalTransportConfig::new(daemon_root.clone()), &store)
            .await
            .expect("local daemon");
        Self {
            root,
            repository,
            git_path,
            store,
            cas,
            build,
            kernel_build_id,
            process,
            tickets,
            forum,
            runner,
            git,
            installed,
            execution,
            daemon,
            daemon_root,
            application: application.id,
            campaign,
        }
    }

    async fn run(&self) {
        let product = self
            .launch_materialized(DurableAssignmentTarget::Product)
            .await;
        assert_eq!(
            product.outcome.session.terminal.cost,
            factory_protocol::TerminalCostV1::Known(MicroUsd::new(10))
        );

        // This bounded application-side read is the sole route from the
        // Product terminal to an external Architect decision. The actor never
        // supplied a ticket revision ID in its socket request.
        let ticket = self
            .tickets
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

        let resolver = Arc::new(DurableAuthorityResolver::new(
            self.store.clone(),
            self.cas.clone(),
            self.runner.clone(),
            Arc::clone(&self.git),
        ));
        let qualified = self
            .git
            .qualify_repository(&self.repository, DefaultBranchName::parse("main").unwrap())
            .expect("clean primary repository");
        let requalification = self.requalify(&qualified).await;
        let status = self
            .tickets
            .ticket_buffer_status(self.campaign.campaign_id)
            .await
            .expect("ticket buffer");
        let head = status.oldest_sponsored_ticket.expect("sponsored FIFO head");
        let claimed = self
            .tickets
            .claim_sponsored_ticket(&factory_kernel::ticket_store::ClaimSponsoredTicket {
                principal: "daemon".to_owned(),
                command_id: unique("claim-sponsored-ticket"),
                campaign_id: self.campaign.campaign_id,
                expected_campaign_revision: ExpectedRevision::new(status.campaign_revision),
                ticket_revision_id: head.ticket_revision_id,
                expected_ticket_revision: ExpectedRevision::new(head.revision),
                requalification,
            })
            .await
            .expect("trusted requalification claim");
        let attempt = match claimed.outcome {
            ClaimOutcome::Claimed { ticket_attempt_id } => ticket_attempt_id,
            other => panic!("expected reproduced ticket claim, got {other:?}"),
        };

        let engineering = self
            .launch_materialized(DurableAssignmentTarget::Engineering {
                ticket_attempt_id: attempt,
            })
            .await;
        assert_eq!(
            engineering.outcome.session.terminal.cost,
            factory_protocol::TerminalCostV1::Known(MicroUsd::new(10))
        );

        let downstream = self
            .tickets
            .ticket_buffer_status(self.campaign.campaign_id)
            .await
            .expect("post-candidate buffer")
            .downstream_action
            .expect("candidate Quality action");
        let candidate = downstream.candidate_id;
        assert_eq!(downstream.ticket_attempt_id, attempt);
        let quality = self
            .launch_materialized(DurableAssignmentTarget::Quality {
                ticket_attempt_id: attempt,
                candidate_id: candidate,
            })
            .await;
        assert_eq!(
            quality.outcome.session.terminal.cost,
            factory_protocol::TerminalCostV1::Known(MicroUsd::new(10))
        );

        let candidate_status = self
            .tickets
            .ticket_buffer_status(self.campaign.campaign_id)
            .await
            .expect("awaiting Architect")
            .downstream_action
            .expect("Architect decision context");
        let review_id = review_id_from_stdout(&quality.stdout);
        let decision = resolver
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
        let delivery = resolver
            .deliver_accepted_candidate(DeliverAcceptedCandidate {
                principal: "kernel-local-delivery".to_owned(),
                command_id: unique("guarded-local-delivery"),
                candidate_id: candidate,
            })
            .await
            .expect("guarded local fast-forward and durable delivery");
        assert!(delivery.campaign_completed);

        let campaign = self
            .process
            .campaign_status(self.campaign.campaign_id)
            .await
            .expect("completed campaign status");
        assert_eq!(campaign.state, factory_protocol::CampaignState::Completed);
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
        assert!(costs.iter().all(
            |row| row.cost == Some(factory_protocol::TerminalCostV1::Known(MicroUsd::new(10)))
        ));
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

        for launched in [product, engineering, quality] {
            self.git
                .cleanup_worktree(launched.outcome.workspace)
                .expect("actor worktree cleanup");
            let _ = fs::remove_dir_all(launched.outcome.staging_root);
        }
    }

    async fn launch_materialized(&self, target: DurableAssignmentTarget) -> ActorLaunch {
        let campaign = self
            .process
            .campaign_status(self.campaign.campaign_id)
            .await
            .expect("campaign before materialized actor launch");
        let resolver = Arc::new(DurableAuthorityResolver::new(
            self.store.clone(),
            self.cas.clone(),
            self.runner.clone(),
            Arc::clone(&self.git),
        ));
        let outcome = materialize_and_launch_assignment(
            &self.store,
            &self.cas,
            &self.daemon,
            &self.installed,
            &self.execution,
            resolver,
            AssignmentMaterializationRequest {
                principal: "daemon".to_owned(),
                command_id: unique("materialize-and-launch"),
                expected_campaign_revision: ExpectedRevision::new(campaign.revision),
                campaign_id: self.campaign.campaign_id,
                application_revision_id: self.application,
                target,
                attempt_ordinal: 1,
                credential_environment_value: OsString::from("provider-free-fake-value"),
            },
        )
        .await
        .expect("installed materializer and session runtime");
        let stdout = fs::read_to_string(outcome.staging_root.join("stdout.log"))
            .expect("captured actor stdout");
        ActorLaunch { outcome, stdout }
    }

    async fn requalify(
        &self,
        repository: &factory_kernel::git::QualifiedRepository,
    ) -> CurrentHeadRequalification {
        let profile = reproducer_wire();
        let command = deterministic_command(profile, b"expected\n", b"none\n");
        let workspace = CommandWorkspace::open(&self.repository).expect("primary workspace");
        let first = self
            .runner
            .run(&workspace, &command)
            .expect("first requalification run");
        let second = self
            .runner
            .run(&workspace, &command)
            .expect("second requalification run");
        assert_eq!(first.stdout(), b"actual\n");
        assert_eq!(first.stderr(), b"none\n");
        assert_eq!(second.stdout(), b"actual\n");
        let first_artifact = self
            .seal_kernel("requalification-first", first.stdout())
            .await;
        let second_artifact = self
            .seal_kernel("requalification-second", second.stdout())
            .await;
        CurrentHeadRequalification {
            current_head_commit: repository.snapshot().base_commit().to_string(),
            current_head_tree: repository.snapshot().base_tree().to_string(),
            first_actual_observation_artifact_id: first_artifact.artifact_id,
            second_actual_observation_artifact_id: second_artifact.artifact_id,
        }
    }

    async fn launch_actor(
        &self,
        office: Office,
        ticket_attempt_id: Option<factory_protocol::TicketAttemptId>,
        candidate_id: Option<factory_protocol::CandidateId>,
        workspace: PathBuf,
        actor: Actor,
    ) -> ManualActorLaunch {
        let staging = self.root.join(unique("actor-staging"));
        fs::create_dir_all(&staging).expect("actor staging");
        let required_read = ReadExactFileV1 {
            path: RepositoryRelativePath::parse("AGENTS.md").expect("required read path"),
            digest: ContentDigest::of_bytes(b"read this exact contract\n"),
            reason: "application contract".to_owned(),
        };
        let manifest = self
            .seal_kernel(
                "required-read-manifest",
                &canonical_manifest(&[ReadObservationV1 {
                    path: required_read.path.clone(),
                    digest: required_read.digest,
                }]),
            )
            .await;
        let system = self
            .seal_kernel("system-prompt", b"provider-free system prompt\n")
            .await;
        let assignment_prompt = self
            .seal_kernel("assignment-prompt", b"exact assignment\n")
            .await;
        let identity = self
            .process
            .reserve_assignment_identity()
            .await
            .expect("assignment id");
        let campaign = self
            .process
            .campaign_status(self.campaign.campaign_id)
            .await
            .expect("campaign status");
        let terminal = match office {
            Office::ProductResearch => TerminalOperationV1::WorkComplete,
            Office::Engineering => TerminalOperationV1::CandidateSubmit,
            Office::Quality => TerminalOperationV1::QualitySubmitReview,
        };
        let mut packet = AssignmentPacketV1 {
            format_version: ASSIGNMENT_PACKET_V1_FORMAT,
            campaign_id: self.campaign.campaign_id,
            assignment_id: identity.assignment_id(),
            kernel_build_id: self.kernel_build_id,
            application_revision_id: self.application,
            office,
            target: actor.name().to_owned(),
            ticket_attempt_id,
            candidate_id,
            system_prompt_artifact_id: system.artifact_id,
            assignment_prompt_artifact_id: assignment_prompt.artifact_id,
            required_read_manifest_artifact_id: manifest.artifact_id,
            workspace_root: AbsoluteHostPath::parse(workspace.to_string_lossy().into_owned())
                .expect("workspace absolute"),
            staging_root: AbsoluteHostPath::parse(staging.to_string_lossy().into_owned())
                .expect("staging absolute"),
            model: model(),
            limits: SessionLimitsV1 {
                turn_limit: 5,
                wall_limit: DurationMillis::new(30_000),
                output_byte_limit: 128 * 1024,
            },
            runtime: runtime_identity(),
            required_reads: vec![required_read],
            terminal_operations: vec![terminal],
            remaining_campaign_allowance: campaign_remaining(&campaign),
            revision: campaign.revision,
            packet_digest: digest(1),
        };
        let mut wire = wire_packet(
            &packet,
            b"provider-free system prompt\n",
            b"exact assignment\n",
            actor.tools(),
        );
        let packet_digest = unsigned_assignment_packet_digest_v1(&wire).expect("packet digest");
        wire.packet_digest = packet_digest.to_hex();
        packet.packet_digest = packet_digest;
        let bytes = canonical_assignment_packet_json_v1(&wire)
            .expect("canonical packet")
            .into_bytes();
        let packet_artifact = self.seal_kernel("assignment-packet", &bytes).await;
        let assignment = self
            .process
            .create_assignment(
                &self.cas,
                &CreateAssignment {
                    principal: "daemon".to_owned(),
                    command_id: unique("create-assignment"),
                    expected_campaign_revision: ExpectedRevision::new(campaign.revision),
                    identity,
                    packet: packet.clone(),
                    packet_bytes: bytes.clone(),
                    packet_artifact: packet_artifact.sealed,
                    required_read_manifest_artifact_id: manifest.artifact_id,
                    attempt_ordinal: 1,
                },
            )
            .await
            .expect("typed assignment");

        let host = staging.join("provider-free-fake-host.ts");
        fs::write(&host, actor.source()).expect("actor source");
        let config = staging.join("deno.json");
        let lock = staging.join("deno.lock");
        fs::write(
            &config,
            "{\"lock\":{\"path\":\"./deno.lock\",\"frozen\":true},\"nodeModulesDir\":\"none\"}\n",
        )
        .expect("config");
        fs::write(
            &lock,
            "{\"version\":\"5\",\"specifiers\":{},\"jsr\":{},\"npm\":{},\"remote\":{}}\n",
        )
        .expect("lock");
        let deno_dir = staging.join("deno-cache");
        fs::create_dir_all(&deno_dir).expect("deno cache");
        let spawn = PiHostSpawnSpec::new_for_assignment(
            deno(),
            host,
            config,
            lock,
            workspace.clone(),
            0,
            deno_dir,
            Vec::<(OsString, OsString)>::new(),
        )
        .expect("fake Deno actor spawn");
        let supervision = ProcessSupervisionSpec::new(
            staging.join("stdout.log"),
            staging.join("stderr.log"),
            128 * 1024,
            128 * 1024,
            Duration::from_secs(30),
            Duration::from_millis(100),
        )
        .expect("supervision");
        let runtime = match office {
            Office::ProductResearch => None,
            Office::Engineering | Office::Quality => Some(CandidateQualitySessionRuntime::new(
                self.store.decision_store(),
                Arc::clone(&self.git),
                Arc::new(DurableAuthorityResolver::new(
                    self.store.clone(),
                    self.cas.clone(),
                    self.runner.clone(),
                    Arc::clone(&self.git),
                )),
            )),
        };
        let request = SessionLaunchRequest {
            principal: "daemon".to_owned(),
            command_id: unique("launch-session"),
            expected_assignment_revision: ExpectedRevision::new(assignment.resulting_revision),
            assignment_id: assignment.assignment_id,
            packet_digest,
            packet: packet.clone(),
            canonical_packet_bytes: bytes.clone(),
            packet_artifact: packet_artifact.sealed,
            spawn,
            supervision,
            workspace_root: workspace,
            expected_read_manifest_artifact_id: manifest.artifact_id,
            required_reads: packet.required_reads.clone(),
            candidate_quality_runtime: runtime,
        };
        let outcome = launch_session(
            &self.process,
            &self.forum,
            &self.tickets,
            &self.runner,
            &self.daemon,
            &self.cas,
            request,
            &ExactVerifier {
                packet,
                bytes,
                packet_artifact: packet_artifact.sealed,
            },
        )
        .await
        .expect("fake actor session");
        ManualActorLaunch {
            outcome,
            stdout: fs::read_to_string(staging.join("stdout.log")).expect("actor stdout"),
        }
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
    stdout: String,
}

#[allow(dead_code)]
struct ManualActorLaunch {
    outcome: factory_kernel::session_runtime::SessionRuntimeOutcome,
    stdout: String,
}

struct ExactVerifier {
    packet: AssignmentPacketV1,
    bytes: Vec<u8>,
    packet_artifact: CasArtifact,
}

impl SessionRuntimeVerifier for ExactVerifier {
    fn verify_packet(
        &self,
        packet: &AssignmentPacketV1,
        bytes: &[u8],
    ) -> Result<(), factory_kernel::session_runtime::RuntimeVerificationError> {
        if packet != &self.packet || bytes != self.bytes {
            return Err(
                factory_kernel::session_runtime::RuntimeVerificationError::PacketSealMismatch,
            );
        }
        factory_protocol::verify_assignment_packet_v1(bytes, &packet.packet_digest.to_hex())
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
            || self.packet_artifact.byte_length() == 0
        {
            return Err(
                factory_kernel::session_runtime::RuntimeVerificationError::RuntimeIdentity(
                    "provider-free runtime mismatch".to_owned(),
                ),
            );
        }
        Ok(())
    }
}

#[derive(Clone, Copy)]
enum Actor {
    Product,
    Engineering {
        attempt: factory_protocol::TicketAttemptId,
    },
    Quality,
}

impl Actor {
    fn name(self) -> &'static str {
        match self {
            Self::Product => "product",
            Self::Engineering { .. } => "engineering",
            Self::Quality => "quality",
        }
    }
    fn tools(self) -> Vec<String> {
        match self {
            Self::Product => vec!["workspace_read", "artifact_seal", "product_submit_ticket"],
            Self::Engineering { .. } => vec![
                "workspace_read",
                "artifact_seal",
                "candidate_checkpoint_regression",
                "candidate_submit",
            ],
            Self::Quality => vec![
                "workspace_read",
                "artifact_seal",
                "quality_run_full_suite",
                "quality_submit_review",
            ],
        }
        .into_iter()
        .map(str::to_owned)
        .collect()
    }
    fn source(self) -> String {
        match self {
            Self::Product => format!(
                "const ROLE = 'product';\nconst REPRODUCER = {};\n{ACTOR_SOURCE}",
                js_string(&canonical_command_profile_json_v1(&reproducer_wire()).unwrap())
            ),
            Self::Engineering { attempt } => format!(
                "const ROLE = 'engineering';\nconst ATTEMPT = {};\n{ACTOR_SOURCE}",
                attempt.get()
            ),
            Self::Quality => format!("const ROLE = 'quality';\n{ACTOR_SOURCE}"),
        }
    }
}

const ACTOR_SOURCE: &str = r#"
const io = await Deno.open('/dev/fd/0', { read: true, write: true });
const enc = new TextEncoder(); const dec = new TextDecoder(); let sequence = 0;
async function exact(n) { const out = new Uint8Array(n); let at = 0; while (at < n) { const got = await io.read(out.subarray(at)); if (got === null) throw new Error('closed'); at += got; } return out; }
async function write(bytes) { let at = 0; while (at < bytes.length) at += await io.write(bytes.subarray(at)); }
async function line() { const bytes = []; for (;;) { const one = await exact(1); if (one[0] === 10) return dec.decode(new Uint8Array(bytes)); bytes.push(one[0]); } }
async function frame() { const p = await exact(4); const n = new DataView(p.buffer).getUint32(0, false); return JSON.parse(dec.decode(await exact(n))); }
async function call(operation, fields) { const request_id = `actor-${++sequence}`; const body = enc.encode(JSON.stringify({protocol_version:1, request_id, operation, ...fields})); const p = new Uint8Array(4); new DataView(p.buffer).setUint32(0, body.length, false); await write(p); await write(body); const response = await frame(); if (response.request_id !== request_id || response.operation !== operation || response.error_code) throw new Error(`${operation}: ${response.error_code ?? 'identity'}`); return response; }
const b64 = (text) => btoa(text);
const admission = JSON.parse(await line()); const packetBytes = Uint8Array.from(atob(admission.packet_b64), c => c.charCodeAt(0)); const packet = JSON.parse(dec.decode(packetBytes));
await call('session.verify_packet', {packet_digest: admission.packet_digest, packet_bytes_b64: admission.packet_b64});
const read = await call('workspace.read', {repository_relative_path: packet.required_reads[0].path}); if (read.blake3 !== packet.required_reads[0].digest) throw new Error('required read mismatch');
async function seal(name, text) { await Deno.writeTextFile(`${packet.staging_root}/${name}`, text); const r = await call('artifact.seal_workspace_file', {client_command_id:`seal-${name}`, expected_revision:admission.session_revision, staging_relative_path:name, byte_limit:131072}); return {artifact_id:r.artifact_id,digest:r.digest,byte_length:r.byte_length}; }
function ref(value) { return value; }
if (ROLE === 'product') {
  const narrative = await seal('narrative.md', 'synthetic behavior remains incorrect\n');
  const evidence = await seal('evidence.md', 'reproducer proves observable divergence\n');
  const command = await seal('reproducer.json', REPRODUCER);
  const expectedOut = await seal('expected.out', 'expected\n'); const expectedErr = await seal('expected.err', 'none\n');
  const actualOne = await seal('actual-one.out', 'actual\n'); const actualOneErr = await seal('actual-one.err', 'none\n');
  const actualTwo = await seal('actual-two.out', 'actual\n'); const actualTwoErr = await seal('actual-two.err', 'none\n');
  await call('product.submit_ticket', {client_command_id:'submit-ticket', expected_revision:admission.session_revision, title:'Repair exact synthetic output', mission_value:'prove one correct local change', scope:'reproduce.sh only', contract_owner:'product', risk:'low', narrative, evidence, acceptance_criteria:['reproducer prints expected'], contract_reads:[{path:'AGENTS.md',reason:'repository contract'}], duplicate_search:{query:'synthetic exact output',limit:5}, reproducer_profile:'reproduce', reproducer:{comparison_rule_version:1, command, stdin:null, expected_observation:{exit_status:0,stdout:expectedOut,stderr:expectedErr}, first_observation:{exit_status:0,stdout:actualOne,stderr:actualOneErr}, second_observation:{exit_status:0,stdout:actualTwo,stderr:actualTwoErr}}});
}
if (ROLE === 'engineering') {
  await call('candidate.checkpoint_regression', {client_command_id:'checkpoint', expected_revision:admission.session_revision, regression_command:'reproduce', expected_failure:`ticket-attempt-${ATTEMPT}-reproducer`});
  await Deno.writeTextFile(`${packet.workspace_root}/reproduce.sh`, '#!/bin/sh\nprintf \'expected\\n\'\nprintf \'none\\n\' >&2\n'); await Deno.chmod(`${packet.workspace_root}/reproduce.sh`, 0o755);
  const report = await seal('engineering-report.md', 'changed reproducer after kernel checkpoint\n'); const risks = await seal('engineering-risks.md', 'none\n');
  await call('candidate.submit', {client_command_id:'candidate-submit', expected_revision:admission.session_revision, engineering_report:report, commit_subject:'Repair synthetic reproducer output', commit_body:'', regression_test_identity:'reproduce', risks});
}
if (ROLE === 'quality') {
  const suite = await call('quality.run_full_suite', {client_command_id:'quality-suite', expected_revision:admission.session_revision, validation_profile:'full'});
  const rationale = await seal('quality-rationale.md', 'independent complete suite passed\n'); const risks = await seal('quality-risks.md', 'none\n'); const probes = await seal('quality-probes.md', 'fresh exact candidate tree\n');
  const review = await call('quality.submit_review', {client_command_id:'quality-review', expected_revision:admission.session_revision, full_suite_validation_id:suite.validation_id, verdict:'accept', rationale, risks, additional_probes:probes});
  console.log(`FULL_VERTICAL_REVIEW_ID=${review.review_id}`);
}
const transcriptBytes = enc.encode(JSON.stringify({provider:'none',role:ROLE}) + '\n'); const gzip = new Blob([transcriptBytes]).stream().pipeThrough(new CompressionStream('gzip')); await Deno.writeFile(`${packet.staging_root}/session.ndjson.gz`, new Uint8Array(await new Response(gzip).arrayBuffer()));
const transcript = await call('session.seal_artifact', {client_command_id:'seal-transcript', expected_revision:admission.session_revision, staging_relative_path:'session.ndjson.gz', role:'pi_transcript_gzip', byte_limit:131072});
const terminal = ROLE === 'product' ? 'work_complete' : ROLE === 'engineering' ? 'candidate_submit' : 'quality_submit_review';
await call('session.submit_terminal', {client_command_id:'terminal', expected_revision:admission.session_revision, terminal_operation:terminal, terminal_payload_b64:b64('{"outcome":"complete"}'), transcript_artifact_id:transcript.artifact_id, input_tokens:1, output_tokens:1, cache_read_tokens:0, cache_write_tokens:0, reasoning_tokens:null, reported_cost_micro_usd:10, stop_reason:'completed'});
"#;

fn model() -> ModelProfileV1 {
    ModelProfileV1 {
        provider: "fake".to_owned(),
        model_id: "provider-free".to_owned(),
        thinking_level: ThinkingLevelV1::None,
        context_token_limit: 100,
        output_token_limit: 100,
        price_input_micro_usd_per_million_tokens: MicroUsd::new(1),
        price_output_micro_usd_per_million_tokens: MicroUsd::new(1),
        price_cache_read_micro_usd_per_million_tokens: MicroUsd::new(1),
        price_cache_write_micro_usd_per_million_tokens: MicroUsd::new(1),
        capability_flags: Vec::new(),
    }
}

fn runtime_identity() -> RuntimeIdentityV1 {
    RuntimeIdentityV1 {
        deno_executable: AbsoluteHostPath::parse(deno().to_string_lossy().into_owned()).unwrap(),
        deno_version: "2.9.4".to_owned(),
        source_graph_digest: digest(20),
        resolved_dependency_graph_digest: digest(21),
        deno_json_digest: digest(22),
        deno_lock_digest: digest(23),
        pi_version: "provider-free-fake".to_owned(),
        credential: CredentialDescriptorV1::Environment {
            name: "FACTORY_FAKE_PROVIDER_KEY".to_owned(),
        },
    }
}

fn wire_packet(
    packet: &AssignmentPacketV1,
    system: &[u8],
    assignment: &[u8],
    tools: Vec<String>,
) -> AssignmentPacketWireV1 {
    AssignmentPacketWireV1 {
        format_version: ASSIGNMENT_PACKET_V1_FORMAT,
        campaign_id: packet.campaign_id.get(),
        assignment_id: packet.assignment_id.get(),
        application_revision_id: packet.application_revision_id.get(),
        kernel_build_id: packet.kernel_build_id.digest().to_hex(),
        office: match packet.office {
            Office::ProductResearch => "product_research",
            Office::Engineering => "engineering",
            Office::Quality => "quality",
        }
        .to_owned(),
        target: packet.target.clone(),
        repository_base_identity: digest(30).to_hex(),
        factory_base_identity: digest(31).to_hex(),
        ticket_attempt_id: packet.ticket_attempt_id.map(|id| id.get()),
        candidate_id: packet.candidate_id.map(|id| id.get()),
        system_prompt_artifact_id: packet.system_prompt_artifact_id.get(),
        assignment_prompt_artifact_id: packet.assignment_prompt_artifact_id.get(),
        required_read_manifest_artifact_id: packet.required_read_manifest_artifact_id.get(),
        system_prompt_digest: ContentDigest::of_bytes(system).to_hex(),
        assignment_prompt_digest: ContentDigest::of_bytes(assignment).to_hex(),
        system_prompt_bytes_b64: base64(system),
        assignment_prompt_bytes_b64: base64(assignment),
        workspace_root: packet.workspace_root.as_str().to_owned(),
        staging_root: packet.staging_root.as_str().to_owned(),
        model: AssignmentModelWireV1 {
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
        limits: AssignmentLimitsWireV1 {
            turn_limit: packet.limits.turn_limit,
            wall_limit_millis: packet.limits.wall_limit.get(),
            output_byte_limit: packet.limits.output_byte_limit,
        },
        runtime: AssignmentRuntimeWireV1 {
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
            credential_source: AssignmentCredentialWireV1 {
                kind: "environment".to_owned(),
                name: Some("FACTORY_FAKE_PROVIDER_KEY".to_owned()),
                path: None,
            },
        },
        required_reads: packet
            .required_reads
            .iter()
            .map(|read| AssignmentReadWireV1 {
                path: read.path.as_str().to_owned(),
                digest: read.digest.to_hex(),
                reason: read.reason.clone(),
            })
            .collect(),
        tools,
        terminal_operations: packet
            .terminal_operations
            .iter()
            .map(|operation| {
                match operation {
                    TerminalOperationV1::WorkComplete => "work_complete",
                    TerminalOperationV1::CandidateSubmit => "candidate_submit",
                    TerminalOperationV1::QualitySubmitReview => "quality_submit_review",
                }
                .to_owned()
            })
            .collect(),
        remaining_campaign_allowance_micro_usd: packet.remaining_campaign_allowance.get(),
        aggregate_revision: packet.revision.get(),
        packet_digest: String::new(),
    }
}

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
fn deterministic_command(
    wire: CommandWireV1,
    stdout: &[u8],
    stderr: &[u8],
) -> DeterministicCommand {
    let profile = factory_protocol::parse_command_profile_v1(
        canonical_command_profile_json_v1(&wire).unwrap().as_bytes(),
    )
    .unwrap();
    DeterministicCommand::new(
        profile,
        CommandStdin::Empty,
        CommandExpectation::new(
            ComparisonRevision::parse("full-vertical").unwrap(),
            Some(
                ExactBytes::from_artifact(ContentDigest::of_bytes(stdout), stdout.to_vec())
                    .unwrap(),
            ),
            Some(
                ExactBytes::from_artifact(ContentDigest::of_bytes(stderr), stderr.to_vec())
                    .unwrap(),
            ),
        ),
    )
    .unwrap()
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
            provider: "fake".to_owned(),
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
                vec!["workspace_read", "artifact_seal", "product_submit_ticket"],
            ),
            office(
                "engineering",
                3,
                4,
                vec![
                    "workspace_read",
                    "artifact_seal",
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
            focused: Vec::new(),
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
) -> (
    CasStore,
    factory_kernel::storage::KernelBuildReceipt,
    factory_protocol::KernelBuildId,
) {
    let cas = CasStore::new_with_seed(
        std::env::temp_dir().join(unique("cas")),
        4 * 1024 * 1024,
        unique_number(),
    )
    .unwrap();
    let staging = cas.runtime_root().join("build");
    fs::create_dir_all(&staging).unwrap();
    fs::write(
        staging.join("qualification"),
        b"provider-free qualification\n",
    )
    .unwrap();
    let qualification = cas.adopt(&staging, Path::new("qualification")).unwrap();
    let status = store.kernel_build_status().await.unwrap();
    let id = factory_protocol::KernelBuildId::new(digest(unique_number()));
    let receipt = store
        .install_kernel_build(
            &cas,
            &InstallKernelBuild {
                principal: "operator".to_owned(),
                command_id: unique("install-build"),
                expected_revision: ExpectedRevision::new(status.aggregate_revision),
                build_id: id,
                source_digest: digest(unique_number()),
                binary_digest: digest(unique_number()),
                schema_identity: SCHEMA_IDENTITY.to_owned(),
                deno_executable_path: deno().to_string_lossy().into_owned(),
                deno_version: "2.9.4".to_owned(),
                deno_lock_digest: digest(unique_number()),
                qualification_receipt: qualification,
            },
        )
        .await
        .unwrap();
    (cas, receipt, id)
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
fn command_runner() -> CommandRunner {
    CommandRunner::new(
        ApprovedToolExecutables::new(
            ExactExecutable::discover(
                std::env::var_os("CARGO")
                    .map(PathBuf::from)
                    .unwrap_or_else(|| PathBuf::from("/opt/homebrew/opt/rustup/bin/cargo")),
            )
            .unwrap(),
            ExactExecutable::discover(system_git()).unwrap(),
            ExactExecutable::discover(deno()).unwrap(),
        ),
        Duration::from_millis(100),
    )
    .unwrap()
}
fn canonical_manifest(reads: &[ReadObservationV1]) -> Vec<u8> {
    let mut bytes = b"factory-read-manifest-v1\0".to_vec();
    bytes.extend_from_slice(&(reads.len() as u32).to_be_bytes());
    for read in reads {
        bytes.extend_from_slice(&(read.path.as_str().len() as u32).to_be_bytes());
        bytes.extend_from_slice(read.path.as_str().as_bytes());
        bytes.extend_from_slice(&read.digest.as_bytes());
        let reason = b"application contract";
        bytes.extend_from_slice(&(reason.len() as u32).to_be_bytes());
        bytes.extend_from_slice(reason);
    }
    bytes
}
fn ticket_revision(
    ticket: &factory_kernel::ticket_store::LiveTicketProposalArtifact,
) -> AggregateRevision {
    match ticket.state {
        factory_protocol::TicketState::Proposed => AggregateRevision::initial(),
        _ => panic!("product ticket must be proposed"),
    }
}

fn campaign_remaining(campaign: &factory_kernel::process::CampaignStatus) -> MicroUsd {
    let measured = match campaign.measured_cost {
        factory_protocol::TerminalCostV1::Known(value) => value,
        factory_protocol::TerminalCostV1::Unknown
        | factory_protocol::TerminalCostV1::Exceeded(_) => {
            panic!("new assignment requires a known campaign cost")
        }
    };
    MicroUsd::new(
        campaign
            .aggregate_budget
            .get()
            .checked_sub(measured.get())
            .expect("campaign must not exceed its own budget before assignment"),
    )
}

fn review_id_from_stdout(stdout: &str) -> factory_protocol::ReviewId {
    stdout
        .lines()
        .find_map(|line| line.strip_prefix("FULL_VERTICAL_REVIEW_ID="))
        .and_then(|value| value.parse::<i64>().ok())
        .and_then(|value| factory_protocol::ReviewId::new(value).ok())
        .expect("Quality actor must report its durable review identity")
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
fn base64(bytes: &[u8]) -> String {
    const A: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::with_capacity(bytes.len().div_ceil(3) * 4);
    for chunk in bytes.chunks(3) {
        let a = chunk[0];
        let b = chunk.get(1).copied().unwrap_or(0);
        let c = chunk.get(2).copied().unwrap_or(0);
        out.push(A[(a >> 2) as usize] as char);
        out.push(A[(((a & 3) << 4) | (b >> 4)) as usize] as char);
        out.push(if chunk.len() > 1 {
            A[(((b & 15) << 2) | (c >> 6)) as usize] as char
        } else {
            '='
        });
        out.push(if chunk.len() > 2 {
            A[(c & 63) as usize] as char
        } else {
            '='
        });
    }
    out
}
fn unique(prefix: &str) -> String {
    format!("{prefix}-{}", unique_number())
}
fn unique_number() -> u64 {
    (u64::from(std::process::id()) << 32) | NEXT.fetch_add(1, Ordering::Relaxed)
}
fn digest(value: u64) -> ContentDigest {
    let mut bytes = [0; 32];
    for chunk in bytes.chunks_exact_mut(8) {
        chunk.copy_from_slice(&value.to_be_bytes())
    }
    ContentDigest::from_bytes(bytes)
}
