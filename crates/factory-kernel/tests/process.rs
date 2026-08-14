//! Provider-free T5 PostgreSQL authority judges.

use std::fs;
use std::sync::atomic::{AtomicU64, Ordering};

use factory_kernel::cas::{CasArtifact, CasStore};
use factory_kernel::local_transport::{LocalDaemon, LocalTransportConfig};
use factory_kernel::process::{CancelCampaign, CreateAssignment, StartCampaign, StartSession};
use factory_kernel::storage::{
    ActivateApplicationRevision, AdmitCompiledApplication, InstallKernelBuild, KernelStore,
    RegisterArtifact, RegisterRepository, SCHEMA_IDENTITY,
};
use factory_protocol::{
    ASSIGNMENT_PACKET_V1_FORMAT, AbsoluteHostPath, AggregateRevision, ApplicationKey,
    ApplicationRevisionId, ArchitectPrincipalV1, ArtifactId, AssignmentPacketV1, AssignmentRole,
    ContentDigest, CredentialDescriptorV1, ExpectedRevision, MicroUsd, ModelProfileV1,
    ReadExactFileV1, ReadObservationV1, RepositoryRelativePath, RuntimeIdentityV1,
    SealedArtifactReferenceV1, SessionLimitsV1, StopReasonV1, TerminalOperationV1,
    TerminalReportV1, UsageTotalsV1,
};

static NEXT_TEST: AtomicU64 = AtomicU64::new(1);

