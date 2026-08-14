use factory_protocol as wire;
pub use factory_protocol::*;

use miniserde::{Deserialize, Serialize, json};

#[derive(Debug, PartialEq, Serialize, Deserialize)]
struct GoldenRequest {
    protocol_version: u16,
    request_id: String,
    operation: String,
    client_command_id: String,
    expected_revision: u64,
    workspace_relative_path: String,
    byte_limit: u64,
}

#[test]
fn request_frame_matches_checked_in_wire_shape() {
    let request = GoldenRequest {
        protocol_version: 1,
        request_id: "req-1".to_owned(),
        operation: wire::OP_ARTIFACT_SEAL_WORKSPACE_FILE.to_owned(),
        client_command_id: "cmd-1".to_owned(),
        expected_revision: 7,
        workspace_relative_path: "reports/result.json".to_owned(),
        byte_limit: 4096,
    };
    let payload = json::to_string(&request);
    const GOLDEN: &str =
        include_str!("../../../tests/protocol-fixtures/artifact-seal-request.json");
    let compact_golden: String = GOLDEN
        .chars()
        .filter(|character| !character.is_ascii_whitespace())
        .collect();
    assert_eq!(payload, compact_golden);
    let frame = wire::encode_frame(payload.as_bytes(), wire::REQUEST_FRAME_MAX_BYTES).unwrap();
    assert_eq!(&frame[..4], &(payload.len() as u32).to_be_bytes());
    assert_eq!(
        wire::decode_frame(&frame, wire::REQUEST_FRAME_MAX_BYTES).unwrap(),
        payload.as_bytes()
    );
}

#[test]
fn malformed_truncated_oversized_and_trailing_frames_are_rejected() {
    assert_eq!(
        wire::decode_frame(&[0, 0, 0], wire::REQUEST_FRAME_MAX_BYTES),
        Err(wire::FrameError::MissingLength)
    );
    assert!(matches!(
        wire::decode_frame(&[0, 0, 0, 4, b'a'], wire::REQUEST_FRAME_MAX_BYTES),
        Err(wire::FrameError::Truncated { .. })
    ));
    assert!(matches!(
        wire::decode_frame(&[0, 0, 0, 1, b'a', b'x'], wire::REQUEST_FRAME_MAX_BYTES),
        Err(wire::FrameError::TrailingBytes { .. })
    ));
    assert!(matches!(
        wire::encode_frame(&[0; 9], 8),
        Err(wire::FrameError::Oversized { .. })
    ));
    let invalid_json = wire::encode_frame(b"{", wire::REQUEST_FRAME_MAX_BYTES).unwrap();
    assert!(matches!(
        wire::decode_routing_envelope(&invalid_json, wire::REQUEST_FRAME_MAX_BYTES),
        Err(wire::FrameError::InvalidJson { .. })
    ));
    let wrong_version = wire::encode_frame(
        br#"{"protocol_version":2,"request_id":"r","operation":"work.complete"}"#,
        wire::REQUEST_FRAME_MAX_BYTES,
    )
    .unwrap();
    assert_eq!(
        wire::decode_routing_envelope(&wrong_version, wire::REQUEST_FRAME_MAX_BYTES),
        Err(wire::FrameError::UnsupportedProtocol(2))
    );
    let unknown = wire::encode_frame(
        br#"{"protocol_version":1,"request_id":"r","operation":"unknown"}"#,
        wire::REQUEST_FRAME_MAX_BYTES,
    )
    .unwrap();
    assert_eq!(
        wire::decode_routing_envelope(&unknown, wire::REQUEST_FRAME_MAX_BYTES),
        Err(wire::FrameError::UnknownOperation("unknown".to_owned()))
    );
}

