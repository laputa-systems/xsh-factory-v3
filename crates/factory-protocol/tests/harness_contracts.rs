use factory_protocol::{
    ActorToolV2, ApplicationRevisionId, ArtifactId, AssignmentRole, ContextInclusionClassV2,
    ContextItemV2, ContextReferenceV2, HARNESS_COMPILER_VERSION_V2, HarnessSpecV2, MicroUsd,
    OfficeId,
};

fn spec() -> HarnessSpecV2 {
    HarnessSpecV2 {
        compiler_version: HARNESS_COMPILER_VERSION_V2,
        application_revision_id: ApplicationRevisionId::new(1).expect("application revision"),
        office_id: OfficeId::new(2).expect("office"),
        assignment_role: AssignmentRole::Engineering,
        objective: "ticket-3-revision-4-attempt-5".to_owned(),
        context_items: vec![ContextItemV2 {
            reference: ContextReferenceV2::Artifact(ArtifactId::new(6).expect("artifact")),
            inclusion: ContextInclusionClassV2::DirectEvidence,
            reason: "direct ticket evidence".to_owned(),
        }],
        capabilities: vec![ActorToolV2::WorkspaceRead, ActorToolV2::ArtifactRead],
        remaining_campaign_allowance: MicroUsd::new(7),
    }
}

#[test]
fn harness_specs_are_closed_bounded_and_reference_identity_once() {
    assert_eq!(spec().validate(), Ok(()));

    let mut duplicate = spec();
    duplicate
        .context_items
        .push(duplicate.context_items[0].clone());
    assert!(duplicate.validate().is_err());

    let mut duplicate_tool = spec();
    duplicate_tool.capabilities.push(ActorToolV2::ArtifactRead);
    assert!(duplicate_tool.validate().is_err());
}

#[test]
fn harness_context_reasons_are_one_bounded_explanatory_line() {
    let mut malformed = spec();
    malformed.context_items[0].reason = "a reason\nthat changes the prompt".to_owned();
    assert!(malformed.validate().is_err());

    let mut unsupported_compiler = spec();
    unsupported_compiler.compiler_version = HARNESS_COMPILER_VERSION_V2 + 1;
    assert!(unsupported_compiler.validate().is_err());
}