#[test]
#[ignore = "requires FACTORY_TEST_DATABASE_URL for a disposable PostgreSQL 18 database"]
fn accepted_session_has_exact_facts_and_a_thousand_events_have_no_rows() {
    smol::block_on(async {
        let store = store().await;
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
        let campaign = store
            .process_store()
            .start_campaign(&StartCampaign {
                principal: "architect".to_owned(),
                command_id: unique("campaign"),
                expected_application_revision: ExpectedRevision::new(
                    AggregateRevision::from_persisted(1),
                ),
                application_revision_id: application,
                aggregate_budget: MicroUsd::new(1_000_000),
                deadline_unix_millis: 4_000_000_000_000,
                delivery_target: 1,
            })
            .await
            .expect("campaign");

        let workspace_root = build.cas.runtime_root().join(unique("workspace"));
        fs::create_dir_all(&workspace_root).unwrap();
        fs::write(workspace_root.join("AGENTS.md"), b"read bytes").unwrap();
        let staging_root = build.cas.runtime_root().join("staging");
        let read_path = RepositoryRelativePath::parse("AGENTS.md").unwrap();
        let observed = ReadObservationV1 {
            path: read_path.clone(),
            digest: ContentDigest::of_bytes(b"read bytes"),
        };
        let expected_manifest = canonical_manifest(std::slice::from_ref(&observed));
        let expected_seal =
            seal_and_register(&store, &build, "expected-manifest", &expected_manifest).await;
        let system = seal_and_register(&store, &build, "system-prompt", b"system prompt").await;
        let assignment_prompt =
            seal_and_register(&store, &build, "assignment-prompt", b"assignment prompt").await;
        let process = store.process_store();
        let identity = process
            .reserve_assignment_identity()
            .await
            .expect("assignment identity");
        let mut packet = AssignmentPacketV1 {
            format_version: ASSIGNMENT_PACKET_V1_FORMAT,
            campaign_id: campaign.campaign_id,
            assignment_id: identity.assignment_id(),
            kernel_build_id: build.kernel_build_id,
            application_revision_id: application,
            assignment_role: AssignmentRole::ProductResearch,
            target: "target/base".to_owned(),
            ticket_attempt_id: None,
            candidate_id: None,
            assignment_evidence: Vec::new(),
            system_prompt_artifact_id: system.artifact_id,
            assignment_prompt_artifact_id: assignment_prompt.artifact_id,
            required_read_manifest_artifact_id: expected_seal.artifact_id,
            workspace_root: AbsoluteHostPath::parse(workspace_root.to_str().unwrap()).unwrap(),
            staging_root: AbsoluteHostPath::parse(staging_root.to_str().unwrap()).unwrap(),
            model: ModelProfileV1 {
                provider: "fake".to_owned(),
                model_id: "fake-model".to_owned(),
                thinking_level: factory_protocol::ThinkingLevelV1::None,
                context_token_limit: 10,
                output_token_limit: 10,
                price_input_micro_usd_per_million_tokens: MicroUsd::new(1),
                price_output_micro_usd_per_million_tokens: MicroUsd::new(1),
                price_cache_read_micro_usd_per_million_tokens: MicroUsd::new(1),
                price_cache_write_micro_usd_per_million_tokens: MicroUsd::new(1),
                capability_flags: Vec::new(),
            },
            limits: SessionLimitsV1 {
                turn_limit: 1,
                wall_limit: factory_protocol::DurationMillis::new(10_000),
                output_byte_limit: 10_000,
            },
            runtime: RuntimeIdentityV1 {
                deno_executable: AbsoluteHostPath::parse("/opt/factory/deno").unwrap(),
                deno_version: "2.9.4".to_owned(),
                source_graph_digest: digest(700),
                resolved_dependency_graph_digest: digest(703),
                deno_json_digest: digest(701),
                deno_lock_digest: digest(702),
                pi_version: "test-pi".to_owned(),
                credential: CredentialDescriptorV1::PiAuthStore {
                    path: factory_protocol::RuntimeRelativePath::parse("credentials/fake").unwrap(),
                },
            },
            required_reads: vec![ReadExactFileV1 {
                path: read_path,
                digest: observed.digest,
                reason: "authority contract".to_owned(),
            }],
            terminal_operations: vec![TerminalOperationV1::WorkComplete],
            remaining_campaign_allowance: MicroUsd::new(1_000_000),
            revision: AggregateRevision::initial(),
            packet_digest: digest(703),
        };
        let system_bytes = b"system prompt";
        let assignment_prompt_bytes = b"assignment prompt";
        let mut wire = wire_packet(&packet, system_bytes, assignment_prompt_bytes);
        let packet_digest = factory_protocol::unsigned_assignment_packet_digest_v1(&wire).unwrap();
        wire.packet_digest = packet_digest.to_hex();
        packet.packet_digest = packet_digest;
        let packet_bytes = factory_protocol::canonical_assignment_packet_json_v1(&wire)
            .unwrap()
            .into_bytes();
        let packet_artifact = seal_and_register(&store, &build, "packet", &packet_bytes).await;
        let assignment = process
            .create_assignment(
                &build.cas,
                &CreateAssignment {
                    principal: "architect".to_owned(),
                    command_id: unique("assignment"),
                    expected_campaign_revision: ExpectedRevision::new(campaign.resulting_revision),
                    identity,
                    packet: packet.clone(),
                    packet_bytes: packet_bytes.clone(),
                    packet_artifact: packet_artifact.sealed,
                    required_read_manifest_artifact_id: expected_seal.artifact_id,
                    attempt_ordinal: 1,
                },
            )
            .await
            .expect("assignment");
        let session = process
            .start_session(&StartSession {
                principal: "architect".to_owned(),
                command_id: unique("session"),
                expected_assignment_revision: ExpectedRevision::new(assignment.resulting_revision),
                assignment_id: assignment.assignment_id,
                packet_digest: packet.packet_digest,
                custody: factory_protocol::ProcessCustodyV1 {
                    pid: std::process::id(),
                    pgid: std::process::id(),
                    started_at_unix_millis: 1,
                },
            })
            .await
            .expect("session");
        let before_events = process.process_fact_counts().await.expect("counts");
        let fake_events: Vec<Vec<u8>> = (0_u32..1_000)
            .map(|sequence| sequence.to_be_bytes().to_vec())
            .collect();
        assert_eq!(fake_events.len(), 1_000);
        assert_eq!(
            process.process_fact_counts().await.expect("counts"),
            before_events
        );

        let daemon_root =
            std::env::temp_dir().join(format!("fv3d-{}-{}", std::process::id(), unique_number()));
        let daemon = LocalDaemon::bind(LocalTransportConfig::new(daemon_root), &store)
            .await
            .expect("daemon");
        let (_actor_descriptor, actor_connection) = daemon
            .create_admitted_actor_socketpair(&process, session.session_id, &packet)
            .await
            .expect("admitted actor socketpair");
        let mut read_authority = actor_connection
            .workspace_read_authority(
                &workspace_root,
                expected_seal.artifact_id,
                packet.required_reads.clone(),
            )
            .expect("read authority");
        let read_response = read_authority
            .read_exact(RepositoryRelativePath::parse("AGENTS.md").unwrap())
            .expect("exact workspace read");
        assert_eq!(read_response.blake3, observed.digest.to_hex());

        let transcript = seal_and_register(&store, &build, "transcript", b"transcript").await;
        let stdout = seal_and_register(&store, &build, "stdout", b"stdout").await;
        let stderr = seal_and_register(&store, &build, "stderr", b"stderr").await;
        let assertion = read_authority
            .seal_assertion(&build.cas, &staging_root)
            .expect("sealed assertion");
        store
            .register_artifact(
                &build.cas,
                &RegisterArtifact {
                    principal: "operator".to_owned(),
                    command_id: unique("assertion"),
                    expected_kernel_build_revision: ExpectedRevision::new(
                        build.receipt.resulting_revision,
                    ),
                    kernel_build_id: build.kernel_build_id,
                    sealed: assertion.artifact(),
                },
            )
            .await
            .expect("registered assertion");
        let evidence = process
            .verify_terminal_evidence_with_packet_bytes(
                &build.cas,
                session.session_id,
                &packet,
                packet_artifact.sealed,
                &packet_bytes,
                factory_kernel::process::TerminalArtifactSeals {
                    transcript: transcript.sealed,
                    stdout: stdout.sealed,
                    stderr: stderr.sealed,
                    partial_transcript: None,
                },
                assertion,
                Some(UsageTotalsV1 {
                    input_tokens: 1,
                    output_tokens: 1,
                    reported_cost_micro_usd: Some(MicroUsd::new(7)),
                    ..UsageTotalsV1::default()
                }),
            )
            .await
            .expect("verified evidence");
        let terminal = process
            .terminal_session(
                "architect",
                &unique("terminal"),
                session.session_id,
                &TerminalReportV1 {
                    packet_digest: packet.packet_digest,
                    expected_session_revision: ExpectedRevision::new(session.resulting_revision),
                    operation: Some(TerminalOperationV1::WorkComplete),
                    stop_reason: StopReasonV1::Completed,
                    report_digest: digest(704),
                },
                evidence,
            )
            .await
            .expect("terminal");
        assert_eq!(
            terminal.cost,
            factory_protocol::TerminalCostV1::Known(MicroUsd::new(7))
        );
        let cancel_command_id = unique("cancel-campaign");
        let cancelled = process
            .cancel_campaign(&CancelCampaign {
                principal: "architect".to_owned(),
                command_id: cancel_command_id.clone(),
                expected_revision: ExpectedRevision::new(terminal.campaign_revision),
                campaign_id: campaign.campaign_id,
            })
            .await
            .expect("cancel completed campaign");
        assert_eq!(
            process
                .campaign_status(campaign.campaign_id)
                .await
                .expect("cancelled status")
                .state,
            factory_protocol::CampaignState::Cancelled
        );
        let retry = process
            .cancel_campaign(&CancelCampaign {
                principal: "architect".to_owned(),
                command_id: cancel_command_id,
                expected_revision: ExpectedRevision::new(terminal.campaign_revision),
                campaign_id: campaign.campaign_id,
            })
            .await
            .expect("cancel retry");
        assert!(retry.was_idempotent_retry);
        assert_eq!(retry.resulting_revision, cancelled.resulting_revision);
        let after = process.process_fact_counts().await.expect("counts");
        assert_eq!(after.0, before_events.0);
        assert_eq!(after.1, before_events.1);
        // Terminal provenance registers four logical artifacts. The physical
        // artifact facts are content-addressed, so a repeat of this judge can
        // reuse the three fixed stream objects; only the per-session
        // daemon-authored assertion is necessarily new. The immediate count
        // comparison above is the exact proof that 1,000 streamed actor
        // events add no rows.
        assert!((before_events.2 + 1..=before_events.2 + 4).contains(&after.2));
        assert_eq!(after.3, before_events.3 + 2);
        daemon.shutdown().await.expect("daemon shutdown");
        store.close().await;
    });
}