#[test]
fn campaign_operator_frames_are_closed_and_exclude_daemon_resolved_pins() {
    let start = wire::OperatorStartCampaignRequest {
        protocol_version: wire::PROTOCOL_VERSION_V1,
        request_id: "campaign-start-1".to_owned(),
        operation: wire::OP_OPERATOR_START_CAMPAIGN.to_owned(),
        client_command_id: "campaign-command-1".to_owned(),
        expected_application_revision: 4,
        application_revision_id: 7,
        aggregate_budget_micro_usd: 250_000,
        deadline_unix_millis: 4_000_000_000_000,
        delivery_target: 2,
        principal: "operator".to_owned(),
    };
    let frame = wire::encode_json_frame(&start, wire::REQUEST_FRAME_MAX_BYTES).unwrap();
    assert_eq!(
        wire::decode_routing_envelope(&frame, wire::REQUEST_FRAME_MAX_BYTES)
            .unwrap()
            .operation,
        wire::OP_OPERATOR_START_CAMPAIGN
    );
    assert_eq!(
        wire::decode_operation_request::<wire::OperatorStartCampaignRequest>(
            &frame,
            wire::REQUEST_FRAME_MAX_BYTES,
            wire::OP_OPERATOR_START_CAMPAIGN,
        )
        .unwrap(),
        start
    );
    let start_json = json::to_string(&start);
    assert!(!start_json.contains("kernel_build_id"));
    assert!(!start_json.contains("repository_id"));

    let status = wire::OperatorCampaignStatusRequest {
        protocol_version: wire::PROTOCOL_VERSION_V1,
        request_id: "campaign-status-1".to_owned(),
        operation: wire::OP_OPERATOR_CAMPAIGN_STATUS.to_owned(),
        campaign_id: 9,
    };
    let status_frame = wire::encode_json_frame(&status, wire::REQUEST_FRAME_MAX_BYTES).unwrap();
    assert!(wire::decode_routing_envelope(&status_frame, wire::REQUEST_FRAME_MAX_BYTES).is_ok());
}

#[test]
fn operation_frames_reject_unknown_or_noncanonical_fields_before_dispatch() {
    let request = wire::OperatorCampaignStatusRequest {
        protocol_version: wire::PROTOCOL_VERSION_V1,
        request_id: "campaign-status-closed".to_owned(),
        operation: wire::OP_OPERATOR_CAMPAIGN_STATUS.to_owned(),
        campaign_id: 9,
    };
    let canonical = json::to_string(&request);
    let unknown = format!("{},\"ignored\":true}}", &canonical[..canonical.len() - 1]);
    let frame = wire::encode_frame(unknown.as_bytes(), wire::REQUEST_FRAME_MAX_BYTES).unwrap();
    assert!(matches!(
        wire::decode_operation_request::<wire::OperatorCampaignStatusRequest>(
            &frame,
            wire::REQUEST_FRAME_MAX_BYTES,
            wire::OP_OPERATOR_CAMPAIGN_STATUS,
        ),
        Err(wire::FrameError::InvalidJson { .. })
    ));

    let reordered = format!(
        "{{\"campaign_id\":9,\"protocol_version\":1,\"request_id\":\"campaign-status-closed\",\"operation\":\"{}\"}}",
        wire::OP_OPERATOR_CAMPAIGN_STATUS
    );
    let frame = wire::encode_frame(reordered.as_bytes(), wire::REQUEST_FRAME_MAX_BYTES).unwrap();
    assert!(matches!(
        wire::decode_operation_request::<wire::OperatorCampaignStatusRequest>(
            &frame,
            wire::REQUEST_FRAME_MAX_BYTES,
            wire::OP_OPERATOR_CAMPAIGN_STATUS,
        ),
        Err(wire::FrameError::InvalidJson { .. })
    ));
}

