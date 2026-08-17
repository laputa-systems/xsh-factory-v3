use std::collections::BTreeMap;

use factory_protocol::{
    AbsoluteHostPath, ActorPolicyArtifactV2, ActorToolV2, ApplicationBundleV2,
    ApplicationCompilerV2, ApplicationKey, ApplicationRelativePath, ApplicationSourceFileV2,
    ApprovedToolV2, AssignmentRole, AssignmentRoleProfileV2, CommandProfileV2,
    CommitMessagePolicyV2, ContentDigest, DeliveryModeV2, DurationMillis, EnvironmentAdditionV2,
    ExecutableV2, GitPolicyV2, MAX_SESSION_OUTPUT_BYTES, MicroUsd, ModelProfileV2,
    PolicyEntrypointV2, RepositoryBindingV2, RepositoryRelativePath, RequiredReadV2,
    SessionLimitsV2, TemplateArtifactV2, TemplatePlaceholderV2, ThinkingLevelV2, TicketBoundsV2,
    TicketPolicyV2, ValidationProfilesV2, canonical_command_profile_json_from_domain_v2,
    parse_command_profile_v2, seal_policy_artifact_v2,
};

fn app_path(value: &str) -> ApplicationRelativePath {
    ApplicationRelativePath::parse(value).expect("valid application path")
}

fn repo_path(value: &str) -> RepositoryRelativePath {
    RepositoryRelativePath::parse(value).expect("valid repository path")
}

fn digest(source: &[u8]) -> ContentDigest {
    ContentDigest::of_bytes(source)
}

fn template(path: &str, source: &[u8]) -> TemplateArtifactV2 {
    TemplateArtifactV2 {
        source_path: app_path(path),
        digest: digest(source),
        placeholders: vec![TemplatePlaceholderV2::parse("ASSIGNMENT_ID").unwrap()],
        rendered_byte_limit: 4096,
    }
}

fn policy(path: &str, source: &[u8]) -> ActorPolicyArtifactV2 {
    ActorPolicyArtifactV2 {
        source_path: app_path(path),
        digest: digest(source),
        byte_limit: 4096,
        entrypoint: PolicyEntrypointV2::FactoryPolicy,
    }
}

fn command(name: &str) -> CommandProfileV2 {
    CommandProfileV2 {
        name: name.to_owned(),
        executable: ExecutableV2::ApprovedTool(ApprovedToolV2::Cargo),
        argv: vec!["test".to_owned()],
        working_directory: repo_path("."),
        environment: vec![EnvironmentAdditionV2 {
            name: "TERM".to_owned(),
            value: "dumb".to_owned(),
        }],
        timeout: DurationMillis::new(1000),
        stdout_byte_limit: 4096,
        stderr_byte_limit: 4096,
        expected_exit_status: 0,
    }
}

fn profile(
    role: AssignmentRole,
    policy_path: &str,
    tools: &[ActorToolV2],
) -> AssignmentRoleProfileV2 {
    let system = b"system ${ASSIGNMENT_ID}";
    let assignment = b"assignment ${ASSIGNMENT_ID}";
    let policy_source = b"return { factory_policy = function() end }";
    AssignmentRoleProfileV2 {
        assignment_role: role,
        system_template: template("templates/system.md", system),
        assignment_template: template("templates/assignment.md", assignment),
        policy: policy(policy_path, policy_source),
        tools: tools.to_vec(),
        model: ModelProfileV2 {
            provider: "test-provider".to_owned(),
            model_id: "test-model".to_owned(),
            thinking_level: ThinkingLevelV2::None,
            context_token_limit: 1024,
            output_token_limit: 128,
            price_input_micro_usd_per_million_tokens: MicroUsd::new(1),
            price_output_micro_usd_per_million_tokens: MicroUsd::new(1),
            price_cache_read_micro_usd_per_million_tokens: MicroUsd::new(1),
            price_cache_write_micro_usd_per_million_tokens: MicroUsd::new(1),
            capability_flags: vec![],
        },
        limits: SessionLimitsV2 {
            wall_limit: DurationMillis::new(1000),
            output_byte_limit: 4096,
        },
    }
}

fn bundle() -> ApplicationBundleV2 {
    let mission = b"mission";
    ApplicationBundleV2 {
        format_version: 2,
        application_key: ApplicationKey::parse("compiler-test").unwrap(),
        predecessor_bundle: None,
        repository: RepositoryBindingV2 {
            repository_key: "product".to_owned(),
            canonical_local_path: AbsoluteHostPath::parse("/workspace/product").unwrap(),
            default_branch: "main".to_owned(),
            delivery_mode: DeliveryModeV2::LocalFastForwardOnly,
        },
        mission_template: template("templates/mission.md", mission),
        assignment_role_profiles: vec![
            profile(
                AssignmentRole::ProductResearch,
                "policies/product.luau",
                &[ActorToolV2::WorkspaceRead, ActorToolV2::ProductSubmitTicket],
            ),
            profile(
                AssignmentRole::Engineering,
                "policies/engineering.luau",
                &[ActorToolV2::WorkspaceRead, ActorToolV2::CandidateSubmit],
            ),
            profile(
                AssignmentRole::Quality,
                "policies/quality.luau",
                &[ActorToolV2::WorkspaceRead, ActorToolV2::QualitySubmitReview],
            ),
        ],
        ticket_policy: TicketPolicyV2 {
            low_water: 1,
            target: 1,
            maximum: Some(2),
            proposal_maximum: 1,
            ticket_bounds: TicketBoundsV2 {
                narrative_byte_limit: 1024,
                acceptance_criteria_limit: 4,
                contract_read_limit: 4,
            },
        },
        required_reads: vec![RequiredReadV2 {
            path: repo_path("AGENTS.md"),
            reason: "contract".to_owned(),
        }],
        reproducer_profiles: vec![command("reproduce")],
        validation_profiles: ValidationProfilesV2 {
            focused: vec![command("focused")],
            full: vec![command("full")],
        },
        git_policy: GitPolicyV2 {
            forbidden_paths: vec![repo_path(".git")],
            delivery_mode: DeliveryModeV2::LocalFastForwardOnly,
            provenance_trailers_required: true,
        },
        commit_message_policy: CommitMessagePolicyV2 {
            subject_byte_limit: 72,
            body_byte_limit: 2048,
        },
    }
}

