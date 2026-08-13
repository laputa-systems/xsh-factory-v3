//! Provider-free PostgreSQL judge for the final candidate-to-delivery authority.
//!
//! This deliberately drives only public typed authority APIs.  It proves the
//! transaction boundary across the five final relations without depending on
//! a provider, a product checkout, a Git subprocess, or test-owned SQL.

use std::fs;
use std::sync::atomic::{AtomicU64, Ordering};

use factory_kernel::cas::{CasArtifact, CasStore};
use factory_kernel::decision_store::{
    AttachCandidateCommit, DecideCandidate, DecisionStoreError, RecordDelivery, RecordValidation,
    ReleaseTicketAttempt as ArchitectReleaseTicketAttempt, SponsorTicket, SubmitCandidate,
    SubmitQualityReview, ValidationResult, ValidationScope,
};
use factory_kernel::local_transport::{LocalDaemon, LocalTransportConfig};
use factory_kernel::process::{
    CancelCampaign, CreateAssignment, StartCampaign, StartSession, TerminalArtifactSeals,
};
use factory_kernel::storage::{
    ActivateApplicationRevision, AdmitCompiledApplication, InstallKernelBuild, KernelStore,
    RegisterArtifact, RegisterRepository, SCHEMA_IDENTITY,
};
use factory_kernel::ticket_store::{
    ClaimOutcome, ClaimSponsoredTicket, CurrentHeadRequalification, SubmitTicketProposal,
};
use factory_protocol::{
    ASSIGNMENT_PACKET_V1_FORMAT, AbsoluteHostPath, AggregateRevision, ApplicationBundleWireV1,
    ApplicationRevisionId, ArchitectDecisionKindV1, ArchitectPrincipalV1,
    AssignmentCredentialWireV1, AssignmentEvidenceRoleV1, AssignmentEvidenceV1,
    AssignmentEvidenceWireV1, AssignmentLimitsWireV1, AssignmentModelWireV1, AssignmentPacketV1,
    AssignmentPacketWireV1, AssignmentReadWireV1, AssignmentRuntimeWireV1,
    CandidateDecisionRequestV1, CandidateDecisionV1, CandidateSubmissionV1, CommandWireV1,
    CommitMessageWireV1, ContentDigest, CredentialDescriptorV1, DurationMillis, ExecutableWireV1,
    ExpectedRevision, GitWireV1, KernelBuildId, LimitsWireV1, MicroUsd, ModelProfileV1,
    ModelWireV1, Office, OfficeWireV1, ProcessCustodyV1, QualityReviewSubmissionV1,
    ReadExactFileV1, ReleaseDecisionV1, RepositoryObjectIdV1, RepositoryRelativePath,
    RepositoryWireV1, RuntimeIdentityV1, SealedArtifactReferenceV1, SessionLimitsV1,
    SponsorshipDecisionV1, StopReasonV1, TemplateWireV1, TerminalOperationV1, TerminalReportV1,
    ThinkingLevelV1, TicketBoundsWireV1, TicketPolicyWireV1, UsageTotalsV1, ValidationWireV1,
    canonical_application_bundle_json_v1, canonical_assignment_packet_json_v1,
    unsigned_assignment_packet_digest_v1,
};

static NEXT_TEST: AtomicU64 = AtomicU64::new(1);

#[test]
#[ignore = "requires FACTORY_TEST_DATABASE_URL for a disposable PostgreSQL 18 database"]
fn typed_candidate_validation_review_decision_delivery_vertical() {
    smol::block_on(async {
        let mut fixture = Fixture::new().await;
        fixture.run_vertical().await;
        fixture.store.close().await;
    });
}

#[test]
#[ignore = "requires FACTORY_TEST_DATABASE_URL for a disposable PostgreSQL 18 database"]
fn failed_attempt_requires_an_explicit_architect_release() {
    smol::block_on(async {
        let mut fixture = Fixture::new().await;
        let engineering = fixture.open_session(Office::Engineering).await;
        let candidate = fixture
            // A regression checkpoint intentionally captures the pristine
            // base tree before the implementation exists. Candidate storage
            // must accept that equality while still requiring the completed
            // candidate tree to differ.
            .submit_candidate(&engineering, 'b', 'd', "candidate-failure")
            .await;
        fixture.attempt_revision = fixture.attempt_revision.next().expect("attempt revision");
        let failed = fixture
            .store
            .decision_store()
            .record_validation(&RecordValidation {
                principal: "kernel-validation".to_owned(),
                command_id: unique("hard-failure"),
                candidate_id: candidate.candidate_id,
                expected_candidate_revision: ExpectedRevision::new(candidate.resulting_revision),
                expected_attempt_revision: ExpectedRevision::new(fixture.attempt_revision),
                scope: ValidationScope::HardCandidate,
                kernel_build_id: fixture.build.kernel_build_id,
                performed_by_session_id: engineering.session_id,
                validation_profile: "full".to_owned(),
                pristine_tree: object('d'),
                command_set: fixture.common.reference(),
                result: ValidationResult::Failed,
                duration_millis: 1,
                log: fixture.common.reference(),
            })
            .await
            .expect("failed deterministic validation");
        assert_eq!(failed.state, factory_protocol::ValidationState::Failed);
        assert_eq!(
            failed.candidate_state,
            factory_protocol::CandidateState::Rejected
        );
        fixture.attempt_revision = failed.resulting_attempt_revision;
        fixture.finish_session(engineering).await;
        assert!(matches!(
            fixture
                .decide(
                    candidate.candidate_id,
                    factory_protocol::ReviewId::new(1).unwrap(),
                    failed.resulting_candidate_revision,
                    CandidateDecisionV1::Deliver,
                    None,
                )
                .await,
            Err(DecisionStoreError::CandidateStateConflict { .. })
        ));

        let release_command = ArchitectReleaseTicketAttempt {
            command_id: unique("architect-release"),
            expected_attempt_revision: ExpectedRevision::new(fixture.attempt_revision),
            expected_ticket_revision: ExpectedRevision::new(fixture.ticket_revision),
            decision: ReleaseDecisionV1 {
                ticket_attempt_id: fixture.attempt_id,
                rationale: fixture.common.reference(),
                principal: architect(),
            },
            requalification: fixture.requalification(),
        };
        let released = fixture
            .store
            .decision_store()
            .release_ticket_attempt(&release_command)
            .await
            .expect("only an explicit Architect decision may release a failed attempt");
        assert_eq!(
            released.outcome,
            factory_kernel::decision_store::ReleaseOutcome::Released
        );
        assert_eq!(released.decision.kind, ArchitectDecisionKindV1::Release);
        assert!(
            fixture
                .store
                .decision_store()
                .release_ticket_attempt(&release_command)
                .await
                .expect("exact release retry")
                .was_idempotent_retry
        );
        fixture
            .store
            .process_store()
            .cancel_campaign(&CancelCampaign {
                principal: "operator".to_owned(),
                command_id: unique("cancel-release-campaign"),
                campaign_id: fixture.campaign_id,
                expected_revision: ExpectedRevision::new(fixture.campaign_revision),
            })
            .await
            .expect("close the provider-free release fixture");
        fixture.store.close().await;
    });
}