#[test]
fn operator_navigation_requests_are_closed_and_known() {
    let request = wire::OperatorTicketListRequest {
        protocol_version: wire::PROTOCOL_VERSION_V1,
        request_id: "ticket-list-1".to_owned(),
        operation: wire::OP_OPERATOR_LIST_TICKETS.to_owned(),
        state: Some("sponsored".to_owned()),
    };
    let frame = wire::encode_json_frame(&request, wire::REQUEST_FRAME_MAX_BYTES).unwrap();
    assert_eq!(
        wire::decode_routing_envelope(&frame, wire::REQUEST_FRAME_MAX_BYTES)
            .unwrap()
            .operation,
        wire::OP_OPERATOR_LIST_TICKETS
    );
    let audit = wire::OperatorAuditShowRequest {
        protocol_version: wire::PROTOCOL_VERSION_V1,
        request_id: "audit-show-1".to_owned(),
        operation: wire::OP_OPERATOR_SHOW_AUDIT.to_owned(),
        selector: "ticket:9".to_owned(),
    };
    let payload = json::to_string(&audit);
    assert!(!payload.contains("subject_kind"));
    assert!(!payload.contains("query"));
}

#[test]
fn institutional_navigation_has_one_closed_kind_and_a_kind_matched_cursor() {
    let request = wire::OperatorInstitutionalSearchRequest {
        protocol_version: wire::PROTOCOL_VERSION_V1,
        request_id: "institutional-search-1".to_owned(),
        operation: wire::OP_OPERATOR_INSTITUTIONAL_SEARCH.to_owned(),
        query: "typed records".to_owned(),
        kind: "rfc".to_owned(),
        project_id: Some(7),
        owner_office_id: Some(3),
        anchor: None,
        limit: 20,
        cursor: Some(wire::InstitutionalReferenceWireV1 {
            kind: "rfc".to_owned(),
            id: 9,
        }),
    };
    assert_eq!(request.validate(), Ok(()));
    let frame = wire::encode_json_frame(&request, wire::REQUEST_FRAME_MAX_BYTES).unwrap();
    assert_eq!(
        wire::decode_operation_request::<wire::OperatorInstitutionalSearchRequest>(
            &frame,
            wire::REQUEST_FRAME_MAX_BYTES,
            wire::OP_OPERATOR_INSTITUTIONAL_SEARCH,
        )
        .unwrap(),
        request
    );

    let mismatched_cursor = wire::OperatorInstitutionalSearchRequest {
        cursor: Some(wire::InstitutionalReferenceWireV1 {
            kind: "experiment".to_owned(),
            id: 9,
        }),
        ..request
    };
    assert!(mismatched_cursor.validate().is_err());
    let unknown_kind = wire::OperatorInstitutionalSearchRequest {
        kind: "anything".to_owned(),
        cursor: None,
        ..mismatched_cursor
    };
    assert!(unknown_kind.validate().is_err());
}

