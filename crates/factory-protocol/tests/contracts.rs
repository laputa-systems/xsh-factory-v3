use std::{collections::BTreeMap, str::FromStr};

use factory_protocol::{
    APPLICATION_BUNDLE_V1_FORMAT, AbsoluteHostPath, ActorToolV1, AggregateRevision,
    ApplicationBundleV1, ApplicationKey, ApplicationRelativePath, AssignmentRole,
    AssignmentRoleProfileV1, CommandProfileV1, CommitMessagePolicyV1, ContentDigest,
    CredentialDescriptorV1, DeliveryModeV1, DurationMillis, EnvironmentAdditionV1, ExecutableV1,
    GitPolicyV1, MicroUsd, ModelCapabilityV1, ModelProfileV1, RepositoryBindingV1,
    RepositoryRelativePath, RequiredReadV1, RuntimeRelativePath, SessionLimitsV1,
    TemplateArtifactV1, TemplatePlaceholderV1, ThinkingLevelV1, TicketBoundsV1, TicketPolicyV1,
    ValidationProfilesV1,
};

fn path(value: &str) -> RepositoryRelativePath {
    RepositoryRelativePath::parse(value).expect("fixture path is valid")
}

fn application_path(value: &str) -> ApplicationRelativePath {
    ApplicationRelativePath::parse(value).expect("fixture application path is valid")
}

fn digest(byte: u8) -> ContentDigest {
    ContentDigest::from_bytes([byte; 32])
}

fn template(path_value: &str, byte: u8) -> TemplateArtifactV1 {
    TemplateArtifactV1 {
        source_path: application_path(path_value),
        digest: digest(byte),
        placeholders: vec![TemplatePlaceholderV1::parse("ASSIGNMENT_ID").expect("placeholder")],
        rendered_byte_limit: 4096,
    }
}

fn command(name: &str) -> CommandProfileV1 {
    CommandProfileV1 {
        name: name.to_owned(),
        executable: ExecutableV1::ApprovedTool(factory_protocol::ApprovedToolV1::Cargo),
        argv: vec!["test".to_owned()],
        working_directory: path("."),
        environment: vec![EnvironmentAdditionV1 {
            name: "TERM".to_owned(),
            value: "dumb".to_owned(),
        }],
        timeout: DurationMillis::new(60_000),
        stdout_byte_limit: 1_000_000,
        stderr_byte_limit: 1_000_000,
        expected_exit_status: 0,
    }
}

fn assignment_role_profile(
    assignment_role: AssignmentRole,
    terminal_tool: ActorToolV1,
    byte: u8,
) -> AssignmentRoleProfileV1 {
    AssignmentRoleProfileV1 {
        assignment_role,
        system_template: template("templates/system.md", byte),
        assignment_template: template("templates/assignment.md", byte + 1),
        tools: vec![
            ActorToolV1::WorkspaceRead,
            terminal_tool,
            ActorToolV1::WorkComplete,
        ],
        model: ModelProfileV1 {
            provider: "provider".to_owned(),
            model_id: "model".to_owned(),
            thinking_level: ThinkingLevelV1::High,
            context_token_limit: 1,
            output_token_limit: 1,
            price_input_micro_usd_per_million_tokens: MicroUsd::new(1),
            price_output_micro_usd_per_million_tokens: MicroUsd::new(1),
            price_cache_read_micro_usd_per_million_tokens: MicroUsd::new(1),
            price_cache_write_micro_usd_per_million_tokens: MicroUsd::new(1),
            capability_flags: vec![],
        },
        limits: SessionLimitsV1 {
            turn_limit: 1,
            wall_limit: DurationMillis::new(1),
            output_byte_limit: 1,
        },
    }
}

fn bundle() -> ApplicationBundleV1 {
    ApplicationBundleV1 {
        format_version: APPLICATION_BUNDLE_V1_FORMAT,
        application_key: ApplicationKey::parse("example").expect("application key"),
        predecessor_bundle: None,
        repository: RepositoryBindingV1 {
            repository_key: "product".to_owned(),
            canonical_local_path: AbsoluteHostPath::parse("/workspace/product").expect("path"),
            default_branch: "main".to_owned(),
            delivery_mode: DeliveryModeV1::LocalFastForwardOnly,
        },
        mission_template: template("templates/mission.md", 1),
        assignment_role_profiles: vec![
            assignment_role_profile(
                AssignmentRole::ProductResearch,
                ActorToolV1::ProductSubmitTicket,
                2,
            ),
            assignment_role_profile(
                AssignmentRole::Engineering,
                ActorToolV1::CandidateCheckpointRegression,
                4,
            ),
            assignment_role_profile(AssignmentRole::Quality, ActorToolV1::QualitySubmitReview, 6),
        ],
        ticket_policy: TicketPolicyV1 {
            low_water: 1,
            target: 2,
            maximum: 3,
            proposal_maximum: 1,
            ticket_bounds: TicketBoundsV1 {
                narrative_byte_limit: 1,
                acceptance_criteria_limit: 1,
                contract_read_limit: 1,
            },
        },
        required_reads: vec![RequiredReadV1 {
            path: path("AGENTS.md"),
            reason: "product contract".to_owned(),
        }],
        reproducer_profiles: vec![command("reproduce")],
        validation_profiles: ValidationProfilesV1 {
            focused: vec![command("focused")],
            full: vec![command("full")],
        },
        git_policy: GitPolicyV1 {
            forbidden_paths: vec![path(".git")],
            delivery_mode: DeliveryModeV1::LocalFastForwardOnly,
            provenance_trailers_required: true,
        },
        commit_message_policy: CommitMessagePolicyV1 {
            subject_byte_limit: 72,
            body_byte_limit: 2048,
        },
    }
}