struct Fixture {
    store: KernelStore,
    build: InstalledBuild,
    application: ApplicationRevisionId,
    campaign_id: factory_protocol::CampaignId,
    campaign_revision: AggregateRevision,
    ticket_id: factory_protocol::TicketId,
    ticket_revision: AggregateRevision,
    attempt_id: factory_protocol::TicketAttemptId,
    attempt_revision: AggregateRevision,
    current_candidate: Option<factory_protocol::CandidateId>,
    provider_cost_spent: u64,
    assignment_ordinal: u32,
    common: RegisteredArtifact,
    observed: RegisteredArtifact,
    probes: RegisteredArtifact,
}

impl Fixture {
    async fn new() -> Self {
        let store = KernelStore::connect(&test_database_url())
            .await
            .expect("connect");
        store.migrate_and_verify().await.expect("migration");
        let build = install_build(&store).await;
        let repository_key = unique("repository-key");
        let repository_path = format!("/tmp/{}", unique("repository"));
        store
            .register_repository(&RegisterRepository {
                principal: "operator".to_owned(),
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
                principal: "operator".to_owned(),
                command_id: unique("campaign"),
                expected_application_revision: ExpectedRevision::new(
                    application.resulting_revision,
                ),
                application_revision_id: application.application_revision_id,
                aggregate_budget: MicroUsd::new(100),
                deadline_unix_millis: 4_000_000_000_000,
                delivery_target: 1,
            })
            .await
            .expect("campaign");

        // Artifacts retain their creating kernel build. The physical CAS may
        // reuse equal bytes, but this fixture intentionally gives each build
        // distinct evidence so the attempted workflow cannot cross-build
        // reference a prior fixture's provenance.
        let common_bytes = unique("sealed-evidence");
        let expected_bytes = unique("expected-result");
        let observed_bytes = unique("observed-failure");
        let probes_bytes = unique("quality-probes");
        let common = register(&store, &build, "common", common_bytes.as_bytes()).await;
        let expected = register(&store, &build, "expected", expected_bytes.as_bytes()).await;
        let observed = register(&store, &build, "observed", observed_bytes.as_bytes()).await;
        let probes = register(&store, &build, "probes", probes_bytes.as_bytes()).await;
        let ticket = store
            .ticket_store()
            .submit_ticket_proposal(&SubmitTicketProposal {
                principal: "product".to_owned(),
                command_id: unique("proposal"),
                expected_application_revision: ExpectedRevision::new(
                    application.resulting_revision,
                ),
                application_revision_id: application.application_revision_id,
                proposal_artifact_id: common.artifact_id,
                reproducer_artifact_id: common.artifact_id,
                expected_observation_artifact_id: expected.artifact_id,
                first_actual_observation_artifact_id: observed.artifact_id,
                second_actual_observation_artifact_id: observed.artifact_id,
                discovery_commit: object('a').as_str().to_owned(),
                discovery_tree: object('b').as_str().to_owned(),
            })
            .await
            .expect("reproducible proposal");
        let sponsored = store
            .decision_store()
            .sponsor_ticket(&SponsorTicket {
                command_id: unique("sponsor"),
                expected_ticket_revision: ExpectedRevision::new(ticket.resulting_revision),
                decision: SponsorshipDecisionV1 {
                    ticket_revision_id: ticket.ticket_revision_id,
                    rationale: common.reference(),
                    principal: architect(),
                },
            })
            .await
            .expect("architect sponsorship is one durable transaction");
        let claim = store
            .ticket_store()
            .claim_sponsored_ticket(&ClaimSponsoredTicket {
                principal: "scheduler".to_owned(),
                command_id: unique("claim"),
                campaign_id: campaign.campaign_id,
                expected_campaign_revision: ExpectedRevision::new(campaign.resulting_revision),
                ticket_revision_id: ticket.ticket_revision_id,
                expected_ticket_revision: ExpectedRevision::new(
                    sponsored.resulting_ticket_revision,
                ),
                requalification: requalification(observed.artifact_id),
            })
            .await
            .expect("claim sponsored ticket");
        let ClaimOutcome::Claimed { ticket_attempt_id } = claim.outcome else {
            panic!("a matching current-head requalification must claim the ticket");
        };
        Self {
            store,
            build,
            application: application.application_revision_id,
            campaign_id: campaign.campaign_id,
            campaign_revision: campaign.resulting_revision,
            ticket_id: ticket.ticket_id,
            ticket_revision: claim.resulting_ticket_revision,
            attempt_id: ticket_attempt_id,
            attempt_revision: AggregateRevision::initial(),
            current_candidate: None,
            provider_cost_spent: 0,
            assignment_ordinal: 0,
            common,
            observed,
            probes,
        }
    }