#[test]
fn canonical_bundle_parser_admits_closed_domain_values() {
    let template = |placeholder: Vec<String>| wire::TemplateWireV2 {
        source_path: "templates/system.md".to_owned(),
        digest: "0000000000000000000000000000000000000000000000000000000000000000".to_owned(),
        placeholders: placeholder,
        rendered_byte_limit: 4096,
    };
    let command = |name: &str| wire::CommandWireV2 {
        name: name.to_owned(),
        executable: wire::ExecutableWireV2 {
            approved_tool: Some("cargo".to_owned()),
            repository_path: None,
        },
        argv: vec!["test".to_owned()],
        working_directory: ".".to_owned(),
        environment: Vec::new(),
        timeout_millis: 1000,
        stdout_byte_limit: 4096,
        stderr_byte_limit: 4096,
        expected_exit_status: 0,
    };
    let model = || wire::ModelWireV2 {
        provider: "provider".to_owned(),
        model_id: "model".to_owned(),
        thinking_level: "high".to_owned(),
        context_token_limit: 1,
        output_token_limit: 1,
        price_input_micro_usd_per_million_tokens: 0,
        price_output_micro_usd_per_million_tokens: 0,
        price_cache_read_micro_usd_per_million_tokens: 0,
        price_cache_write_micro_usd_per_million_tokens: 0,
        capability_flags: vec![],
    };
    let limits = || wire::LimitsWireV2 {
        turn_limit: 1,
        wall_limit_millis: 1,
        output_byte_limit: 4096,
    };
    let assignment_role_profile = |assignment_role: &str, tool: &str| wire::AssignmentRoleWireV2 {
        assignment_role: assignment_role.to_owned(),
        system_template: template(vec!["ASSIGNMENT_ID".to_owned()]),
        assignment_template: template(vec!["ASSIGNMENT_ID".to_owned()]),
        policy: wire::PolicyWireV2 {
            source_path: format!("policies/{assignment_role}.luau"),
            digest: "0000000000000000000000000000000000000000000000000000000000000000".to_owned(),
            byte_limit: 4096,
            entrypoint: "factory_policy".to_owned(),
        },
        tools: vec!["workspace_read".to_owned(), tool.to_owned()],
        model: model(),
        limits: limits(),
    };
    let bundle = wire::ApplicationBundleWireV2 {
        format_version: 2,
        application_key: "example".to_owned(),
        predecessor_bundle: None,
        repository: wire::RepositoryWireV2 {
            repository_key: "product".to_owned(),
            canonical_local_path: "/workspace/product".to_owned(),
            default_branch: "main".to_owned(),
            delivery_mode: "local_fast_forward_only".to_owned(),
        },
        mission_template: wire::TemplateWireV2 {
            source_path: "templates/mission.md".to_owned(),
            digest: "0000000000000000000000000000000000000000000000000000000000000000".to_owned(),
            placeholders: Vec::new(),
            rendered_byte_limit: 4096,
        },
        assignment_role_profiles: vec![
            assignment_role_profile("product_research", "product_submit_ticket"),
            assignment_role_profile("engineering", "candidate_submit"),
            assignment_role_profile("quality", "quality_submit_review"),
        ],
        ticket_policy: wire::TicketPolicyWireV2 {
            low_water: 1,
            target: 2,
            maximum: 3,
            proposal_maximum: 1,
            ticket_bounds: wire::TicketBoundsWireV2 {
                narrative_byte_limit: 4096,
                acceptance_criteria_limit: 4,
                contract_read_limit: 4,
            },
        },
        required_reads: vec![wire::RequiredReadWireV2 {
            path: "AGENTS.md".to_owned(),
            reason: "contract".to_owned(),
        }],
        reproducer_profiles: vec![command("reproducer")],
        validation_profiles: wire::ValidationWireV2 {
            focused: vec![command("focused")],
            full: vec![command("full")],
        },
        git_policy: wire::GitWireV2 {
            forbidden_paths: vec![".git".to_owned()],
            delivery_mode: "local_fast_forward_only".to_owned(),
            provenance_trailers_required: true,
        },
        commit_message_policy: wire::CommitMessageWireV2 {
            subject_byte_limit: 72,
            body_byte_limit: 4096,
        },
    };
    let mut invalid = bundle.clone();
    invalid.reproducer_profiles[0].executable.repository_path = Some("tool".to_owned());
    assert!(matches!(
        wire::canonical_application_bundle_json_v2(&invalid),
        Err(wire::FrameError::InvalidJson { .. })
    ));
    let payload = wire::canonical_application_bundle_json_v2(&bundle).expect("canonical bundle");
    let admitted = wire::parse_application_bundle_v2(payload.as_bytes()).expect("closed bundle");
    assert_eq!(admitted.application_key.as_str(), "example");

    let mut xhigh_bundle = bundle.clone();
    xhigh_bundle.assignment_role_profiles[1]
        .model
        .thinking_level = "xhigh".to_owned();
    let xhigh_payload =
        wire::canonical_application_bundle_json_v2(&xhigh_bundle).expect("xhigh canonical bundle");
    let xhigh =
        wire::parse_application_bundle_v2(xhigh_payload.as_bytes()).expect("xhigh closed bundle");
    assert_eq!(
        xhigh.assignment_role_profiles[1].model.thinking_level,
        ThinkingLevelV2::XHigh
    );

    let mut reordered = payload.as_bytes().to_vec();
    reordered.insert(0, b' ');
    assert!(matches!(
        wire::parse_application_bundle_v2(&reordered),
        Err(wire::FrameError::InvalidJson { .. })
    ));
    let with_unknown = payload.trim_end_matches('}').to_owned() + ",\"unknown\":true}";
    assert!(matches!(
        wire::parse_application_bundle_v2(with_unknown.as_bytes()),
        Err(wire::FrameError::InvalidJson { .. })
    ));
}

