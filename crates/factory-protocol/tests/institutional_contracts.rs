use factory_protocol::{
    AggregateRevision, ApplicationRevisionId, ArtifactId, Claim, ClaimId, ClaimState,
    ContentDigest, ContractError, Decision, DecisionId, DecisionKind, DecisionState, Experiment,
    ExperimentId, ExperimentRun, ExperimentRunId, ExperimentRunState, ExperimentState,
    InstitutionalReference, OfficeId, Project, ProjectId, ProjectState, Publication, PublicationId,
    PublicationKind, RepositoryObjectIdV1, Rfc, RfcId, RfcRevision, RfcRevisionId, RfcState,
    SealedArtifactReferenceV1, SessionId,
};

fn artifact(id: i64, bytes: u64) -> SealedArtifactReferenceV1 {
    SealedArtifactReferenceV1 {
        artifact_id: ArtifactId::new(id).expect("positive artifact ID"),
        digest: ContentDigest::from_bytes([id as u8; 32]),
        byte_length: bytes,
    }
}

fn base() -> (ApplicationRevisionId, OfficeId) {
    (
        ApplicationRevisionId::new(1).expect("application revision ID"),
        OfficeId::new(2).expect("office ID"),
    )
}

#[test]
fn institutional_ids_are_positive_and_architect_decisions_remain_distinct() {
    assert!(ProjectId::new(0).is_err());
    assert!(ExperimentRunId::new(-1).is_err());
    assert_eq!(DecisionId::new(7).expect("decision ID").get(), 7);

    fn architect_only(_: factory_protocol::ArchitectDecisionId) {}
    architect_only(factory_protocol::ArchitectDecisionId::new(7).expect("architect ID"));
    // The two constructors have deliberately different nominal types; a
    // DecisionId cannot be passed to `architect_only` without an explicit,
    // semantically meaningful conversion (there is no such conversion).
}

#[test]
fn project_and_rfc_revision_validate_sealed_bodies_and_bounds() {
    let (application_revision_id, owner_office_id) = base();
    let project = Project {
        id: ProjectId::new(3).expect("project ID"),
        application_revision_id,
        owner_office_id,
        title: "Control plane clarity".to_owned(),
        summary: "A bounded project summary".to_owned(),
        body: artifact(4, 10),
        state: ProjectState::Active,
        aggregate_revision: AggregateRevision::initial(),
    };
    assert_eq!(project.validate(), Ok(()));

    let revision = RfcRevision {
        id: RfcRevisionId::new(5).expect("RFC revision ID"),
        rfc_id: RfcId::new(6).expect("RFC ID"),
        application_revision_id,
        author_office_id: owner_office_id,
        revision_number: 1,
        summary: "Use typed institutional records".to_owned(),
        body: artifact(7, 12),
    };
    assert_eq!(revision.validate(), Ok(()));

    let invalid = Project {
        title: String::new(),
        ..project
    };
    assert!(matches!(
        invalid.validate(),
        Err(ContractError::InvalidValue { .. })
    ));
}

#[test]
fn experiment_and_run_keep_execution_facts_separate() {
    let (application_revision_id, owner_office_id) = base();
    let experiment = Experiment {
        id: ExperimentId::new(8).expect("experiment ID"),
        application_revision_id,
        owner_office_id,
        project_id: Some(ProjectId::new(3).expect("project ID")),
        question: "Does the typed resolver preserve replay?".to_owned(),
        summary: "A bounded replay experiment".to_owned(),
        intended_base: Some(
            RepositoryObjectIdV1::parse("0123456789012345678901234567890123456789")
                .expect("base commit"),
        ),
        intended_target: InstitutionalReference::RfcRevision(
            RfcRevisionId::new(5).expect("RFC revision ID"),
        ),
        evaluation_plan: artifact(9, 20),
        budget_micro_usd: 1_000,
        state: ExperimentState::Proposed,
        aggregate_revision: AggregateRevision::initial(),
    };
    assert_eq!(experiment.validate(), Ok(()));

    let run = ExperimentRun {
        id: ExperimentRunId::new(10).expect("experiment run ID"),
        experiment_id: experiment.id,
        application_revision_id,
        owner_office_id,
        base_commit: RepositoryObjectIdV1::parse("0123456789012345678901234567890123456789")
            .expect("base commit"),
        base_tree: RepositoryObjectIdV1::parse("abcdefabcdefabcdefabcdefabcdefabcdefabcd")
            .expect("base tree"),
        invocation: artifact(11, 20),
        candidate_id: None,
        result_artifact: None,
        evaluator_receipt: None,
        state: ExperimentRunState::Prepared,
        aggregate_revision: AggregateRevision::initial(),
    };
    assert_eq!(run.validate(), Ok(()));
}

#[test]
fn references_are_closed_and_publications_must_be_anchored_to_institutional_work() {
    let (application_revision_id, owner_office_id) = base();
    let project = InstitutionalReference::Project(ProjectId::new(3).expect("project ID"));
    assert!(project.can_anchor_publication());
    assert!(
        !InstitutionalReference::ExperimentRun(ExperimentRunId::new(10).expect("run ID"))
            .can_anchor_publication()
    );

    let publication = Publication {
        id: PublicationId::new(12).expect("publication ID"),
        application_revision_id,
        authoring_office_id: owner_office_id,
        originating_session_id: Some(SessionId::new(13).expect("session ID")),
        anchor: project,
        kind: PublicationKind::Finding,
        body: artifact(14, 15),
        reply_to: None,
        supersedes: None,
        aggregate_revision: AggregateRevision::initial(),
    };
    assert_eq!(publication.validate(), Ok(()));

    let mut unanchored = publication;
    unanchored.anchor =
        InstitutionalReference::Publication(PublicationId::new(15).expect("publication ID"));
    assert!(unanchored.validate().is_err());
}

#[test]
fn decision_rejects_decision_targets_but_accepts_typed_rfc_targets() {
    let (application_revision_id, owner_office_id) = base();
    let decision = Decision {
        id: DecisionId::new(16).expect("decision ID"),
        application_revision_id,
        deciding_office_id: owner_office_id,
        title: "Approve typed records".to_owned(),
        summary: "The required evidence is present".to_owned(),
        target: InstitutionalReference::RfcRevision(
            RfcRevisionId::new(6).expect("RFC revision ID"),
        ),
        kind: DecisionKind::Approve,
        state: DecisionState::Final,
        rationale: artifact(17, 24),
        aggregate_revision: AggregateRevision::initial(),
    };
    assert_eq!(decision.validate(), Ok(()));

    let invalid = Decision {
        target: InstitutionalReference::Decision(decision.id),
        ..decision
    };
    assert!(invalid.validate().is_err());
}

#[test]
fn rfc_and_claim_values_have_closed_lifecycles() {
    let (application_revision_id, owner_office_id) = base();
    let rfc = Rfc {
        id: RfcId::new(18).expect("RFC ID"),
        application_revision_id,
        owner_office_id,
        project_id: None,
        title: "Typed records".to_owned(),
        summary: "Keep institutional data searchable".to_owned(),
        state: RfcState::Draft,
        current_revision_id: None,
        aggregate_revision: AggregateRevision::initial(),
    };
    assert_eq!(rfc.validate(), Ok(()));

    let claim = Claim {
        id: ClaimId::new(19).expect("claim ID"),
        application_revision_id,
        owner_office_id,
        proposition: "Every publication has one typed anchor".to_owned(),
        body: artifact(20, 30),
        state: ClaimState::Proposed,
        aggregate_revision: AggregateRevision::initial(),
    };
    assert_eq!(claim.validate(), Ok(()));
}