    async fn run_vertical(&mut self) {
        let first_engineering = self.open_session(Office::Engineering).await;
        let first = self
            .submit_candidate(&first_engineering, 'c', 'd', "candidate-d")
            .await;
        self.current_candidate = Some(first.candidate_id);
        let first_retry = self
            .store
            .decision_store()
            .submit_candidate(&self.candidate_command(&first_engineering, 'c', 'd', "candidate-d"))
            .await
            .expect("exact candidate retry");
        assert!(first_retry.was_idempotent_retry);
        assert_eq!(first_retry.candidate_id, first.candidate_id);
        self.attempt_revision = self.attempt_revision.next().expect("attempt revision");

        assert!(matches!(
            self.store
                .decision_store()
                .record_validation(&RecordValidation {
                    principal: "kernel-validation".to_owned(),
                    command_id: unique("stale-validation"),
                    candidate_id: first.candidate_id,
                    expected_candidate_revision: ExpectedRevision::new(
                        first.resulting_revision.next().unwrap(),
                    ),
                    expected_attempt_revision: ExpectedRevision::new(self.attempt_revision),
                    scope: ValidationScope::HardCandidate,
                    kernel_build_id: self.build.kernel_build_id,
                    performed_by_session_id: first_engineering.session_id,
                    validation_profile: "full".to_owned(),
                    pristine_tree: object('d'),
                    command_set: self.common.reference(),
                    result: ValidationResult::Passed,
                    duration_millis: 1,
                    log: self.common.reference(),
                })
                .await,
            Err(DecisionStoreError::RevisionConflict { .. })
        ));
        assert!(matches!(
            self.store
                .decision_store()
                .record_validation(&RecordValidation {
                    principal: "kernel-validation".to_owned(),
                    command_id: unique("changed-tree-validation"),
                    candidate_id: first.candidate_id,
                    expected_candidate_revision: ExpectedRevision::new(first.resulting_revision),
                    expected_attempt_revision: ExpectedRevision::new(self.attempt_revision),
                    scope: ValidationScope::HardCandidate,
                    kernel_build_id: self.build.kernel_build_id,
                    performed_by_session_id: first_engineering.session_id,
                    validation_profile: "full".to_owned(),
                    pristine_tree: object('c'),
                    command_set: self.common.reference(),
                    result: ValidationResult::Passed,
                    duration_millis: 1,
                    log: self.common.reference(),
                })
                .await,
            Err(DecisionStoreError::ValidationTreeChanged)
        ));

        let hard_first = self
            .record_validation(
                first.candidate_id,
                first.resulting_revision,
                ValidationScope::HardCandidate,
                first_engineering.session_id,
                'd',
            )
            .await;
        self.attempt_revision = hard_first.resulting_attempt_revision;
        let attached = self
            .attach(
                first.candidate_id,
                hard_first.resulting_candidate_revision,
                'e',
            )
            .await;
        self.finish_session(first_engineering).await;

        let first_quality = self.open_session(Office::Quality).await;
        let quality_first = self
            .record_validation(
                first.candidate_id,
                attached.resulting_revision,
                ValidationScope::QualityFullSuite,
                first_quality.session_id,
                'd',
            )
            .await;
        self.attempt_revision = quality_first.resulting_attempt_revision;
        // Simulate the daemon/actor dying after the full-suite transition but
        // before the terminal review. A fresh Quality session must be able to
        // submit prose against this exact durable receipt without rerunning
        // the suite or inheriting a trusted actor result.
        self.finish_session(first_quality).await;
        let continuation_quality = self.open_session(Office::Quality).await;
        let accepted_review = self
            .submit_review(
                first.candidate_id,
                quality_first.resulting_candidate_revision,
                continuation_quality.session_id,
                quality_first.validation_id,
                factory_protocol::ReviewVerdict::Accept,
                self.probes.reference(),
            )
            .await
            .expect("accepted Quality review");
        self.attempt_revision = accepted_review.resulting_attempt_revision;
        let reworked = self
            .decide(
                first.candidate_id,
                accepted_review.review_id,
                accepted_review.resulting_candidate_revision,
                CandidateDecisionV1::Rework,
                None,
            )
            .await
            .expect("the one permitted semantic rework");
        assert_eq!(
            reworked.candidate_state,
            factory_protocol::CandidateState::Rejected
        );
        self.attempt_revision = reworked.resulting_attempt_revision;
        self.finish_session(continuation_quality).await;

        let second_engineering = self.open_session(Office::Engineering).await;
        let second = self
            .submit_candidate(&second_engineering, 'f', 'c', "candidate-c")
            .await;
        self.current_candidate = Some(second.candidate_id);
        self.attempt_revision = self.attempt_revision.next().expect("attempt revision");
        let hard_second = self
            .record_validation(
                second.candidate_id,
                second.resulting_revision,
                ValidationScope::HardCandidate,
                second_engineering.session_id,
                'c',
            )
            .await;
        self.attempt_revision = hard_second.resulting_attempt_revision;
        let attached_second = self
            .attach(
                second.candidate_id,
                hard_second.resulting_candidate_revision,
                'd',
            )
            .await;
        self.finish_session(second_engineering).await;

        let second_quality = self.open_session(Office::Quality).await;
        let quality_second = self
            .record_validation(
                second.candidate_id,
                attached_second.resulting_revision,
                ValidationScope::QualityFullSuite,
                second_quality.session_id,
                'c',
            )
            .await;
        self.attempt_revision = quality_second.resulting_attempt_revision;

        // The retained extra-probe relation is checked exactly like every
        // other sealed review input. A forged digest must roll back before a
        // review or attempt revision can appear.
        let forged_probes = SealedArtifactReferenceV1 {
            artifact_id: self.probes.artifact_id,
            digest: ContentDigest::of_bytes(b"forged probe digest"),
            byte_length: self.probes.sealed.byte_length(),
        };
        assert!(matches!(
            self.submit_review(
                second.candidate_id,
                quality_second.resulting_candidate_revision,
                second_quality.session_id,
                quality_second.validation_id,
                factory_protocol::ReviewVerdict::Reject,
                forged_probes,
            )
            .await,
            Err(DecisionStoreError::ArtifactReferenceMismatch)
        ));
        let rejected_review = self
            .submit_review(
                second.candidate_id,
                quality_second.resulting_candidate_revision,
                second_quality.session_id,
                quality_second.validation_id,
                factory_protocol::ReviewVerdict::Reject,
                self.probes.reference(),
            )
            .await
            .expect("review retry after a rolled-back forged-probe request");
        self.attempt_revision = rejected_review.resulting_attempt_revision;

        assert!(matches!(
            self.decide(
                second.candidate_id,
                rejected_review.review_id,
                rejected_review.resulting_candidate_revision,
                CandidateDecisionV1::Rework,
                None,
            )
            .await,
            Err(DecisionStoreError::ReworkLimitReached)
        ));
        assert!(matches!(
            self.decide(
                second.candidate_id,
                rejected_review.review_id,
                rejected_review.resulting_candidate_revision,
                CandidateDecisionV1::Deliver,
                None,
            )
            .await,
            Err(DecisionStoreError::QualityRejectionOverrideRequired)
        ));
        let delivered_decision = self
            .decide(
                second.candidate_id,
                rejected_review.review_id,
                rejected_review.resulting_candidate_revision,
                CandidateDecisionV1::Deliver,
                Some(rejected_review.review_id),
            )
            .await
            .expect("only an exact rejected review may be overridden");
        self.attempt_revision = delivered_decision.resulting_attempt_revision;
        self.finish_session(second_quality).await;

        let delivery = RecordDelivery {
            principal: "kernel-git".to_owned(),
            command_id: unique("delivery"),
            candidate_id: second.candidate_id,
            expected_candidate_revision: ExpectedRevision::new(
                delivered_decision.resulting_candidate_revision,
            ),
            expected_attempt_revision: ExpectedRevision::new(self.attempt_revision),
            expected_ticket_revision: ExpectedRevision::new(self.ticket_revision),
            expected_campaign_revision: ExpectedRevision::new(self.campaign_revision),
            expected_old_commit: object('a'),
            resulting_commit: object('d'),
            resulting_tree: object('c'),
            receipt: self.common.reference(),
        };
        let receipt = self
            .store
            .decision_store()
            .record_delivery(&delivery)
            .await
            .expect("accepted candidate is deliverable only after all terminal evidence");
        assert!(receipt.campaign_completed);
        assert_eq!(
            receipt.resulting_ticket_revision,
            self.ticket_revision.next().unwrap()
        );
        let retry = self
            .store
            .decision_store()
            .record_delivery(&delivery)
            .await
            .expect("delivery idempotency retry");
        assert!(retry.was_idempotent_retry);
        assert_eq!(retry.delivery_id, receipt.delivery_id);
    }