fn fixture_field<'a>(root: &'a json::Value, section: &str, operation: &str) -> &'a json::Value {
    let json::Value::Object(root) = root else {
        panic!("fixture root is not an object")
    };
    let json::Value::Object(section) = root.get(section).expect("fixture section") else {
        panic!("fixture section is not an object")
    };
    section.get(operation).expect("fixture operation")
}

fn round_trip<T>(value: &json::Value)
where
    T: Deserialize + Serialize,
{
    let encoded = json::to_string(value);
    let decoded: T = json::from_str(&encoded).expect("typed fixture parse");
    let reencoded = json::to_string(&decoded);
    let _: json::Value = json::from_str(&reencoded).expect("typed fixture serialization");
}

#[test]
fn every_operation_golden_is_typed_parsed_and_serialized() {
    let root: json::Value = json::from_str(include_str!(
        "../../../tests/protocol-fixtures/operation-goldens.json"
    ))
    .expect("operation fixture JSON");
    let json::Value::Object(root_object) = &root else {
        panic!("fixture root")
    };
    let json::Value::Array(operations) = root_object.get("operations").expect("operations") else {
        panic!("operations is not an array")
    };
    for operation in operations {
        let json::Value::String(operation) = operation else {
            panic!("operation is not a string")
        };
        let request = fixture_field(&root, "requests", operation);
        match operation.as_str() {
            OP_WORKSPACE_READ => round_trip::<WorkspaceReadRequest>(request),
            OP_ARTIFACT_SEAL_WORKSPACE_FILE => {
                round_trip::<ArtifactSealWorkspaceFileRequest>(request);
            }
            OP_ARTIFACT_READ => round_trip::<ArtifactReadRequest>(request),
            OP_PRODUCT_SUBMIT_TICKET => round_trip::<ProductSubmitTicketRequest>(request),
            OP_CANDIDATE_CHECKPOINT_REGRESSION => {
                round_trip::<CandidateCheckpointRegressionRequest>(request);
            }
            OP_CANDIDATE_SUBMIT => round_trip::<CandidateSubmitRequest>(request),
            OP_QUALITY_RUN_FULL_SUITE => round_trip::<QualityRunFullSuiteRequest>(request),
            OP_QUALITY_SUBMIT_REVIEW => round_trip::<QualitySubmitReviewRequest>(request),
            OP_WORK_COMPLETE => round_trip::<WorkCompleteRequest>(request),
            OP_ARCHITECT_SPONSOR_TICKET_REVISION => {
                round_trip::<ArchitectSponsorTicketRevisionRequest>(request);
            }
            OP_ARCHITECT_RELEASE_TICKET_ATTEMPT => {
                round_trip::<ArchitectReleaseTicketAttemptRequest>(request);
            }
            OP_ARCHITECT_DECIDE_CANDIDATE => round_trip::<ArchitectDecideCandidateRequest>(request),
            OP_FACTORYD_STATUS => round_trip::<OperatorStatusRequest>(request),
            OP_OPERATOR_START_CAMPAIGN => round_trip::<OperatorStartCampaignRequest>(request),
            OP_OPERATOR_CAMPAIGN_STATUS => round_trip::<OperatorCampaignStatusRequest>(request),
            OP_OPERATOR_CANCEL_CAMPAIGN => round_trip::<OperatorCancelCampaignRequest>(request),
            OP_OPERATOR_SHOW_APPLICATION => round_trip::<OperatorApplicationShowRequest>(request),
            OP_OPERATOR_REGISTER_APPLICATION => {
                round_trip::<OperatorApplicationRegisterRequest>(request);
            }
            OP_OPERATOR_ACTIVATE_APPLICATION => {
                round_trip::<OperatorApplicationActivateRequest>(request);
            }
            OP_OPERATOR_SEAL_ARTIFACT => round_trip::<OperatorArtifactSealRequest>(request),
            OP_OPERATOR_LIST_TICKETS => round_trip::<OperatorTicketListRequest>(request),
            OP_OPERATOR_SHOW_TICKET => round_trip::<OperatorTicketShowRequest>(request),
            OP_OPERATOR_SHOW_CANDIDATE => round_trip::<OperatorCandidateShowRequest>(request),
            OP_OPERATOR_SHOW_AUDIT => round_trip::<OperatorAuditShowRequest>(request),
            OP_OPERATOR_INSTITUTIONAL_SEARCH => {
                round_trip::<OperatorInstitutionalSearchRequest>(request);
            }
            OP_OPERATOR_INSTITUTIONAL_SHOW => {
                round_trip::<OperatorInstitutionalShowRequest>(request);
            }
            OP_OPERATOR_PUBLICATION_CREATE => {
                round_trip::<OperatorPublicationCreateRequest>(request);
            }
            OP_PUBLICATION_CREATE => round_trip::<PublicationCreateRequest>(request),
            OP_SESSION_VERIFY_PACKET => round_trip::<SessionVerifyPacketRequest>(request),
            OP_SESSION_SEAL_ARTIFACT => round_trip::<SessionSealArtifactRequest>(request),
            OP_SESSION_SUBMIT_TERMINAL => round_trip::<SessionSubmitTerminalRequest>(request),
            OP_FORUM_LIST_TOPICS => round_trip::<ForumListTopicsRequestV1>(request),
            OP_FORUM_LIST_THREADS => round_trip::<ForumListThreadsRequestV1>(request),
            OP_FORUM_SEARCH => round_trip::<ForumSearchRequestV1>(request),
            OP_FORUM_READ_THREAD => round_trip::<ForumReadThreadRequestV1>(request),
            other => panic!("unknown fixture operation {other}"),
        }

        let success = fixture_field(&root, "success", operation);
        match operation.as_str() {
            OP_WORKSPACE_READ => round_trip::<WorkspaceReadResponse>(success),
            OP_ARTIFACT_SEAL_WORKSPACE_FILE => round_trip::<ArtifactReceiptResponse>(success),
            OP_ARTIFACT_READ => round_trip::<ArtifactReadResponse>(success),
            OP_CANDIDATE_CHECKPOINT_REGRESSION => {
                round_trip::<RegressionCheckpointReceiptResponse>(success);
            }
            OP_CANDIDATE_SUBMIT => round_trip::<CandidateReceiptResponse>(success),
            OP_QUALITY_RUN_FULL_SUITE => round_trip::<QualityValidationReceiptResponse>(success),
            OP_QUALITY_SUBMIT_REVIEW => round_trip::<QualityReviewReceiptResponse>(success),
            OP_ARCHITECT_SPONSOR_TICKET_REVISION
            | OP_ARCHITECT_RELEASE_TICKET_ATTEMPT
            | OP_ARCHITECT_DECIDE_CANDIDATE => {
                round_trip::<ArchitectDecisionReceiptResponse>(success);
            }
            OP_FACTORYD_STATUS => round_trip::<OperatorStatusResponse>(success),
            OP_OPERATOR_START_CAMPAIGN | OP_OPERATOR_CANCEL_CAMPAIGN => {
                round_trip::<CampaignReceiptResponse>(success);
            }
            OP_OPERATOR_CAMPAIGN_STATUS => round_trip::<CampaignStatusResponse>(success),
            OP_OPERATOR_SHOW_APPLICATION => round_trip::<ApplicationShowResponse>(success),
            OP_OPERATOR_REGISTER_APPLICATION | OP_OPERATOR_ACTIVATE_APPLICATION => {
                round_trip::<ApplicationRevisionReceiptResponse>(success);
            }
            OP_OPERATOR_SEAL_ARTIFACT => round_trip::<OperatorArtifactSealReceiptResponse>(success),
            OP_OPERATOR_LIST_TICKETS => round_trip::<TicketListResponse>(success),
            OP_OPERATOR_SHOW_TICKET => round_trip::<TicketShowResponse>(success),
            OP_OPERATOR_SHOW_CANDIDATE => round_trip::<CandidateShowResponse>(success),
            OP_OPERATOR_SHOW_AUDIT => round_trip::<AuditShowResponse>(success),
            OP_OPERATOR_INSTITUTIONAL_SEARCH => {
                round_trip::<InstitutionalSearchResponse>(success);
            }
            OP_OPERATOR_INSTITUTIONAL_SHOW => round_trip::<InstitutionalShowResponse>(success),
            OP_OPERATOR_PUBLICATION_CREATE | OP_PUBLICATION_CREATE => {
                round_trip::<PublicationReceiptResponse>(success);
            }
            OP_SESSION_VERIFY_PACKET => round_trip::<SessionPacketVerificationResponse>(success),
            OP_SESSION_SEAL_ARTIFACT => round_trip::<ArtifactReceiptResponse>(success),
            OP_FORUM_LIST_TOPICS => round_trip::<ForumTopicsResponseV1>(success),
            OP_FORUM_LIST_THREADS => round_trip::<ForumThreadsResponseV1>(success),
            OP_FORUM_SEARCH => round_trip::<ForumSearchResponseV1>(success),
            OP_FORUM_READ_THREAD => round_trip::<ForumPostsResponseV1>(success),
            _ => round_trip::<OperationReceiptResponse>(success),
        }
        round_trip::<ConflictResponse>(fixture_field(&root, "conflict", operation));
        round_trip::<ErrorResponse>(fixture_field(&root, "error", operation));
    }
}