#[test]
fn model_capabilities_and_credentials_are_closed() {
    let mut application = bundle();
    application.assignment_role_profiles[0]
        .model
        .capability_flags = vec![ModelCapabilityV1::Reasoning, ModelCapabilityV1::Reasoning];
    assert!(application.validate().is_err());

    assert!(
        CredentialDescriptorV1::Environment {
            name: "FACTORY_PROVIDER_KEY".to_owned(),
        }
        .validate()
        .is_ok()
    );
    assert!(
        CredentialDescriptorV1::Environment {
            name: "factory_provider_key".to_owned(),
        }
        .validate()
        .is_err()
    );
    assert!(
        CredentialDescriptorV1::PiAuthStore {
            path: RuntimeRelativePath::parse("credentials/pi.json").unwrap(),
        }
        .validate()
        .is_ok()
    );
}

#[test]
fn template_renderer_is_strict_and_one_pass() {
    let mut template = template("templates/system.md", 9);
    template.placeholders = vec![
        TemplatePlaceholderV1::parse("ASSIGNMENT_ID").unwrap(),
        TemplatePlaceholderV1::parse("MISSION").unwrap(),
    ];
    let mut values = BTreeMap::new();
    values.insert("ASSIGNMENT_ID".to_owned(), "a-1".to_owned());
    values.insert("MISSION".to_owned(), "${ASSIGNMENT_ID}".to_owned());
    assert_eq!(
        factory_protocol::render_template_v1(&template, "id=${ASSIGNMENT_ID}; ${MISSION}", &values)
            .unwrap(),
        b"id=a-1; ${ASSIGNMENT_ID}"
    );

    let mut missing = values.clone();
    missing.remove("MISSION");
    assert!(
        factory_protocol::render_template_v1(&template, "${ASSIGNMENT_ID}; ${MISSION}", &missing)
            .is_err()
    );
    assert!(factory_protocol::render_template_v1(&template, "${UNKNOWN}", &values).is_err());
    assert!(factory_protocol::render_template_v1(&template, "${ASSIGNMENT_ID", &values).is_err());
    let mut nul = values.clone();
    nul.insert("MISSION".to_owned(), "bad\0value".to_owned());
    assert!(
        factory_protocol::render_template_v1(&template, "${ASSIGNMENT_ID}; ${MISSION}", &nul)
            .is_err()
    );
    template.rendered_byte_limit = 2;
    assert!(
        factory_protocol::render_template_v1(&template, "${ASSIGNMENT_ID}; ${MISSION}", &values)
            .is_err()
    );
}

#[test]
fn bundle_requires_the_fixed_assignment_roles_and_closed_tools() {
    let valid = bundle();
    assert_eq!(valid.validate(), Ok(()));

    let mut wrong_tool = bundle();
    wrong_tool.assignment_role_profiles[0]
        .tools
        .push(ActorToolV1::CandidateSubmit);
    assert!(wrong_tool.validate().is_err());

    let mut missing_role_profile = bundle();
    missing_role_profile.assignment_role_profiles.pop();
    assert!(missing_role_profile.validate().is_err());
}

#[test]
fn paths_revisions_and_digests_are_strict_at_boundaries() {
    assert!(RepositoryRelativePath::parse("src/lib.rs").is_ok());
    assert!(RepositoryRelativePath::parse("../escape").is_err());
    assert!(RepositoryRelativePath::parse("a//b").is_err());
    assert!(RepositoryRelativePath::parse(".").is_ok());

    assert_eq!(AggregateRevision::initial().next().expect("next").get(), 1);
    assert!(ApplicationKey::parse("MixedCase").is_err());

    let encoded = ContentDigest::of_bytes(b"stable bytes").to_hex();
    assert_eq!(
        ContentDigest::from_str(&encoded).expect("digest").to_hex(),
        encoded
    );
    assert!(ContentDigest::from_str(&encoded.to_uppercase()).is_err());
}