    fn requalification(&self) -> CurrentHeadRequalification {
        requalification(self.observed.artifact_id)
    }

    async fn submit_candidate(
        &self,
        session: &LiveSession,
        regression: char,
        candidate: char,
        command_id: &str,
    ) -> factory_kernel::decision_store::CandidateReceipt {
        self.store
            .decision_store()
            .submit_candidate(&self.candidate_command(session, regression, candidate, command_id))
            .await
            .expect("candidate submission")
    }

    fn candidate_command(
        &self,
        session: &LiveSession,
        regression: char,
        candidate: char,
        command_id: &str,
    ) -> SubmitCandidate {
        SubmitCandidate {
            principal: "engineering".to_owned(),
            command_id: command_id.to_owned(),
            ticket_attempt_id: self.attempt_id,
            expected_attempt_revision: ExpectedRevision::new(self.attempt_revision),
            expected_ticket_revision: ExpectedRevision::new(self.ticket_revision),
            engineering_session_id: session.session_id,
            base_commit: object('a'),
            base_tree: object('b'),
            regression_tree: object(regression),
            candidate_tree: object(candidate),
            changed_paths: self.common.reference(),
            regression_patch: self.common.reference(),
            regression_command_set: self.common.reference(),
            regression_log: self.common.reference(),
            candidate_patch: self.common.reference(),
            engineering_report: self.common.reference(),
            engineering_risks: self.common.reference(),
            submission: CandidateSubmissionV1 {
                commit_subject: "Fix observable behavior".to_owned(),
                commit_body: String::new(),
                regression_test_identity: "cargo test visible_regression".to_owned(),
            },
        }
    }