#[test]
fn quality_and_architect_goldens_convert_to_closed_contracts() {
    let root: json::Value = json::from_str(include_str!(
        "../../../tests/protocol-fixtures/operation-goldens.json"
    ))
    .expect("operation fixture JSON");

    let quality: QualitySubmitReviewRequest = json::from_str(&json::to_string(fixture_field(
        &root,
        "requests",
        OP_QUALITY_SUBMIT_REVIEW,
    )))
    .expect("Quality request");
    assert_eq!(quality.submission().unwrap().verdict, ReviewVerdict::Accept);

    let architect: ArchitectDecideCandidateRequest = json::from_str(&json::to_string(
        fixture_field(&root, "requests", OP_ARCHITECT_DECIDE_CANDIDATE),
    ))
    .expect("Architect request");
    assert_eq!(
        architect.decision().unwrap().decision,
        CandidateDecisionV1::Deliver
    );
}

#[test]
fn product_ticket_golden_converts_to_the_closed_reproducer_contract() {
    let root: json::Value = json::from_str(include_str!(
        "../../../tests/protocol-fixtures/operation-goldens.json"
    ))
    .expect("operation fixture JSON");
    let request: ProductSubmitTicketRequest = json::from_str(&json::to_string(fixture_field(
        &root,
        "requests",
        OP_PRODUCT_SUBMIT_TICKET,
    )))
    .expect("Product request");
    let bounds = TicketBoundsV2 {
        narrative_byte_limit: 100,
        acceptance_criteria_limit: 1,
        contract_read_limit: 1,
    };
    assert!(request.proposal(&bounds).is_ok());

    let mut invalid_duplicate = request.clone();
    invalid_duplicate.duplicate_search.limit = 21;
    assert!(invalid_duplicate.proposal(&bounds).is_err());

    let mut nonreproducible = request;
    nonreproducible.reproducer.second_observation.exit_status = 2;
    assert!(nonreproducible.proposal(&bounds).is_err());
}