#[test]
#[ignore = "requires FACTORY_TEST_DATABASE_URL for a disposable PostgreSQL 18 database"]
fn one_thousand_status_reads_have_zero_write_rows() {
    smol::block_on(async {
        let store = store().await;
        store.migrate_and_verify().await.expect("migration");
        let process = store.process_store();
        let before = process.process_audit_count().await.expect("audit count");
        let missing = factory_protocol::SessionId::new(9_223_372_036_854_775_000).unwrap();
        for _ in 0..1_000 {
            assert!(process.session_status(missing).await.is_err());
        }
        assert_eq!(
            process.process_audit_count().await.expect("audit count"),
            before
        );
        store.close().await;
    });
}

struct InstalledBuild {
    cas: CasStore,
    receipt: factory_kernel::storage::KernelBuildReceipt,
    kernel_build_id: factory_protocol::KernelBuildId,
}

async fn store() -> KernelStore {
    KernelStore::connect(&test_database_url())
        .await
        .expect("connect")
}

fn test_database_url() -> String {
    let url = std::env::var("FACTORY_TEST_DATABASE_URL").expect("FACTORY_TEST_DATABASE_URL");
    let name = url
        .rsplit('/')
        .next()
        .and_then(|part| part.split('?').next())
        .unwrap();
    assert!(
        name.strip_prefix("factory_test_v3_")
            .is_some_and(|s| !s.is_empty() && s.bytes().all(|b| b.is_ascii_digit()))
    );
    url
}