    async fn record_validation(
        &self,
        candidate_id: factory_protocol::CandidateId,
        candidate_revision: AggregateRevision,
        scope: ValidationScope,
        session_id: factory_protocol::SessionId,
        tree: char,
    ) -> factory_kernel::decision_store::ValidationReceipt {
        self.store
            .decision_store()
            .record_validation(&RecordValidation {
                principal: "kernel-validation".to_owned(),
                command_id: unique("validation"),
                candidate_id,
                expected_candidate_revision: ExpectedRevision::new(candidate_revision),
                expected_attempt_revision: ExpectedRevision::new(self.attempt_revision),
                scope,
                kernel_build_id: self.build.kernel_build_id,
                performed_by_session_id: session_id,
                validation_profile: "full".to_owned(),
                pristine_tree: object(tree),
                command_set: self.common.reference(),
                result: ValidationResult::Passed,
                duration_millis: 1,
                log: self.common.reference(),
            })
            .await
            .expect("passed exact-tree validation")
    }

    async fn attach(
        &self,
        candidate_id: factory_protocol::CandidateId,
        candidate_revision: AggregateRevision,
        commit: char,
    ) -> factory_kernel::decision_store::CandidateReceipt {
        self.store
            .decision_store()
            .attach_candidate_commit(&AttachCandidateCommit {
                principal: "kernel-git".to_owned(),
                command_id: unique("attach-commit"),
                candidate_id,
                expected_candidate_revision: ExpectedRevision::new(candidate_revision),
                candidate_commit: object(commit),
                candidate_ref: format!(
                    "refs/heads/factory/{}/{}",
                    self.ticket_id.get(),
                    candidate_id.get()
                ),
            })
            .await
            .expect("commit after durable hard validation")
    }

    async fn submit_review(
        &self,
        candidate_id: factory_protocol::CandidateId,
        candidate_revision: AggregateRevision,
        quality_session_id: factory_protocol::SessionId,
        validation_id: factory_protocol::ValidationId,
        verdict: factory_protocol::ReviewVerdict,
        additional_probes: SealedArtifactReferenceV1,
    ) -> Result<factory_kernel::decision_store::ReviewReceipt, DecisionStoreError> {
        self.store
            .decision_store()
            .submit_quality_review(&SubmitQualityReview {
                principal: "quality".to_owned(),
                command_id: unique("review"),
                candidate_id,
                expected_candidate_revision: ExpectedRevision::new(candidate_revision),
                expected_attempt_revision: ExpectedRevision::new(self.attempt_revision),
                quality_session_id,
                submission: QualityReviewSubmissionV1 {
                    full_suite_validation_id: validation_id,
                    verdict,
                    rationale: self.common.reference(),
                    risks: self.common.reference(),
                    additional_probes,
                },
            })
            .await
    }

    async fn decide(
        &self,
        candidate_id: factory_protocol::CandidateId,
        review_id: factory_protocol::ReviewId,
        candidate_revision: AggregateRevision,
        decision: CandidateDecisionV1,
        quality_rejection_override: Option<factory_protocol::ReviewId>,
    ) -> Result<factory_kernel::decision_store::CandidateDecisionReceipt, DecisionStoreError> {
        self.store
            .decision_store()
            .decide_candidate(&DecideCandidate {
                command_id: unique("decision"),
                expected_candidate_revision: ExpectedRevision::new(candidate_revision),
                expected_attempt_revision: ExpectedRevision::new(self.attempt_revision),
                expected_ticket_revision: ExpectedRevision::new(self.ticket_revision),
                request: CandidateDecisionRequestV1 {
                    candidate_id,
                    review_id,
                    decision,
                    rationale: self.common.reference(),
                    quality_rejection_override,
                    principal: architect(),
                },
            })
            .await
    }