fn source(path: &str, bytes: &[u8]) -> ApplicationSourceFileV2 {
    ApplicationSourceFileV2::new(app_path(path), bytes.to_vec()).unwrap()
}

fn source_files() -> Vec<ApplicationSourceFileV2> {
    let policy_source = b"return { factory_policy = function() end }";
    vec![
        source("templates/mission.md", b"mission"),
        source("templates/system.md", b"system ${ASSIGNMENT_ID}"),
        source("templates/assignment.md", b"assignment ${ASSIGNMENT_ID}"),
        source("policies/product.luau", policy_source),
        source("policies/engineering.luau", policy_source),
        source("policies/quality.luau", policy_source),
    ]
}

#[test]
fn compiler_seals_all_declared_files_with_stable_identity() {
    let first = ApplicationCompilerV2::compile(bundle(), source_files()).unwrap();
    let second = ApplicationCompilerV2::compile(bundle(), source_files()).unwrap();
    assert_eq!(first.source_digest, second.source_digest);
    assert_eq!(first.files, second.files);
    assert_eq!(
        first.file(&app_path("policies/engineering.luau")).unwrap(),
        b"return { factory_policy = function() end }"
    );
    assert!(
        first
            .files
            .windows(2)
            .all(|files| files[0].path < files[1].path)
    );
}

#[test]
fn domain_command_profile_spelling_is_exactly_parseable() {
    let profile = command("reproduce");
    let canonical = canonical_command_profile_json_from_domain_v2(&profile);
    assert_eq!(
        parse_command_profile_v2(canonical.as_bytes()).unwrap(),
        profile
    );
    assert_eq!(
        parse_command_profile_v2(format!("{canonical}\n").as_bytes()).unwrap(),
        profile
    );
}

#[test]
fn malformed_command_profile_reports_actionable_canonicality() {
    let error = parse_command_profile_v2(b"./target/debug/product fixture.script").unwrap_err();
    assert!(
        error
            .to_string()
            .contains("command bytes are not canonical V2 JSON")
    );
}

#[test]
fn compiler_rejects_undeclared_and_mismatched_source() {
    let mut files = source_files();
    files.push(source("README.md", b"not admitted"));
    assert!(ApplicationCompilerV2::compile(bundle(), files).is_err());

    let mut files = source_files();
    files[0] = source("templates/mission.md", b"wrong bytes");
    assert!(ApplicationCompilerV2::compile(bundle(), files).is_err());
}

#[test]
fn ticket_policy_accepts_an_unrestricted_ready_backlog() {
    let mut application = bundle();
    application.ticket_policy.maximum = None;
    assert!(ApplicationCompilerV2::compile(application, source_files()).is_ok());
}

#[test]
fn compiler_rejects_session_output_limit_above_cas_ceiling() {
    let mut application = bundle();
    application.assignment_role_profiles[0]
        .limits
        .output_byte_limit = (MAX_SESSION_OUTPUT_BYTES + 1) as u32;
    assert!(ApplicationCompilerV2::compile(application, source_files()).is_err());
}

#[test]
fn policy_sealing_enforces_entrypoint_digest_utf8_and_limit() {
    let source = b"return { factory_policy = function() end }";
    let artifact = policy("policies/product.luau", source);
    assert_eq!(seal_policy_artifact_v2(&artifact, source).unwrap(), source);

    let mut wrong_digest = artifact.clone();
    wrong_digest.digest = digest(b"different");
    assert!(seal_policy_artifact_v2(&wrong_digest, source).is_err());

    let mut too_small = artifact.clone();
    too_small.byte_limit = 1;
    assert!(seal_policy_artifact_v2(&too_small, source).is_err());

    let mut wrong_entrypoint = artifact;
    // Parsing is closed at the wire/domain boundary; this also documents that
    // no arbitrary Luau function name can enter the sealed policy contract.
    assert!(PolicyEntrypointV2::parse("main").is_err());
    wrong_entrypoint.entrypoint = PolicyEntrypointV2::FactoryPolicy;
    assert!(seal_policy_artifact_v2(&wrong_entrypoint, source).is_ok());
}

#[test]
fn renderer_is_one_pass_and_rejects_unknown_values() {
    let artifact = TemplateArtifactV2 {
        source_path: app_path("templates/system.md"),
        digest: digest(b"unused"),
        placeholders: vec![TemplatePlaceholderV2::parse("NAME").unwrap()],
        rendered_byte_limit: 64,
    };
    let mut values = BTreeMap::new();
    values.insert("NAME".to_owned(), "${OTHER}".to_owned());
    let rendered =
        factory_protocol::render_template_v2(&artifact, "hello ${NAME}", &values).unwrap();
    assert_eq!(rendered, b"hello ${OTHER}");
    values.insert("OTHER".to_owned(), "nope".to_owned());
    assert!(factory_protocol::render_template_v2(&artifact, "hello ${NAME}", &values).is_err());
}