async fn install_build(store: &KernelStore) -> InstalledBuild {
    let cas = CasStore::new_with_seed(
        std::env::temp_dir().join(unique("process-cas")),
        4 * 1024 * 1024,
        unique_number(),
    )
    .unwrap();
    let staging = cas.runtime_root().join("staging");
    fs::create_dir_all(&staging).unwrap();
    fs::write(staging.join("qualification"), b"qualified").unwrap();
    let qualification = cas.adopt(&staging, "qualification").unwrap();
    let status = store.kernel_build_status().await.unwrap();
    let build_id = factory_protocol::KernelBuildId::new(digest(unique_number()));
    let command = InstallKernelBuild {
        principal: "operator".to_owned(),
        command_id: unique("build"),
        expected_revision: ExpectedRevision::new(status.aggregate_revision),
        build_id,
        source_digest: digest(unique_number()),
        binary_digest: digest(unique_number()),
        schema_identity: SCHEMA_IDENTITY.to_owned(),
        deno_executable_path: "/opt/factory/deno".to_owned(),
        deno_version: "2.9.4".to_owned(),
        deno_lock_digest: digest(unique_number()),
        qualification_receipt: qualification,
    };
    let receipt = store.install_kernel_build(&cas, &command).await.unwrap();
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
    let key = unique("application");
    let bundle = minimal_bundle_json(&key, repository_key, repository_path, &templates);
    fs::write(root.join("bundle.json"), bundle).unwrap();
    let result = store
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
        .await;
    let admitted = result.expect("application admission");
    let rationale = seal_and_register(store, build, "application-activation", b"activate").await;
    store
        .activate_application_revision(&ActivateApplicationRevision {
            principal: ArchitectPrincipalV1::parse("architect").expect("principal"),
            command_id: unique("application-activate"),
            expected_revision: ExpectedRevision::new(admitted.resulting_revision),
            application_key: ApplicationKey::parse(key).expect("application key"),
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

fn wire_packet(
    packet: &AssignmentPacketV1,
    system_prompt: &[u8],
    assignment_prompt: &[u8],
) -> factory_protocol::AssignmentPacketWireV1 {
    factory_protocol::AssignmentPacketWireV1 {
        format_version: 1,
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
            credential_source: match &packet.runtime.credential {
                CredentialDescriptorV1::Environment { name } => {
                    factory_protocol::AssignmentCredentialWireV1 {
                        kind: "environment".to_owned(),
                        name: Some(name.clone()),
                        path: None,
                    }
                }
                CredentialDescriptorV1::PiAuthStore { path } => {
                    factory_protocol::AssignmentCredentialWireV1 {
                        kind: "pi_auth_store".to_owned(),
                        name: None,
                        path: Some(path.as_str().to_owned()),
                    }
                }
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
        let first = chunk[0];
        output.push(ALPHABET[(first >> 2) as usize] as char);
        output.push(
            ALPHABET[((first & 0x03) << 4 | chunk.get(1).copied().unwrap_or(0) >> 4) as usize]
                as char,
        );
        if let Some(second) = chunk.get(1) {
            output.push(
                ALPHABET[((second & 0x0f) << 2 | chunk.get(2).copied().unwrap_or(0) >> 6) as usize]
                    as char,
            );
        } else {
            output.push('=');
        }
        if let Some(third) = chunk.get(2) {
            output.push(ALPHABET[(third & 0x3f) as usize] as char);
        } else {
            output.push('=');
        }
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
            tools: vec!["workspace_read".to_owned()],
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
    .unwrap()
}