    async fn open_session(&mut self, office: Office) -> LiveSession {
        let process = self.store.process_store();
        self.assignment_ordinal = self
            .assignment_ordinal
            .checked_add(1)
            .expect("fixture assignment ordinal");
        let workspace = self.build.cas.runtime_root().join(unique("workspace"));
        let staging = self.build.cas.runtime_root().join("staging");
        fs::create_dir_all(&workspace).expect("workspace");
        fs::write(workspace.join("AGENTS.md"), b"exact required read").expect("required read");
        let read = ReadExactFileV1 {
            path: RepositoryRelativePath::parse("AGENTS.md").unwrap(),
            digest: ContentDigest::of_bytes(b"exact required read"),
            reason: "authority contract".to_owned(),
        };
        let manifest = register(
            &self.store,
            &self.build,
            "manifest",
            &canonical_manifest(std::slice::from_ref(&read)),
        )
        .await;
        let system = register(&self.store, &self.build, "system", b"system").await;
        let assignment_prompt =
            register(&self.store, &self.build, "assignment", b"assignment").await;
        let identity = process
            .reserve_assignment_identity()
            .await
            .expect("assignment identity");
        let mut packet = AssignmentPacketV1 {
            format_version: ASSIGNMENT_PACKET_V1_FORMAT,
            campaign_id: self.campaign_id,
            assignment_id: identity.assignment_id(),
            kernel_build_id: self.build.kernel_build_id,
            application_revision_id: self.application,
            office,
            target: "one exact candidate".to_owned(),
            ticket_attempt_id: Some(self.attempt_id),
            // Quality sessions are created only after the candidate they
            // review, and this fixture records that candidate below.
            candidate_id: if office == Office::Quality {
                self.current_candidate
            } else {
                None
            },
            assignment_evidence: if office == Office::ProductResearch {
                Vec::new()
            } else {
                vec![AssignmentEvidenceV1 {
                    role: AssignmentEvidenceRoleV1::TicketProposal,
                    artifact_id: system.artifact_id,
                    digest: system.sealed.digest(),
                    byte_length: system.sealed.byte_length(),
                }]
            },
            system_prompt_artifact_id: system.artifact_id,
            assignment_prompt_artifact_id: assignment_prompt.artifact_id,
            required_read_manifest_artifact_id: manifest.artifact_id,
            workspace_root: AbsoluteHostPath::parse(workspace.to_str().unwrap()).unwrap(),
            staging_root: AbsoluteHostPath::parse(staging.to_str().unwrap()).unwrap(),
            model: model(),
            limits: SessionLimitsV1 {
                turn_limit: 1,
                wall_limit: DurationMillis::new(10_000),
                output_byte_limit: 4_096,
            },
            runtime: runtime(),
            required_reads: vec![read],
            terminal_operations: vec![TerminalOperationV1::WorkComplete],
            remaining_campaign_allowance: MicroUsd::new(100 - self.provider_cost_spent),
            revision: self.campaign_revision,
            packet_digest: digest(unique_number()),
        };
        let mut wire = packet_wire(&packet, b"system", b"assignment");
        let packet_digest = unsigned_assignment_packet_digest_v1(&wire).expect("unsigned packet");
        wire.packet_digest = packet_digest.to_hex();
        packet.packet_digest = packet_digest;
        let packet_bytes = canonical_assignment_packet_json_v1(&wire)
            .expect("canonical packet")
            .into_bytes();
        let packet_artifact = register(&self.store, &self.build, "packet", &packet_bytes).await;
        let assignment = process
            .create_assignment(
                &self.build.cas,
                &CreateAssignment {
                    principal: "kernel".to_owned(),
                    command_id: unique("assignment"),
                    expected_campaign_revision: ExpectedRevision::new(self.campaign_revision),
                    identity,
                    packet: packet.clone(),
                    packet_bytes: packet_bytes.clone(),
                    packet_artifact: packet_artifact.sealed,
                    required_read_manifest_artifact_id: manifest.artifact_id,
                    attempt_ordinal: self.assignment_ordinal,
                },
            )
            .await
            .expect("assignment");
        self.campaign_revision = assignment.resulting_campaign_revision;
        let session = process
            .start_session(&StartSession {
                principal: "kernel".to_owned(),
                command_id: unique("session"),
                expected_assignment_revision: ExpectedRevision::new(assignment.resulting_revision),
                assignment_id: assignment.assignment_id,
                packet_digest,
                custody: ProcessCustodyV1 {
                    pid: std::process::id(),
                    pgid: std::process::id(),
                    started_at_unix_millis: unique_number(),
                },
            })
            .await
            .expect("session");
        LiveSession {
            session_id: session.session_id,
            session_revision: session.resulting_revision,
            packet,
            packet_bytes,
            packet_artifact: packet_artifact.sealed,
            manifest_artifact_id: manifest.artifact_id,
            workspace,
            staging,
        }
    }

    async fn finish_session(&mut self, session: LiveSession) {
        let process = self.store.process_store();
        let daemon = LocalDaemon::bind(
            LocalTransportConfig::new(std::env::temp_dir().join(unique("daemon"))),
            &self.store,
        )
        .await
        .expect("daemon");
        let (_, connection) = daemon
            .create_admitted_actor_socketpair(&process, session.session_id, &session.packet)
            .await
            .expect("actor connection");
        let mut reads = connection
            .workspace_read_authority(
                &session.workspace,
                session.manifest_artifact_id,
                session.packet.required_reads.clone(),
            )
            .expect("read authority");
        reads
            .read_exact(RepositoryRelativePath::parse("AGENTS.md").unwrap())
            .expect("exact required read");
        let assertion = reads
            .seal_assertion(&self.build.cas, &session.staging)
            .expect("assertion");
        self.store
            .register_artifact(
                &self.build.cas,
                &RegisterArtifact {
                    principal: "kernel".to_owned(),
                    command_id: unique("assertion"),
                    expected_kernel_build_revision: ExpectedRevision::new(
                        self.build.receipt.resulting_revision,
                    ),
                    kernel_build_id: self.build.kernel_build_id,
                    sealed: assertion.artifact(),
                },
            )
            .await
            .expect("assertion artifact");
        let evidence = process
            .verify_terminal_evidence_with_packet_bytes(
                &self.build.cas,
                session.session_id,
                &session.packet,
                session.packet_artifact,
                &session.packet_bytes,
                TerminalArtifactSeals {
                    transcript: self.common.sealed,
                    stdout: self.common.sealed,
                    stderr: self.common.sealed,
                    partial_transcript: None,
                },
                assertion,
                Some(UsageTotalsV1 {
                    input_tokens: 1,
                    output_tokens: 1,
                    reported_cost_micro_usd: Some(MicroUsd::new(1)),
                    ..UsageTotalsV1::default()
                }),
            )
            .await
            .expect("terminal evidence");
        let terminal = process
            .terminal_session(
                "kernel",
                &unique("terminal"),
                session.session_id,
                &TerminalReportV1 {
                    packet_digest: session.packet.packet_digest,
                    expected_session_revision: ExpectedRevision::new(session.session_revision),
                    operation: Some(TerminalOperationV1::WorkComplete),
                    stop_reason: StopReasonV1::Completed,
                    report_digest: digest(unique_number()),
                },
                evidence,
            )
            .await
            .expect("terminal session");
        self.campaign_revision = terminal.campaign_revision;
        self.provider_cost_spent += 1;
        daemon.shutdown().await.expect("daemon shutdown");
    }
}

struct LiveSession {
    session_id: factory_protocol::SessionId,
    session_revision: AggregateRevision,
    packet: AssignmentPacketV1,
    packet_bytes: Vec<u8>,
    packet_artifact: CasArtifact,
    manifest_artifact_id: factory_protocol::ArtifactId,
    workspace: std::path::PathBuf,
    staging: std::path::PathBuf,
}

struct InstalledBuild {
    cas: CasStore,
    receipt: factory_kernel::storage::KernelBuildReceipt,
    kernel_build_id: KernelBuildId,
}

#[derive(Clone, Copy)]
struct RegisteredArtifact {
    artifact_id: factory_protocol::ArtifactId,
    sealed: CasArtifact,
}

impl RegisteredArtifact {
    fn reference(self) -> SealedArtifactReferenceV1 {
        SealedArtifactReferenceV1 {
            artifact_id: self.artifact_id,
            digest: self.sealed.digest(),
            byte_length: self.sealed.byte_length(),
        }
    }
}

async fn install_build(store: &KernelStore) -> InstalledBuild {
    let cas = CasStore::new_with_seed(
        std::env::temp_dir().join(unique("cas")),
        4 * 1024 * 1024,
        unique_number(),
    )
    .expect("CAS");
    let staging = cas.runtime_root().join("staging");
    fs::create_dir_all(&staging).expect("staging");
    fs::write(staging.join("qualification"), b"qualified").expect("qualification");
    let qualification = cas
        .adopt(&staging, "qualification")
        .expect("seal qualification");
    let receipt = store
        .install_kernel_build(
            &cas,
            &InstallKernelBuild {
                principal: "operator".to_owned(),
                command_id: unique("build"),
                expected_revision: ExpectedRevision::new(
                    store
                        .kernel_build_status()
                        .await
                        .expect("build status")
                        .aggregate_revision,
                ),
                build_id: KernelBuildId::new(digest(unique_number())),
                source_digest: digest(unique_number()),
                binary_digest: digest(unique_number()),
                schema_identity: SCHEMA_IDENTITY.to_owned(),
                deno_executable_path: "/opt/factory/deno".to_owned(),
                deno_version: "2.9.4".to_owned(),
                deno_lock_digest: digest(unique_number()),
                qualification_receipt: qualification,
            },
        )
        .await
        .expect("build install");
    InstalledBuild {
        cas,
        kernel_build_id: receipt.kernel_build_id,
        receipt,
    }
}

async fn register(
    store: &KernelStore,
    build: &InstalledBuild,
    label: &str,
    bytes: &[u8],
) -> RegisteredArtifact {
    let path = build
        .cas
        .runtime_root()
        .join("staging")
        .join(format!("{label}-{}", unique_number()));
    fs::write(&path, bytes).expect("artifact source");
    let sealed = build
        .cas
        .adopt(path.parent().unwrap(), path.file_name().unwrap())
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
    RegisteredArtifact {
        artifact_id: receipt.artifact_id,
        sealed,
    }
}

async fn admit_application(
    store: &KernelStore,
    build: &InstalledBuild,
    repository_key: &str,
    repository_path: &str,
) -> factory_kernel::storage::ApplicationRevisionReceipt {
    let root = build.cas.runtime_root().join(unique("application"));
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
        let bytes = format!("template-{index}");
        fs::write(root.join(path), &bytes).expect("template");
        templates.push((*path, ContentDigest::of_bytes(bytes.as_bytes())));
    }
    let application_key = unique("application");
    let bundle = bundle_json(
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
                principal: "operator".to_owned(),
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
    let rationale = register(store, build, "application-activation", b"activate").await;
    store
        .activate_application_revision(&ActivateApplicationRevision {
            principal: architect(),
            command_id: unique("application-activate"),
            expected_revision: ExpectedRevision::new(admitted.resulting_revision),
            application_key: factory_protocol::ApplicationKey::parse(application_key)
                .expect("application key"),
            application_revision_id: admitted.application_revision_id,
            rationale: rationale.reference(),
        })
        .await
        .expect("activate application");
    admitted
}

fn requalification(actual: factory_protocol::ArtifactId) -> CurrentHeadRequalification {
    CurrentHeadRequalification {
        current_head_commit: object('a').as_str().to_owned(),
        current_head_tree: object('b').as_str().to_owned(),
        first_actual_observation_artifact_id: actual,
        second_actual_observation_artifact_id: actual,
    }
}

fn architect() -> ArchitectPrincipalV1 {
    ArchitectPrincipalV1::parse("grand-architect").expect("principal")
}

fn object(character: char) -> RepositoryObjectIdV1 {
    RepositoryObjectIdV1::parse(character.to_string().repeat(40)).expect("object ID")
}

fn model() -> ModelProfileV1 {
    ModelProfileV1 {
        provider: "provider-free".to_owned(),
        model_id: "fixture".to_owned(),
        thinking_level: ThinkingLevelV1::None,
        context_token_limit: 1,
        output_token_limit: 1,
        price_input_micro_usd_per_million_tokens: MicroUsd::new(1),
        price_output_micro_usd_per_million_tokens: MicroUsd::new(1),
        price_cache_read_micro_usd_per_million_tokens: MicroUsd::new(1),
        price_cache_write_micro_usd_per_million_tokens: MicroUsd::new(1),
        capability_flags: Vec::new(),
    }
}

fn runtime() -> RuntimeIdentityV1 {
    RuntimeIdentityV1 {
        deno_executable: AbsoluteHostPath::parse("/opt/factory/deno").unwrap(),
        deno_version: "2.9.4".to_owned(),
        source_graph_digest: digest(1),
        resolved_dependency_graph_digest: digest(2),
        deno_json_digest: digest(3),
        deno_lock_digest: digest(4),
        pi_version: "fixture".to_owned(),
        credential: CredentialDescriptorV1::PiAuthStore {
            path: factory_protocol::RuntimeRelativePath::parse("credentials/fixture").unwrap(),
        },
    }
}

fn canonical_manifest(reads: &[ReadExactFileV1]) -> Vec<u8> {
    let mut bytes = b"factory-read-manifest-v1\0".to_vec();
    bytes.extend_from_slice(&(reads.len() as u32).to_be_bytes());
    for read in reads {
        bytes.extend_from_slice(&(read.path.as_str().len() as u32).to_be_bytes());
        bytes.extend_from_slice(read.path.as_str().as_bytes());
        bytes.extend_from_slice(&read.digest.as_bytes());
        bytes.extend_from_slice(&(read.reason.len() as u32).to_be_bytes());
        bytes.extend_from_slice(read.reason.as_bytes());
    }
    bytes
}

fn packet_wire(
    packet: &AssignmentPacketV1,
    system_prompt: &[u8],
    assignment_prompt: &[u8],
) -> AssignmentPacketWireV1 {
    AssignmentPacketWireV1 {
        format_version: 1,
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
        repository_base_identity: digest(5).to_hex(),
        factory_base_identity: digest(6).to_hex(),
        ticket_attempt_id: packet.ticket_attempt_id.map(|id| id.get()),
        candidate_id: packet.candidate_id.map(|id| id.get()),
        assignment_evidence: packet
            .assignment_evidence
            .iter()
            .map(|evidence| AssignmentEvidenceWireV1 {
                role: evidence.role.wire_name().to_owned(),
                artifact_id: evidence.artifact_id.get(),
                digest: evidence.digest.to_hex(),
                byte_length: evidence.byte_length,
            })
            .collect(),
        system_prompt_artifact_id: packet.system_prompt_artifact_id.get(),
        assignment_prompt_artifact_id: packet.assignment_prompt_artifact_id.get(),
        required_read_manifest_artifact_id: packet.required_read_manifest_artifact_id.get(),
        system_prompt_digest: ContentDigest::of_bytes(system_prompt).to_hex(),
        assignment_prompt_digest: ContentDigest::of_bytes(assignment_prompt).to_hex(),
        system_prompt_bytes_b64: base64(system_prompt),
        assignment_prompt_bytes_b64: base64(assignment_prompt),
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
                kind: "pi_auth_store".to_owned(),
                name: None,
                path: Some("credentials/fixture".to_owned()),
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
        tools: vec!["workspace_read".to_owned(), "work_complete".to_owned()],
        terminal_operations: vec!["work_complete".to_owned()],
        remaining_campaign_allowance_micro_usd: packet.remaining_campaign_allowance.get(),
        aggregate_revision: packet.revision.get(),
        packet_digest: String::new(),
    }
}

fn bundle_json(
    application_key: &str,
    repository_key: &str,
    repository_path: &str,
    templates: &[(&str, ContentDigest)],
) -> String {
    let template = |index: usize| TemplateWireV1 {
        source_path: templates[index].0.to_owned(),
        digest: templates[index].1.to_hex(),
        placeholders: Vec::new(),
        rendered_byte_limit: 4_096,
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
        stdout_byte_limit: 4_096,
        stderr_byte_limit: 4_096,
        expected_exit_status: 0,
    };
    let office = |name: &str, system: usize, assignment: usize| OfficeWireV1 {
        office: name.to_owned(),
        system_template: template(system),
        assignment_template: template(assignment),
        tools: vec!["workspace_read".to_owned()],
        model: ModelWireV1 {
            provider: "fixture".to_owned(),
            model_id: "fixture".to_owned(),
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
            output_byte_limit: 4_096,
        },
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
        office_profiles: vec![
            office("product_research", 1, 2),
            office("engineering", 3, 4),
            office("quality", 5, 6),
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
        required_reads: vec![factory_protocol::RequiredReadWireV1 {
            path: "AGENTS.md".to_owned(),
            reason: "authority contract".to_owned(),
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
            subject_byte_limit: 120,
            body_byte_limit: 8_192,
        },
    })
    .expect("canonical bundle")
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

fn unique(prefix: &str) -> String {
    format!("{prefix}-{}", unique_number())
}

fn unique_number() -> u64 {
    (u64::from(std::process::id()) << 32) | NEXT_TEST.fetch_add(1, Ordering::Relaxed)
}

fn digest(serial: u64) -> ContentDigest {
    let mut bytes = [0; 32];
    for chunk in bytes.chunks_exact_mut(8) {
        chunk.copy_from_slice(&serial.to_be_bytes());
    }
    ContentDigest::from_bytes(bytes)
}
