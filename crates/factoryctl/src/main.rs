//! Grand Architect CLI for the local Factory daemon.
//!
//! `factoryctl` opens no PostgreSQL connection. Its sole database argument is
//! the bootstrap URL it forwards unchanged to one explicit `factoryd init`
//! child. Status, campaign control, and the explicit Architect decision
//! families travel over the daemon-created mode-`0600` operator socket. Pi
//! actors receive neither this CLI surface nor a reconnectable operator
//! listener.

use std::{
    env,
    ffi::OsString,
    fs, io,
    path::{Path, PathBuf},
    process::{Command, ExitCode},
};

use factory_kernel::local_transport::OperatorClient;
use factory_protocol::{
    ApplicationRevisionReceiptResponse, ApplicationShowResponse, ArchitectDecideCandidateRequest,
    ArchitectDecisionReceiptResponse, ArchitectReleaseTicketAttemptRequest,
    ArchitectSponsorTicketRevisionRequest, AuditShowResponse, CampaignReceiptResponse,
    CampaignStatusResponse, CandidateShowResponse, CredentialDescriptorV1, ForumAttachmentWireV1,
    ForumCreateThreadRequestV1, ForumCreateTopicRequestV1, ForumListThreadsRequestV1,
    ForumListTopicsRequestV1, ForumPostRequestV1, ForumPostsResponseV1, ForumReadThreadRequestV1,
    ForumSearchRequestV1, ForumSearchResponseV1, ForumThreadsResponseV1, ForumTopicsResponseV1,
    OperationReceiptResponse, OperatorApplicationActivateRequest,
    OperatorApplicationRegisterRequest, OperatorApplicationShowRequest,
    OperatorArtifactSealReceiptResponse, OperatorArtifactSealRequest, OperatorAuditShowRequest,
    OperatorCampaignStatusRequest, OperatorCancelCampaignRequest, OperatorCandidateShowRequest,
    OperatorStartCampaignRequest, OperatorTicketListRequest, OperatorTicketShowRequest,
    PROTOCOL_VERSION_V1, RuntimeRelativePath, SealedArtifactReferenceWireV1, TicketListResponse,
    TicketShowResponse,
};

fn main() -> ExitCode {
    tracing_subscriber::fmt().with_target(false).init();
    match parse_args(env::args().skip(1).collect()) {
        Ok(command) => match smol::block_on(run(command)) {
            Ok(()) => ExitCode::SUCCESS,
            Err(error) => {
                eprintln!("factoryctl: {error}");
                ExitCode::FAILURE
            }
        },
        Err(error) => {
            eprintln!("factoryctl: {error}\n{}", usage());
            ExitCode::FAILURE
        }
    }
}

async fn run(command: CliCommand) -> Result<(), Box<dyn std::error::Error>> {
    match command {
        CliCommand::Init(command) => {
            smol::unblock(move || spawn_factoryd_init(&command)).await?;
        }
        CliCommand::DaemonStatus(connection) => {
            let status = OperatorClient::new(connection.socket_path)
                .probe(status_request_id())
                .await?;
            if connection.json {
                println!(
                    "{{\"protocol_version\":{},\"request_id\":\"{}\",\"operation\":\"{}\",\"state\":\"{}\"}}",
                    status.protocol_version, status.request_id, status.operation, status.state
                );
            } else {
                println!("daemon: {}", status.state);
            }
        }
        CliCommand::Sponsor {
            base,
            ticket_revision_id,
        } => {
            let connection = base.connection.clone();
            let receipt = OperatorClient::new(connection.socket_path)
                .sponsor_ticket_revision(ArchitectSponsorTicketRevisionRequest {
                    protocol_version: PROTOCOL_VERSION_V1,
                    request_id: architect_request_id("sponsor"),
                    operation: "architect.sponsor_ticket_revision".to_owned(),
                    client_command_id: base.client_command_id,
                    expected_revision: base.expected_revision,
                    ticket_revision_id,
                    rationale: base.rationale,
                    principal: base.principal,
                })
                .await?;
            print_decision(&receipt, connection.json);
        }
        CliCommand::Release {
            base,
            ticket_attempt_id,
        } => {
            let connection = base.connection.clone();
            let receipt = OperatorClient::new(connection.socket_path)
                .release_ticket_attempt(ArchitectReleaseTicketAttemptRequest {
                    protocol_version: PROTOCOL_VERSION_V1,
                    request_id: architect_request_id("release"),
                    operation: "architect.release_ticket_attempt".to_owned(),
                    client_command_id: base.client_command_id,
                    expected_revision: base.expected_revision,
                    ticket_attempt_id,
                    rationale: base.rationale,
                    principal: base.principal,
                })
                .await?;
            print_decision(&receipt, connection.json);
        }
        CliCommand::Decide {
            base,
            candidate_id,
            review_id,
            decision,
            quality_override_review_id,
        } => {
            let connection = base.connection.clone();
            let receipt = OperatorClient::new(connection.socket_path)
                .decide_candidate(ArchitectDecideCandidateRequest {
                    protocol_version: PROTOCOL_VERSION_V1,
                    request_id: architect_request_id("decide"),
                    operation: "architect.decide_candidate".to_owned(),
                    client_command_id: base.client_command_id,
                    expected_revision: base.expected_revision,
                    candidate_id,
                    review_id,
                    decision: decision.as_str().to_owned(),
                    rationale: base.rationale,
                    quality_rejection_override_review_id: quality_override_review_id,
                    principal: base.principal,
                })
                .await?;
            print_decision(&receipt, connection.json);
        }
        CliCommand::CampaignStart(command) => {
            let connection = command.connection.clone();
            let receipt = OperatorClient::new(connection.socket_path)
                .start_campaign(OperatorStartCampaignRequest {
                    protocol_version: PROTOCOL_VERSION_V1,
                    request_id: campaign_request_id("start"),
                    operation: "operator.campaign.start".to_owned(),
                    client_command_id: command.client_command_id,
                    expected_application_revision: command.expected_application_revision,
                    application_revision_id: command.application_revision_id,
                    aggregate_budget_micro_usd: command.aggregate_budget_micro_usd,
                    deadline_unix_millis: command.deadline_unix_millis,
                    delivery_target: command.delivery_target,
                    principal: command.principal,
                })
                .await?;
            print_campaign_receipt(&receipt, connection.json);
        }
        CliCommand::CampaignStatus {
            connection,
            campaign_id,
        } => {
            let status = OperatorClient::new(connection.socket_path)
                .campaign_status(OperatorCampaignStatusRequest {
                    protocol_version: PROTOCOL_VERSION_V1,
                    request_id: campaign_request_id("status"),
                    operation: "operator.campaign.status".to_owned(),
                    campaign_id,
                })
                .await?;
            print_campaign_status(&status, connection.json);
        }
        CliCommand::CampaignCancel(command) => {
            let connection = command.connection.clone();
            let receipt = OperatorClient::new(connection.socket_path)
                .cancel_campaign(OperatorCancelCampaignRequest {
                    protocol_version: PROTOCOL_VERSION_V1,
                    request_id: campaign_request_id("cancel"),
                    operation: "operator.campaign.cancel".to_owned(),
                    client_command_id: command.client_command_id,
                    expected_revision: command.expected_revision,
                    campaign_id: command.campaign_id,
                    principal: command.principal,
                })
                .await?;
            print_campaign_receipt(&receipt, connection.json);
        }
        CliCommand::ApplicationShow(command) => {
            let connection = command.connection.clone();
            let status = OperatorClient::new(connection.socket_path)
                .show_application(OperatorApplicationShowRequest {
                    protocol_version: PROTOCOL_VERSION_V1,
                    request_id: application_request_id("show"),
                    operation: "operator.application.show".to_owned(),
                    application_key: command.application_key,
                    application_revision_id: command.application_revision_id,
                })
                .await?;
            print_application_show(&status, connection.json);
        }
        CliCommand::ApplicationRegister(command) => {
            let connection = command.connection.clone();
            let receipt = OperatorClient::new(connection.socket_path)
                .register_application(OperatorApplicationRegisterRequest {
                    protocol_version: PROTOCOL_VERSION_V1,
                    request_id: application_request_id("register"),
                    operation: "operator.application.register".to_owned(),
                    client_command_id: command.client_command_id,
                    expected_revision: command.expected_revision,
                    expected_kernel_build_revision: command.expected_kernel_build_revision,
                    kernel_build_id: command.kernel_build_id,
                    source_root: command.source_root.to_string_lossy().into_owned(),
                    bundle_relative_path: command.bundle_relative_path,
                    principal: command.principal,
                })
                .await?;
            print_application_receipt(&receipt, connection.json);
        }
        CliCommand::ApplicationActivate(command) => {
            let connection = command.base.connection.clone();
            let receipt = OperatorClient::new(connection.socket_path)
                .activate_application(OperatorApplicationActivateRequest {
                    protocol_version: PROTOCOL_VERSION_V1,
                    request_id: application_request_id("activate"),
                    operation: "operator.application.activate".to_owned(),
                    client_command_id: command.base.client_command_id,
                    expected_revision: command.base.expected_revision,
                    application_key: command.application_key,
                    application_revision_id: command.application_revision_id,
                    rationale: command.base.rationale,
                    principal: command.base.principal,
                })
                .await?;
            print_application_receipt(&receipt, connection.json);
        }
        CliCommand::OperatorArtifactSeal(command) => {
            let connection = command.connection.clone();
            let receipt = OperatorClient::new(connection.socket_path)
                .seal_operator_artifact(OperatorArtifactSealRequest {
                    protocol_version: PROTOCOL_VERSION_V1,
                    request_id: operator_artifact_request_id(),
                    operation: "operator.artifact.seal".to_owned(),
                    client_command_id: command.client_command_id,
                    expected_kernel_build_revision: command.expected_kernel_build_revision,
                    source_root: command.source_root.to_string_lossy().into_owned(),
                    source_relative_path: command.source_relative_path,
                    principal: command.principal,
                })
                .await?;
            print_operator_artifact_receipt(&receipt, connection.json);
        }
        CliCommand::TicketList(command) => {
            let connection = command.connection.clone();
            let response = OperatorClient::new(connection.socket_path)
                .list_tickets(OperatorTicketListRequest {
                    protocol_version: PROTOCOL_VERSION_V1,
                    request_id: navigation_request_id("ticket-list"),
                    operation: "operator.ticket.list".to_owned(),
                    state: command.state,
                })
                .await?;
            print_ticket_list(&response, connection.json);
        }
        CliCommand::TicketShow {
            connection,
            ticket_id,
        } => {
            let response = OperatorClient::new(connection.socket_path.clone())
                .show_ticket(OperatorTicketShowRequest {
                    protocol_version: PROTOCOL_VERSION_V1,
                    request_id: navigation_request_id("ticket-show"),
                    operation: "operator.ticket.show".to_owned(),
                    ticket_id,
                })
                .await?;
            print_ticket_show(&response, connection.json);
        }
        CliCommand::CandidateShow {
            connection,
            candidate_id,
        } => {
            let response = OperatorClient::new(connection.socket_path.clone())
                .show_candidate(OperatorCandidateShowRequest {
                    protocol_version: PROTOCOL_VERSION_V1,
                    request_id: navigation_request_id("candidate-show"),
                    operation: "operator.candidate.show".to_owned(),
                    candidate_id,
                })
                .await?;
            print_candidate_show(&response, connection.json);
        }
        CliCommand::AuditShow {
            connection,
            selector,
        } => {
            let response = OperatorClient::new(connection.socket_path.clone())
                .show_audit(OperatorAuditShowRequest {
                    protocol_version: PROTOCOL_VERSION_V1,
                    request_id: navigation_request_id("audit-show"),
                    operation: "operator.audit.show".to_owned(),
                    selector,
                })
                .await?;
            print_audit_show(&response, connection.json);
        }
        CliCommand::ForumTopics(base) => {
            let connection = base.connection.clone();
            let response = OperatorClient::new(connection.socket_path)
                .forum_list_topics(ForumListTopicsRequestV1 {
                    protocol_version: PROTOCOL_VERSION_V1,
                    request_id: forum_request_id("topics"),
                    operation: "forum.list_topics".to_owned(),
                    cursor: base.cursor,
                    limit: base.limit,
                })
                .await?;
            print_forum_topics(&response, connection.json);
        }
        CliCommand::ForumThreads { base, topic_id } => {
            let connection = base.connection.clone();
            let response = OperatorClient::new(connection.socket_path)
                .forum_list_threads(ForumListThreadsRequestV1 {
                    protocol_version: PROTOCOL_VERSION_V1,
                    request_id: forum_request_id("threads"),
                    operation: "forum.list_threads".to_owned(),
                    topic_id,
                    cursor: base.cursor,
                    limit: base.limit,
                })
                .await?;
            print_forum_threads(&response, connection.json);
        }
        CliCommand::ForumRead {
            base,
            thread_id,
            after_post_id,
        } => {
            let connection = base.connection.clone();
            let response = OperatorClient::new(connection.socket_path)
                .forum_read_thread(ForumReadThreadRequestV1 {
                    protocol_version: PROTOCOL_VERSION_V1,
                    request_id: forum_request_id("read"),
                    operation: "forum.read_thread".to_owned(),
                    thread_id,
                    after_post_id,
                    limit: base.limit,
                })
                .await?;
            print_forum_posts(&response, connection.json);
        }
        CliCommand::ForumSearch { base, query } => {
            let connection = base.connection.clone();
            let response = OperatorClient::new(connection.socket_path)
                .forum_search(ForumSearchRequestV1 {
                    protocol_version: PROTOCOL_VERSION_V1,
                    request_id: forum_request_id("search"),
                    operation: "forum.search".to_owned(),
                    query,
                    topic_id: None,
                    thread_id: None,
                    author_office: None,
                    post_kind: None,
                    created_after_micros: None,
                    created_before_micros: None,
                    cursor: base.cursor,
                    limit: base.limit,
                })
                .await?;
            print_forum_search(&response, connection.json);
        }
        CliCommand::ForumCreateTopic {
            base,
            name,
            description,
        } => {
            let connection = base.connection.clone();
            let receipt = OperatorClient::new(connection.socket_path)
                .forum_create_topic(ForumCreateTopicRequestV1 {
                    protocol_version: PROTOCOL_VERSION_V1,
                    request_id: forum_request_id("create-topic"),
                    operation: "forum.create_topic".to_owned(),
                    client_command_id: base.client_command_id,
                    expected_revision: base.expected_revision,
                    name,
                    description,
                })
                .await?;
            print_forum_receipt(&receipt, connection.json);
        }
        CliCommand::ForumCreateThread {
            base,
            topic_id,
            title,
        } => {
            let connection = base.connection.clone();
            let receipt = OperatorClient::new(connection.socket_path)
                .forum_create_thread(ForumCreateThreadRequestV1 {
                    protocol_version: PROTOCOL_VERSION_V1,
                    request_id: forum_request_id("create-thread"),
                    operation: "forum.create_thread".to_owned(),
                    client_command_id: base.client_command_id,
                    expected_revision: base.expected_revision,
                    topic_id,
                    title,
                })
                .await?;
            print_forum_receipt(&receipt, connection.json);
        }
        CliCommand::ForumPost {
            base,
            thread_id,
            kind,
            body,
            reply_to,
            supersedes,
            attachments,
        } => {
            let connection = base.connection.clone();
            let receipt = OperatorClient::new(connection.socket_path)
                .forum_post(ForumPostRequestV1 {
                    protocol_version: PROTOCOL_VERSION_V1,
                    request_id: forum_request_id("post"),
                    operation: "forum.post".to_owned(),
                    client_command_id: base.client_command_id,
                    expected_revision: base.expected_revision,
                    thread_id,
                    kind,
                    body,
                    reply_to,
                    supersedes,
                    attachments,
                })
                .await?;
            print_forum_receipt(&receipt, connection.json);
        }
    }
    Ok(())
}

fn print_decision(receipt: &ArchitectDecisionReceiptResponse, json: bool) {
    if json {
        println!(
            "{{\"protocol_version\":{},\"request_id\":\"{}\",\"operation\":\"{}\",\"audit_id\":{},\"aggregate_revision\":{},\"architect_decision_id\":{},\"decision_kind\":\"{}\"}}",
            receipt.protocol_version,
            receipt.request_id,
            receipt.operation,
            receipt.audit_id,
            receipt.aggregate_revision,
            receipt.architect_decision_id,
            receipt.decision_kind,
        );
    } else {
        println!(
            "architect decision {}: #{} (revision {}, audit #{})",
            receipt.decision_kind,
            receipt.architect_decision_id,
            receipt.aggregate_revision,
            receipt.audit_id,
        );
    }
}

fn print_campaign_receipt(receipt: &CampaignReceiptResponse, json: bool) {
    if json {
        println!(
            "{{\"protocol_version\":{},\"request_id\":\"{}\",\"operation\":\"{}\",\"audit_id\":{},\"aggregate_revision\":{},\"campaign_id\":{},\"kernel_build_id\":\"{}\",\"application_revision_id\":{},\"repository_id\":{},\"was_idempotent_retry\":{}}}",
            receipt.protocol_version,
            receipt.request_id,
            receipt.operation,
            receipt.audit_id,
            receipt.aggregate_revision,
            receipt.campaign_id,
            receipt.kernel_build_id,
            receipt.application_revision_id,
            receipt.repository_id,
            receipt.was_idempotent_retry,
        );
    } else {
        println!(
            "campaign #{} (revision {}, audit #{})\n  build: {}\n  application revision: {}\n  repository: {}{}",
            receipt.campaign_id,
            receipt.aggregate_revision,
            receipt.audit_id,
            receipt.kernel_build_id,
            receipt.application_revision_id,
            receipt.repository_id,
            if receipt.was_idempotent_retry {
                "\n  replay: idempotent"
            } else {
                ""
            },
        );
    }
}

fn print_campaign_status(status: &CampaignStatusResponse, json: bool) {
    if json {
        println!(
            "{{\"protocol_version\":{},\"request_id\":\"{}\",\"operation\":\"{}\",\"campaign_id\":{},\"state\":\"{}\",\"aggregate_revision\":{},\"kernel_build_id\":\"{}\",\"application_revision_id\":{},\"repository_id\":{},\"aggregate_budget_micro_usd\":{},\"measured_cost_state\":\"{}\",\"measured_cost_micro_usd\":{},\"remaining_budget_micro_usd\":{},\"deadline_unix_millis\":{},\"delivery_target\":{},\"delivered_attempt_count\":{},\"ready_ticket_count\":{},\"proposed_ticket_count\":{},\"in_flight_ticket_count\":{},\"downstream_ticket_attempt_count\":{},\"downstream_action_stage\":{},\"downstream_ticket_attempt_id\":{},\"downstream_ticket_attempt_revision\":{},\"downstream_candidate_id\":{},\"downstream_candidate_revision\":{},\"ready_low_water\":{},\"ready_target\":{},\"ready_maximum\":{},\"proposal_maximum\":{},\"oldest_sponsored_ticket_revision_id\":{},\"oldest_sponsored_ticket_revision\":{},\"scheduler_next_action\":\"{}\",\"scheduler_constraint\":{},\"session_costs\":{}}}",
            status.protocol_version,
            status.request_id,
            status.operation,
            status.campaign_id,
            status.state,
            status.aggregate_revision,
            status.kernel_build_id,
            status.application_revision_id,
            status.repository_id,
            status.aggregate_budget_micro_usd,
            status.measured_cost_state,
            optional_u64(status.measured_cost_micro_usd),
            optional_u64(status.remaining_budget_micro_usd),
            status.deadline_unix_millis,
            status.delivery_target,
            status.delivered_attempt_count,
            status.ready_ticket_count,
            status.proposed_ticket_count,
            status.in_flight_ticket_count,
            status.downstream_ticket_attempt_count,
            optional_json_string(status.downstream_action_stage.as_deref()),
            optional_i64(status.downstream_ticket_attempt_id),
            optional_u64(status.downstream_ticket_attempt_revision),
            optional_i64(status.downstream_candidate_id),
            optional_u64(status.downstream_candidate_revision),
            status.ready_low_water,
            status.ready_target,
            status.ready_maximum,
            status.proposal_maximum,
            optional_i64(status.oldest_sponsored_ticket_revision_id),
            optional_u64(status.oldest_sponsored_ticket_revision),
            status.scheduler_next_action,
            optional_json_string(status.scheduler_constraint.as_deref()),
            session_costs_json(&status.session_costs),
        );
    } else {
        println!(
            "campaign #{}: {} (revision {})\n  build: {}\n  application revision: {}; repository: {}\n  budget: {} μUSD; cost: {}{}\n  tickets: ready {}, proposed {}, in flight {}, downstream {}; delivered {}/{}{}\n  scheduler: {}{}{}",
            status.campaign_id,
            status.state,
            status.aggregate_revision,
            status.kernel_build_id,
            status.application_revision_id,
            status.repository_id,
            status.aggregate_budget_micro_usd,
            status.measured_cost_state,
            status
                .remaining_budget_micro_usd
                .map(|value| format!("; remaining {value} μUSD"))
                .unwrap_or_default(),
            status.ready_ticket_count,
            status.proposed_ticket_count,
            status.in_flight_ticket_count,
            status.downstream_ticket_attempt_count,
            status.delivered_attempt_count,
            status.delivery_target,
            status
                .downstream_action_stage
                .as_deref()
                .map(|stage| {
                    format!(
                        "\n  downstream head: {stage}; attempt #{} rev {}; candidate #{} rev {}",
                        status
                            .downstream_ticket_attempt_id
                            .map_or_else(|| "?".to_owned(), |id| id.to_string()),
                        status
                            .downstream_ticket_attempt_revision
                            .map_or_else(|| "?".to_owned(), |revision| revision.to_string()),
                        status
                            .downstream_candidate_id
                            .map_or_else(|| "?".to_owned(), |id| id.to_string()),
                        status
                            .downstream_candidate_revision
                            .map_or_else(|| "?".to_owned(), |revision| revision.to_string()),
                    )
                })
                .unwrap_or_default(),
            status.scheduler_next_action,
            status
                .scheduler_constraint
                .as_deref()
                .map(|value| format!(" ({value})"))
                .unwrap_or_default(),
            session_costs_text(&status.session_costs),
        );
    }
}

fn session_costs_json(rows: &[factory_protocol::CampaignSessionCostResponse]) -> String {
    let mut output = String::from("[");
    for (index, row) in rows.iter().enumerate() {
        if index != 0 {
            output.push(',');
        }
        output.push_str(&format!(
            "{{\"session_id\":{},\"assignment_id\":{},\"office\":\"{}\",\"model_provider\":\"{}\",\"model_id\":\"{}\",\"outcome\":\"{}\",\"cost_state\":\"{}\",\"cost_micro_usd\":{},\"elapsed_millis\":{}}}",
            row.session_id,
            row.assignment_id,
            row.office,
            row.model_provider,
            row.model_id,
            row.outcome,
            row.cost_state,
            optional_u64(row.cost_micro_usd),
            optional_u64(row.elapsed_millis),
        ));
    }
    output.push(']');
    output
}

fn session_costs_text(rows: &[factory_protocol::CampaignSessionCostResponse]) -> String {
    if rows.is_empty() {
        return "\n  sessions: none".to_owned();
    }
    let mut output = String::from("\n  sessions:");
    for row in rows {
        output.push_str(&format!(
            "\n    #{} assignment #{}: {} / {}/{} ({}, {}; cost {}{})",
            row.session_id,
            row.assignment_id,
            row.office,
            row.model_provider,
            row.model_id,
            row.outcome,
            row.cost_state,
            row.cost_micro_usd
                .map_or_else(|| "unknown".to_owned(), |cost| format!("{cost} μUSD")),
            row.elapsed_millis
                .map_or_else(|| String::new(), |elapsed| format!("; elapsed {elapsed} ms")),
        ));
    }
    output
}

fn print_application_show(status: &ApplicationShowResponse, json: bool) {
    if json {
        println!(
            "{{\"protocol_version\":{},\"request_id\":\"{}\",\"operation\":\"{}\",\"application_key\":\"{}\",\"application_revision_id\":{},\"aggregate_revision\":{},\"bundle_artifact_id\":{},\"is_active\":{}}}",
            status.protocol_version,
            status.request_id,
            status.operation,
            status.application_key,
            status.application_revision_id,
            status.aggregate_revision,
            status.bundle_artifact_id,
            status.is_active,
        );
    } else {
        println!(
            "application {} revision #{} (aggregate revision {})\n  bundle artifact: {}\n  active: {}",
            status.application_key,
            status.application_revision_id,
            status.aggregate_revision,
            status.bundle_artifact_id,
            status.is_active,
        );
    }
}

fn print_application_receipt(receipt: &ApplicationRevisionReceiptResponse, json: bool) {
    if json {
        println!(
            "{{\"protocol_version\":{},\"request_id\":\"{}\",\"operation\":\"{}\",\"audit_id\":{},\"aggregate_revision\":{},\"application_revision_id\":{},\"is_active\":{},\"was_idempotent_retry\":{}}}",
            receipt.protocol_version,
            receipt.request_id,
            receipt.operation,
            receipt.audit_id,
            receipt.aggregate_revision,
            receipt.application_revision_id,
            receipt.is_active,
            receipt.was_idempotent_retry,
        );
    } else {
        println!(
            "application revision #{} (aggregate revision {}, audit #{})\n  active: {}{}",
            receipt.application_revision_id,
            receipt.aggregate_revision,
            receipt.audit_id,
            receipt.is_active,
            if receipt.was_idempotent_retry {
                "\n  replay: idempotent"
            } else {
                ""
            },
        );
    }
}

fn print_operator_artifact_receipt(receipt: &OperatorArtifactSealReceiptResponse, json: bool) {
    if json {
        println!(
            "{{\"protocol_version\":{},\"request_id\":\"{}\",\"operation\":\"{}\",\"audit_id\":{},\"aggregate_revision\":{},\"artifact_id\":{},\"digest\":\"{}\",\"byte_length\":{},\"was_idempotent_retry\":{},\"was_reused\":{}}}",
            receipt.protocol_version,
            receipt.request_id,
            receipt.operation,
            receipt.audit_id,
            receipt.aggregate_revision,
            receipt.artifact_id,
            receipt.digest,
            receipt.byte_length,
            receipt.was_idempotent_retry,
            receipt.was_reused,
        );
    } else {
        println!(
            "artifact #{} ({} bytes, audit #{}, kernel build revision {})\n  digest: {}{}{}",
            receipt.artifact_id,
            receipt.byte_length,
            receipt.audit_id,
            receipt.aggregate_revision,
            receipt.digest,
            if receipt.was_idempotent_retry {
                "\n  replay: idempotent"
            } else {
                ""
            },
            if receipt.was_reused {
                "\n  sealed object reused"
            } else {
                ""
            },
        );
    }
}

fn print_ticket_list(response: &TicketListResponse, json: bool) {
    if json {
        print!(
            "{{\"protocol_version\":{},\"request_id\":\"{}\",\"operation\":\"{}\",\"items\":[",
            response.protocol_version, response.request_id, response.operation
        );
        for (index, item) in response.items.iter().enumerate() {
            if index != 0 {
                print!(",");
            }
            print!(
                "{{\"ticket_id\":{},\"ticket_revision_id\":{},\"ticket_revision\":{},\"application_revision_id\":{},\"state\":\"{}\",\"proposal_artifact_id\":{},\"created_at_micros\":{}}}",
                item.ticket_id,
                item.ticket_revision_id,
                item.ticket_revision,
                item.application_revision_id,
                item.state,
                item.proposal_artifact_id,
                item.created_at_micros
            );
        }
        println!("]}}");
    } else if response.items.is_empty() {
        println!("tickets: none");
    } else {
        for item in &response.items {
            println!(
                "ticket #{} revision #{} (revision {}): {}\n  application revision: {}; proposal artifact: {}",
                item.ticket_id,
                item.ticket_revision_id,
                item.ticket_revision,
                item.state,
                item.application_revision_id,
                item.proposal_artifact_id
            );
        }
    }
}

fn print_ticket_show(response: &TicketShowResponse, json: bool) {
    if json {
        print!(
            "{{\"protocol_version\":{},\"request_id\":\"{}\",\"operation\":\"{}\",\"ticket_id\":{},\"ticket_revision_id\":{},\"ticket_revision\":{},\"application_revision_id\":{},\"state\":\"{}\",\"sponsorship_reason\":{},\"blocked_reason\":{},\"evidence\":[",
            response.protocol_version,
            response.request_id,
            response.operation,
            response.ticket_id,
            response.ticket_revision_id,
            response.ticket_revision,
            response.application_revision_id,
            response.state,
            optional_json_string(response.sponsorship_reason.as_deref()),
            optional_json_string(response.blocked_reason.as_deref())
        );
        for (index, evidence) in response.evidence.iter().enumerate() {
            if index != 0 {
                print!(",");
            }
            print!(
                "{{\"role\":\"{}\",\"artifact_id\":{},\"digest\":\"{}\",\"byte_length\":{}}}",
                evidence.role, evidence.artifact_id, evidence.digest, evidence.byte_length
            );
        }
        print!("],\"attempts\":[");
        for (index, attempt) in response.attempts.iter().enumerate() {
            if index != 0 {
                print!(",");
            }
            print!(
                "{{\"ticket_attempt_id\":{},\"attempt_revision\":{},\"campaign_id\":{},\"stage\":\"{}\",\"candidate_id\":{}}}",
                attempt.ticket_attempt_id,
                attempt.attempt_revision,
                attempt.campaign_id,
                attempt.stage,
                optional_i64(attempt.candidate_id)
            );
        }
        println!("]}}");
    } else {
        println!(
            "ticket #{} revision #{} (revision {}): {}\n  application revision: {}\n  evidence: {}\n  attempts: {}",
            response.ticket_id,
            response.ticket_revision_id,
            response.ticket_revision,
            response.state,
            response.application_revision_id,
            response.evidence.len(),
            response.attempts.len()
        );
    }
}

fn print_candidate_show(response: &CandidateShowResponse, json: bool) {
    if json {
        println!(
            "{{\"protocol_version\":{},\"request_id\":\"{}\",\"operation\":\"{}\",\"candidate_id\":{},\"candidate_revision\":{},\"state\":\"{}\",\"ticket_attempt_id\":{},\"ticket_revision_id\":{},\"ticket_revision\":{},\"base_commit\":\"{}\",\"candidate_tree\":\"{}\",\"candidate_commit\":{},\"evidence_count\":{},\"validation_count\":{},\"review_id\":{},\"architect_decision_id\":{}}}",
            response.protocol_version,
            response.request_id,
            response.operation,
            response.candidate_id,
            response.candidate_revision,
            response.state,
            response.ticket_attempt_id,
            response.ticket_revision_id,
            response.ticket_revision,
            response.base_commit,
            response.candidate_tree,
            optional_json_string(response.candidate_commit.as_deref()),
            response.evidence.len(),
            response.validations.len(),
            optional_i64(response.review.as_ref().map(|review| review.review_id)),
            optional_i64(
                response
                    .latest_architect_decision
                    .as_ref()
                    .map(|decision| decision.architect_decision_id)
            )
        );
    } else {
        println!(
            "candidate #{} (revision {}): {}\n  ticket attempt: {}; ticket revision: {} (revision {})\n  base: {}; candidate tree: {}\n  evidence: {}; validations: {}{}{}",
            response.candidate_id,
            response.candidate_revision,
            response.state,
            response.ticket_attempt_id,
            response.ticket_revision_id,
            response.ticket_revision,
            response.base_commit,
            response.candidate_tree,
            response.evidence.len(),
            response.validations.len(),
            response
                .review
                .as_ref()
                .map(|review| format!(
                    "\n  review #{} (revision {}, {})",
                    review.review_id, review.review_revision, review.verdict
                ))
                .unwrap_or_default(),
            response
                .latest_architect_decision
                .as_ref()
                .map(|decision| format!(
                    "\n  latest architect decision #{} ({})",
                    decision.architect_decision_id, decision.decision_kind
                ))
                .unwrap_or_default()
        );
    }
}

fn print_audit_show(response: &AuditShowResponse, json: bool) {
    if json {
        print!(
            "{{\"protocol_version\":{},\"request_id\":\"{}\",\"operation\":\"{}\",\"selector\":\"{}\",\"items\":[",
            response.protocol_version, response.request_id, response.operation, response.selector
        );
        for (index, item) in response.items.iter().enumerate() {
            if index != 0 {
                print!(",");
            }
            print!(
                "{{\"audit_id\":{},\"principal\":\"{}\",\"operation\":\"{}\",\"subject_kind\":{},\"subject_id\":{},\"aggregate_revision\":{}}}",
                item.audit_id,
                item.principal,
                item.operation,
                item.subject_kind,
                item.subject_id,
                item.aggregate_revision
            );
        }
        println!("]}}");
    } else if response.items.is_empty() {
        println!("audit {}: none", response.selector);
    } else {
        for item in &response.items {
            println!(
                "audit #{}: {} {} subject {}:{} (revision {})",
                item.audit_id,
                item.principal,
                item.operation,
                item.subject_kind,
                item.subject_id,
                item.aggregate_revision
            );
        }
    }
}

fn print_forum_topics(response: &ForumTopicsResponseV1, json: bool) {
    if json {
        print!(
            "{{\"protocol_version\":{},\"request_id\":\"{}\",\"operation\":\"{}\",\"items\":[",
            response.protocol_version, response.request_id, response.operation
        );
        for (index, item) in response.items.iter().enumerate() {
            if index != 0 {
                print!(",");
            }
            print!(
                "{{\"id\":{},\"name\":\"{}\",\"description\":\"{}\",\"author_kind\":{}}}",
                item.id, item.name, item.description, item.author_kind
            );
        }
        println!("],\"next_cursor\":\"{}\"}}", response.next_cursor);
    } else {
        for item in &response.items {
            println!("topic #{}: {}\n  {}", item.id, item.name, item.description);
        }
        if response.items.is_empty() {
            println!("forum topics: none");
        }
    }
}
fn print_forum_threads(response: &ForumThreadsResponseV1, json: bool) {
    if json {
        print!(
            "{{\"protocol_version\":{},\"request_id\":\"{}\",\"operation\":\"{}\",\"items\":[",
            response.protocol_version, response.request_id, response.operation
        );
        for (index, item) in response.items.iter().enumerate() {
            if index != 0 {
                print!(",");
            }
            print!(
                "{{\"id\":{},\"topic_id\":{},\"title\":\"{}\",\"author_kind\":{}}}",
                item.id, item.topic_id, item.title, item.author_kind
            );
        }
        println!("],\"next_cursor\":\"{}\"}}", response.next_cursor);
    } else {
        for item in &response.items {
            println!(
                "thread #{} in topic #{}: {}",
                item.id, item.topic_id, item.title
            );
        }
        if response.items.is_empty() {
            println!("forum threads: none");
        }
    }
}
fn print_forum_posts(response: &ForumPostsResponseV1, json: bool) {
    if json {
        print!(
            "{{\"protocol_version\":{},\"request_id\":\"{}\",\"operation\":\"{}\",\"items\":[",
            response.protocol_version, response.request_id, response.operation
        );
        for (index, item) in response.items.iter().enumerate() {
            if index != 0 {
                print!(",");
            }
            print!(
                "{{\"id\":{},\"thread_id\":{},\"kind\":{},\"body\":\"{}\",\"author_kind\":{}}}",
                item.id, item.thread_id, item.kind, item.body, item.author_kind
            );
        }
        println!("],\"next_cursor\":\"{}\"}}", response.next_cursor);
    } else {
        for item in &response.items {
            println!("post #{} (kind {}): {}", item.id, item.kind, item.body);
        }
        if response.items.is_empty() {
            println!("forum posts: none");
        }
    }
}
fn print_forum_search(response: &ForumSearchResponseV1, json: bool) {
    if json {
        print!(
            "{{\"protocol_version\":{},\"request_id\":\"{}\",\"operation\":\"{}\",\"items\":[",
            response.protocol_version, response.request_id, response.operation
        );
        for (index, item) in response.items.iter().enumerate() {
            if index != 0 {
                print!(",");
            }
            print!(
                "{{\"topic_id\":{},\"thread_id\":{},\"post_id\":{},\"snippet\":\"{}\"}}",
                item.topic_id, item.thread_id, item.post_id, item.snippet
            );
        }
        println!("],\"next_cursor\":\"{}\"}}", response.next_cursor);
    } else {
        for item in &response.items {
            println!(
                "post #{} / thread #{} / topic #{}: {}",
                item.post_id, item.thread_id, item.topic_id, item.snippet
            );
        }
        if response.items.is_empty() {
            println!("forum search: no matches");
        }
    }
}
fn print_forum_receipt(receipt: &OperationReceiptResponse, json: bool) {
    if json {
        println!(
            "{{\"protocol_version\":{},\"request_id\":\"{}\",\"operation\":\"{}\",\"audit_id\":{},\"aggregate_revision\":{}}}",
            receipt.protocol_version,
            receipt.request_id,
            receipt.operation,
            receipt.audit_id,
            receipt.aggregate_revision
        );
    } else {
        println!(
            "Forum mutation accepted (revision {}, audit #{})",
            receipt.aggregate_revision, receipt.audit_id
        );
    }
}

fn optional_u64(value: Option<u64>) -> String {
    value.map_or_else(|| "null".to_owned(), |value| value.to_string())
}

fn optional_i64(value: Option<i64>) -> String {
    value.map_or_else(|| "null".to_owned(), |value| value.to_string())
}

fn optional_json_string(value: Option<&str>) -> String {
    value.map_or_else(|| "null".to_owned(), |value| format!("\"{value}\""))
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct ConnectionArgs {
    socket_path: PathBuf,
    json: bool,
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct ArchitectBaseArgs {
    connection: ConnectionArgs,
    client_command_id: String,
    expected_revision: u64,
    rationale: SealedArtifactReferenceWireV1,
    principal: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct CampaignStartArgs {
    connection: ConnectionArgs,
    client_command_id: String,
    expected_application_revision: u64,
    application_revision_id: i64,
    aggregate_budget_micro_usd: u64,
    deadline_unix_millis: u64,
    delivery_target: u32,
    principal: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct CampaignCancelArgs {
    connection: ConnectionArgs,
    client_command_id: String,
    expected_revision: u64,
    campaign_id: i64,
    principal: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct ApplicationShowArgs {
    connection: ConnectionArgs,
    application_key: String,
    application_revision_id: Option<i64>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct ApplicationRegisterArgs {
    connection: ConnectionArgs,
    client_command_id: String,
    expected_revision: u64,
    expected_kernel_build_revision: u64,
    kernel_build_id: String,
    source_root: PathBuf,
    bundle_relative_path: String,
    principal: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct ApplicationActivateArgs {
    base: ArchitectBaseArgs,
    application_key: String,
    application_revision_id: i64,
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct OperatorArtifactSealArgs {
    connection: ConnectionArgs,
    client_command_id: String,
    expected_kernel_build_revision: u64,
    source_root: PathBuf,
    source_relative_path: String,
    principal: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct TicketListArgs {
    connection: ConnectionArgs,
    state: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct ForumConnectionArgs {
    connection: ConnectionArgs,
    cursor: String,
    limit: u8,
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct ForumMutationArgs {
    connection: ConnectionArgs,
    client_command_id: String,
    expected_revision: u64,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum CandidateDecision {
    Deliver,
    Rework,
    Reject,
}

impl CandidateDecision {
    const fn as_str(self) -> &'static str {
        match self {
            Self::Deliver => "deliver",
            Self::Rework => "rework",
            Self::Reject => "reject",
        }
    }
}

#[derive(Debug, PartialEq, Eq)]
enum CliCommand {
    Init(InitCommand),
    DaemonStatus(ConnectionArgs),
    Sponsor {
        base: ArchitectBaseArgs,
        ticket_revision_id: i64,
    },
    Release {
        base: ArchitectBaseArgs,
        ticket_attempt_id: i64,
    },
    Decide {
        base: ArchitectBaseArgs,
        candidate_id: i64,
        review_id: i64,
        decision: CandidateDecision,
        quality_override_review_id: Option<i64>,
    },
    CampaignStart(CampaignStartArgs),
    CampaignStatus {
        connection: ConnectionArgs,
        campaign_id: i64,
    },
    CampaignCancel(CampaignCancelArgs),
    ApplicationShow(ApplicationShowArgs),
    ApplicationRegister(ApplicationRegisterArgs),
    ApplicationActivate(ApplicationActivateArgs),
    OperatorArtifactSeal(OperatorArtifactSealArgs),
    TicketList(TicketListArgs),
    TicketShow {
        connection: ConnectionArgs,
        ticket_id: i64,
    },
    CandidateShow {
        connection: ConnectionArgs,
        candidate_id: i64,
    },
    AuditShow {
        connection: ConnectionArgs,
        selector: String,
    },
    ForumTopics(ForumConnectionArgs),
    ForumThreads {
        base: ForumConnectionArgs,
        topic_id: i64,
    },
    ForumRead {
        base: ForumConnectionArgs,
        thread_id: i64,
        after_post_id: i64,
    },
    ForumSearch {
        base: ForumConnectionArgs,
        query: String,
    },
    ForumCreateTopic {
        base: ForumMutationArgs,
        name: String,
        description: String,
    },
    ForumCreateThread {
        base: ForumMutationArgs,
        topic_id: i64,
        title: String,
    },
    ForumPost {
        base: ForumMutationArgs,
        thread_id: i64,
        kind: u8,
        body: String,
        reply_to: Option<i64>,
        supersedes: Option<i64>,
        attachments: Vec<ForumAttachmentWireV1>,
    },
}

/// Configuration which `factoryctl` forwards to one exact `factoryd init`
/// child. This CLI retains no SQL code or database connection: it validates
/// only the bounded command line and then waits for the one-shot daemon.
#[derive(Clone, Debug, PartialEq, Eq)]
struct InitCommand {
    factoryd: PathBuf,
    database_url: String,
    runtime_root: PathBuf,
    kernel_source_root: PathBuf,
    kernel_source_files: Vec<String>,
    cargo_executable: PathBuf,
    git_executable: PathBuf,
    deno_executable: PathBuf,
    pi_host_source_root: PathBuf,
    pi_host_source_files: Vec<String>,
    pi_host_entrypoint: PathBuf,
    deno_config: PathBuf,
    deno_lock: PathBuf,
    deno_dir: PathBuf,
    dependency_graph_receipt: PathBuf,
    pi_version: String,
    openrouter_credential_environment: String,
}

fn parse_args(arguments: Vec<String>) -> Result<CliCommand, String> {
    let mut values = arguments.into_iter();
    match values.next().as_deref() {
        Some("init") => parse_init(values.collect()).map(CliCommand::Init),
        Some("daemon") => match values.next().as_deref() {
            Some("status") => parse_status(values.collect()),
            _ => Err("expected `daemon status`".to_owned()),
        },
        Some("ticket") => match values.next().as_deref() {
            Some("list") => parse_ticket_list(values.collect()).map(CliCommand::TicketList),
            Some("show") => {
                let ticket_id = positive_id(
                    &values
                        .next()
                        .ok_or_else(|| "ticket ID is required".to_owned())?,
                    "ticket ID",
                )?;
                parse_campaign_connection(values.collect())
                    .map(|connection| CliCommand::TicketShow { connection, ticket_id })
            }
            Some("sponsor") => {
                let ticket_revision_id = positive_id(
                    &values
                        .next()
                        .ok_or_else(|| "ticket revision ID is required".to_owned())?,
                    "ticket revision ID",
                )?;
                Ok(CliCommand::Sponsor {
                    base: parse_architect_base(values.collect())?,
                    ticket_revision_id,
                })
            }
            Some("release") => {
                let ticket_attempt_id = positive_id(
                    &values
                        .next()
                        .ok_or_else(|| "ticket attempt ID is required".to_owned())?,
                    "ticket attempt ID",
                )?;
                Ok(CliCommand::Release {
                    base: parse_architect_base(values.collect())?,
                    ticket_attempt_id,
                })
            }
            _ => Err("expected `ticket list`, `ticket show`, `ticket sponsor`, or `ticket release`".to_owned()),
        },
        Some("candidate") => match values.next().as_deref() {
            Some("show") => {
                let candidate_id = positive_id(
                    &values
                        .next()
                        .ok_or_else(|| "candidate ID is required".to_owned())?,
                    "candidate ID",
                )?;
                parse_campaign_connection(values.collect()).map(|connection| {
                    CliCommand::CandidateShow {
                        connection,
                        candidate_id,
                    }
                })
            }
            Some("decide") => {
                let candidate_id = positive_id(
                    &values
                        .next()
                        .ok_or_else(|| "candidate ID is required".to_owned())?,
                    "candidate ID",
                )?;
                parse_candidate_decision(candidate_id, values.collect())
            }
            _ => Err("expected `candidate show` or `candidate decide`".to_owned()),
        },
        Some("application") => match values.next().as_deref() {
            Some("show") => {
                let application_key = values
                    .next()
                    .ok_or_else(|| "application key is required".to_owned())?;
                parse_application_show(application_key, values.collect())
                    .map(CliCommand::ApplicationShow)
            }
            Some("register") => parse_application_register(values.collect())
                .map(CliCommand::ApplicationRegister),
            Some("activate") => {
                let application_key = values
                    .next()
                    .ok_or_else(|| "application key is required".to_owned())?;
                let application_revision_id = positive_id(
                    &values
                        .next()
                        .ok_or_else(|| "application revision ID is required".to_owned())?,
                    "application revision ID",
                )?;
                Ok(CliCommand::ApplicationActivate(ApplicationActivateArgs {
                    base: parse_architect_base(values.collect())?,
                    application_key,
                    application_revision_id,
                }))
            }
            _ => Err("expected `application show`, `application register`, or `application activate`".to_owned()),
        },
        Some("artifact") => match values.next().as_deref() {
            Some("seal") => parse_operator_artifact_seal(values.collect())
                .map(CliCommand::OperatorArtifactSeal),
            _ => Err("expected `artifact seal`".to_owned()),
        },
        Some("campaign") => match values.next().as_deref() {
            Some("start") => parse_campaign_start(values.collect()).map(CliCommand::CampaignStart),
            Some("status") => {
                let campaign_id = positive_id(
                    &values.next().ok_or_else(|| "campaign ID is required".to_owned())?,
                    "campaign ID",
                )?;
                parse_campaign_connection(values.collect()).map(|connection| {
                    CliCommand::CampaignStatus {
                        connection,
                        campaign_id,
                    }
                })
            }
            Some("cancel") => {
                let campaign_id = positive_id(
                    &values.next().ok_or_else(|| "campaign ID is required".to_owned())?,
                    "campaign ID",
                )?;
                parse_campaign_cancel(campaign_id, values.collect()).map(CliCommand::CampaignCancel)
            }
            _ => Err("expected `campaign start`, `campaign status`, or `campaign cancel`".to_owned()),
        },
        Some("audit") => match values.next().as_deref() {
            Some("show") => {
                let selector = values
                    .next()
                    .ok_or_else(|| "audit selector is required".to_owned())?;
                parse_campaign_connection(values.collect())
                    .map(|connection| CliCommand::AuditShow { connection, selector })
            }
            _ => Err("expected `audit show`".to_owned()),
        },
        Some("forum") => match values.next().as_deref() {
            Some("topics") => parse_forum_connection(values.collect()).map(CliCommand::ForumTopics),
            Some("threads") => {
                let topic_id = positive_id(&values.next().ok_or_else(|| "topic ID is required".to_owned())?, "topic ID")?;
                parse_forum_connection(values.collect()).map(|base| CliCommand::ForumThreads { base, topic_id })
            }
            Some("read") => {
                let thread_id = positive_id(&values.next().ok_or_else(|| "thread ID is required".to_owned())?, "thread ID")?;
                parse_forum_read(thread_id, values.collect())
            }
            Some("search") => {
                let query = values.next().ok_or_else(|| "search query is required".to_owned())?;
                parse_forum_connection(values.collect()).map(|base| CliCommand::ForumSearch { base, query })
            }
            Some("create-topic") => parse_forum_create_topic(values.collect()),
            Some("create-thread") => parse_forum_create_thread(values.collect()),
            Some("post") => parse_forum_post(values.collect()),
            _ => Err("expected `forum topics|threads|read|search|create-topic|create-thread|post`".to_owned()),
        },
        _ => Err(
            "expected `init`, `daemon status`, application/campaign/ticket/candidate/audit commands, or `forum topics|threads|read|search|create-topic|create-thread|post`"
                .to_owned(),
        ),
    }
}

fn parse_init(arguments: Vec<String>) -> Result<InitCommand, String> {
    let mut values = arguments.into_iter();
    let mut factoryd = None;
    let mut database_url = None;
    let mut runtime_root = None;
    let mut kernel_source_root = None;
    let mut kernel_source_files = Vec::new();
    let mut cargo_executable = None;
    let mut git_executable = None;
    let mut deno_executable = None;
    let mut pi_host_source_root = None;
    let mut pi_host_source_files = Vec::new();
    let mut pi_host_entrypoint = None;
    let mut deno_config = None;
    let mut deno_lock = None;
    let mut deno_dir = None;
    let mut dependency_graph_receipt = None;
    let mut pi_version = None;
    let mut openrouter_credential_environment = None;
    while let Some(flag) = values.next() {
        let value = next_value(&mut values, &flag)?;
        match flag.as_str() {
            "--factoryd" => set_absolute_path(&mut factoryd, value, "--factoryd")?,
            "--database-url" => set_once(&mut database_url, value, "--database-url")?,
            "--runtime-root" => set_absolute_path(&mut runtime_root, value, "--runtime-root")?,
            "--kernel-source-root" => {
                set_absolute_path(&mut kernel_source_root, value, "--kernel-source-root")?
            }
            "--kernel-source-file" => kernel_source_files.push(safe_relative(value, &flag)?),
            // `factoryctl` selects this itself from the exact binary it is
            // about to exec. Letting the caller name a second binary would
            // create false provenance.
            "--kernel-binary" => {
                return Err(
                    "--kernel-binary is selected from --factoryd and must not be supplied"
                        .to_owned(),
                );
            }
            "--cargo-executable" => {
                set_absolute_path(&mut cargo_executable, value, "--cargo-executable")?
            }
            "--git-executable" => {
                set_absolute_path(&mut git_executable, value, "--git-executable")?
            }
            "--deno-executable" => {
                set_absolute_path(&mut deno_executable, value, "--deno-executable")?
            }
            "--pi-host-source-root" => {
                set_absolute_path(&mut pi_host_source_root, value, "--pi-host-source-root")?
            }
            "--pi-host-source-file" => pi_host_source_files.push(safe_relative(value, &flag)?),
            "--pi-host-entrypoint" => {
                set_absolute_path(&mut pi_host_entrypoint, value, "--pi-host-entrypoint")?
            }
            "--deno-config" => set_absolute_path(&mut deno_config, value, "--deno-config")?,
            "--deno-lock" => set_absolute_path(&mut deno_lock, value, "--deno-lock")?,
            "--deno-dir" => set_absolute_path(&mut deno_dir, value, "--deno-dir")?,
            "--dependency-graph-receipt" => set_absolute_path(
                &mut dependency_graph_receipt,
                value,
                "--dependency-graph-receipt",
            )?,
            "--pi-version" => set_once(&mut pi_version, value, "--pi-version")?,
            "--provider-credential-environment" => set_once(
                &mut openrouter_credential_environment,
                parse_openrouter_credential_environment(value)?,
                "--provider-credential-environment",
            )?,
            _ => return Err(format!("unknown flag {flag}")),
        }
    }
    if kernel_source_files.is_empty() {
        return Err("at least one --kernel-source-file is required".to_owned());
    }
    if pi_host_source_files.is_empty() {
        return Err("at least one --pi-host-source-file is required".to_owned());
    }
    Ok(InitCommand {
        factoryd: required(factoryd, "--factoryd")?,
        database_url: required(database_url, "--database-url")?,
        runtime_root: required(runtime_root, "--runtime-root")?,
        kernel_source_root: required(kernel_source_root, "--kernel-source-root")?,
        kernel_source_files,
        cargo_executable: required(cargo_executable, "--cargo-executable")?,
        git_executable: required(git_executable, "--git-executable")?,
        deno_executable: required(deno_executable, "--deno-executable")?,
        pi_host_source_root: required(pi_host_source_root, "--pi-host-source-root")?,
        pi_host_source_files,
        pi_host_entrypoint: required(pi_host_entrypoint, "--pi-host-entrypoint")?,
        deno_config: required(deno_config, "--deno-config")?,
        deno_lock: required(deno_lock, "--deno-lock")?,
        deno_dir: required(deno_dir, "--deno-dir")?,
        dependency_graph_receipt: required(dependency_graph_receipt, "--dependency-graph-receipt")?,
        pi_version: required(pi_version, "--pi-version")?,
        openrouter_credential_environment: required(
            openrouter_credential_environment,
            "--provider-credential-environment openrouter=<ENVIRONMENT_NAME>",
        )?,
    })
}

fn spawn_factoryd_init(command: &InitCommand) -> Result<(), io::Error> {
    let factoryd = exact_regular_file("--factoryd", &command.factoryd)?;
    let status = Command::new(&factoryd)
        .args(factoryd_init_arguments(command, &factoryd))
        .status()
        .map_err(|source| {
            io::Error::new(
                source.kind(),
                format!("cannot start factoryd init {factoryd:?}: {source}"),
            )
        })?;
    if status.success() {
        Ok(())
    } else {
        Err(io::Error::other(format!(
            "factoryd init {factoryd:?} exited with {status}"
        )))
    }
}

/// The child owns initialization semantics. This CLI's only authority here is
/// choosing the exact installed executable and forwarding a closed argument
/// set; it has no SQL imports, pool, or database write path.
fn factoryd_init_arguments(command: &InitCommand, factoryd: &Path) -> Vec<OsString> {
    let mut arguments = vec![OsString::from("init")];
    push_argument(&mut arguments, "--database-url", &command.database_url);
    push_path_argument(&mut arguments, "--runtime-root", &command.runtime_root);
    push_path_argument(
        &mut arguments,
        "--kernel-source-root",
        &command.kernel_source_root,
    );
    for source_file in &command.kernel_source_files {
        push_argument(&mut arguments, "--kernel-source-file", source_file);
    }
    // Never forward a caller-provided kernel binary. The daemon checks this
    // exact canonical child executable against its own current executable.
    push_path_argument(&mut arguments, "--kernel-binary", factoryd);
    push_path_argument(
        &mut arguments,
        "--cargo-executable",
        &command.cargo_executable,
    );
    push_path_argument(&mut arguments, "--git-executable", &command.git_executable);
    push_path_argument(
        &mut arguments,
        "--deno-executable",
        &command.deno_executable,
    );
    push_path_argument(
        &mut arguments,
        "--pi-host-source-root",
        &command.pi_host_source_root,
    );
    for source_file in &command.pi_host_source_files {
        push_argument(&mut arguments, "--pi-host-source-file", source_file);
    }
    push_path_argument(
        &mut arguments,
        "--pi-host-entrypoint",
        &command.pi_host_entrypoint,
    );
    push_path_argument(&mut arguments, "--deno-config", &command.deno_config);
    push_path_argument(&mut arguments, "--deno-lock", &command.deno_lock);
    push_path_argument(&mut arguments, "--deno-dir", &command.deno_dir);
    push_path_argument(
        &mut arguments,
        "--dependency-graph-receipt",
        &command.dependency_graph_receipt,
    );
    push_argument(&mut arguments, "--pi-version", &command.pi_version);
    push_argument(
        &mut arguments,
        "--provider-credential-environment",
        &format!("openrouter={}", command.openrouter_credential_environment),
    );
    arguments
}

fn push_argument(arguments: &mut Vec<OsString>, flag: &str, value: &str) {
    arguments.push(OsString::from(flag));
    arguments.push(OsString::from(value));
}

fn push_path_argument(arguments: &mut Vec<OsString>, flag: &str, value: &Path) {
    arguments.push(OsString::from(flag));
    arguments.push(value.as_os_str().to_owned());
}

fn exact_regular_file(field: &str, path: &Path) -> Result<PathBuf, io::Error> {
    let canonical = fs::canonicalize(path).map_err(|source| {
        io::Error::new(
            source.kind(),
            format!("cannot canonicalize {field} {path:?}: {source}"),
        )
    })?;
    let metadata = fs::metadata(&canonical).map_err(|source| {
        io::Error::new(
            source.kind(),
            format!("cannot inspect {field} {canonical:?}: {source}"),
        )
    })?;
    if !metadata.is_file() {
        return Err(io::Error::other(format!(
            "{field} must name a regular file"
        )));
    }
    Ok(canonical)
}

fn parse_status(arguments: Vec<String>) -> Result<CliCommand, String> {
    let mut values = arguments.into_iter();
    let mut socket_path = None;
    let mut json = false;
    while let Some(flag) = values.next() {
        match flag.as_str() {
            "--socket" => set_once(
                &mut socket_path,
                PathBuf::from(next_value(&mut values, "--socket")?),
                "--socket",
            )?,
            "--format" => {
                let format = next_value(&mut values, "--format")?;
                if format != "json" || json {
                    return Err("only one `--format json` is supported".to_owned());
                }
                json = true;
            }
            _ => return Err(format!("unknown flag {flag}")),
        }
    }
    Ok(CliCommand::DaemonStatus(ConnectionArgs {
        socket_path: socket_path.ok_or_else(|| "--socket is required".to_owned())?,
        json,
    }))
}

fn parse_campaign_connection(arguments: Vec<String>) -> Result<ConnectionArgs, String> {
    let mut values = arguments.into_iter();
    let mut socket_path = None;
    let mut json = false;
    while let Some(flag) = values.next() {
        match flag.as_str() {
            "--socket" => set_once(
                &mut socket_path,
                PathBuf::from(next_value(&mut values, "--socket")?),
                "--socket",
            )?,
            "--format" => {
                let format = next_value(&mut values, "--format")?;
                if format != "json" || json {
                    return Err("only one `--format json` is supported".to_owned());
                }
                json = true;
            }
            _ => return Err(format!("unknown flag {flag}")),
        }
    }
    Ok(ConnectionArgs {
        socket_path: socket_path.ok_or_else(|| "--socket is required".to_owned())?,
        json,
    })
}

fn parse_ticket_list(arguments: Vec<String>) -> Result<TicketListArgs, String> {
    let mut values = arguments.into_iter();
    let mut socket_path = None;
    let mut json = false;
    let mut state = None;
    while let Some(flag) = values.next() {
        match flag.as_str() {
            "--socket" => set_once(
                &mut socket_path,
                PathBuf::from(next_value(&mut values, "--socket")?),
                "--socket",
            )?,
            "--state" => set_once(&mut state, next_value(&mut values, "--state")?, "--state")?,
            "--format" => {
                let format = next_value(&mut values, "--format")?;
                if format != "json" || json {
                    return Err("only one `--format json` is supported".to_owned());
                }
                json = true;
            }
            _ => return Err(format!("unknown flag {flag}")),
        }
    }
    Ok(TicketListArgs {
        connection: ConnectionArgs {
            socket_path: socket_path.ok_or_else(|| "--socket is required".to_owned())?,
            json,
        },
        state,
    })
}

fn parse_forum_connection(arguments: Vec<String>) -> Result<ForumConnectionArgs, String> {
    let mut values = arguments.into_iter();
    let mut socket_path = None;
    let mut json = false;
    let mut cursor = String::new();
    let mut limit = 20_u8;
    while let Some(flag) = values.next() {
        match flag.as_str() {
            "--socket" => set_once(
                &mut socket_path,
                PathBuf::from(next_value(&mut values, "--socket")?),
                "--socket",
            )?,
            "--cursor" => {
                if !cursor.is_empty() {
                    return Err("--cursor was supplied more than once".to_owned());
                }
                cursor = next_value(&mut values, "--cursor")?;
            }
            "--limit" => {
                limit = next_value(&mut values, "--limit")?
                    .parse::<u8>()
                    .ok()
                    .filter(|value| (1..=20).contains(value))
                    .ok_or_else(|| "--limit must be 1 through 20".to_owned())?
            }
            "--format" => {
                let format = next_value(&mut values, "--format")?;
                if format != "json" || json {
                    return Err("only one `--format json` is supported".to_owned());
                }
                json = true;
            }
            _ => return Err(format!("unknown flag {flag}")),
        }
    }
    Ok(ForumConnectionArgs {
        connection: ConnectionArgs {
            socket_path: socket_path.ok_or_else(|| "--socket is required".to_owned())?,
            json,
        },
        cursor,
        limit,
    })
}

fn parse_forum_read(thread_id: i64, arguments: Vec<String>) -> Result<CliCommand, String> {
    let mut after_post_id = 0_i64;
    let mut forwarded = Vec::new();
    let mut values = arguments.into_iter();
    while let Some(flag) = values.next() {
        if flag == "--after-post" {
            if after_post_id != 0 {
                return Err("--after-post was supplied more than once".to_owned());
            }
            after_post_id = positive_id(&next_value(&mut values, "--after-post")?, "--after-post")?;
        } else {
            forwarded.push(flag);
            forwarded.push(next_value(&mut values, "forum read flag")?);
        }
    }
    parse_forum_connection(forwarded).map(|base| CliCommand::ForumRead {
        base,
        thread_id,
        after_post_id,
    })
}

fn parse_forum_mutation(
    arguments: Vec<String>,
) -> Result<(ForumMutationArgs, Vec<(String, String)>), String> {
    let mut values = arguments.into_iter();
    let mut socket_path = None;
    let mut json = false;
    let mut client_command_id = None;
    let mut expected_revision = None;
    let mut remaining = Vec::new();
    while let Some(flag) = values.next() {
        match flag.as_str() {
            "--socket" => set_once(
                &mut socket_path,
                PathBuf::from(next_value(&mut values, "--socket")?),
                "--socket",
            )?,
            "--client-command-id" => set_once(
                &mut client_command_id,
                next_value(&mut values, "--client-command-id")?,
                "--client-command-id",
            )?,
            "--expected-revision" => set_once(
                &mut expected_revision,
                nonnegative_u64(
                    &next_value(&mut values, "--expected-revision")?,
                    "--expected-revision",
                )?,
                "--expected-revision",
            )?,
            "--format" => {
                let format = next_value(&mut values, "--format")?;
                if format != "json" || json {
                    return Err("only one `--format json` is supported".to_owned());
                }
                json = true;
            }
            _ => remaining.push((flag.clone(), next_value(&mut values, &flag)?)),
        }
    }
    Ok((
        ForumMutationArgs {
            connection: ConnectionArgs {
                socket_path: socket_path.ok_or_else(|| "--socket is required".to_owned())?,
                json,
            },
            client_command_id: client_command_id
                .ok_or_else(|| "--client-command-id is required".to_owned())?,
            expected_revision: expected_revision
                .ok_or_else(|| "--expected-revision is required".to_owned())?,
        },
        remaining,
    ))
}

fn parse_forum_create_topic(arguments: Vec<String>) -> Result<CliCommand, String> {
    let (base, fields) = parse_forum_mutation(arguments)?;
    let mut name = None;
    let mut description = None;
    for (flag, value) in fields {
        match flag.as_str() {
            "--name" => set_once(&mut name, value, "--name")?,
            "--description" => set_once(&mut description, value, "--description")?,
            _ => return Err(format!("unknown flag {flag}")),
        }
    }
    Ok(CliCommand::ForumCreateTopic {
        base,
        name: name.ok_or_else(|| "--name is required".to_owned())?,
        description: description.ok_or_else(|| "--description is required".to_owned())?,
    })
}

fn parse_forum_create_thread(arguments: Vec<String>) -> Result<CliCommand, String> {
    let (base, fields) = parse_forum_mutation(arguments)?;
    let mut topic_id = None;
    let mut title = None;
    for (flag, value) in fields {
        match flag.as_str() {
            "--topic-id" => set_once(
                &mut topic_id,
                positive_id(&value, "--topic-id")?,
                "--topic-id",
            )?,
            "--title" => set_once(&mut title, value, "--title")?,
            _ => return Err(format!("unknown flag {flag}")),
        }
    }
    Ok(CliCommand::ForumCreateThread {
        base,
        topic_id: topic_id.ok_or_else(|| "--topic-id is required".to_owned())?,
        title: title.ok_or_else(|| "--title is required".to_owned())?,
    })
}

fn parse_forum_post(arguments: Vec<String>) -> Result<CliCommand, String> {
    let (base, fields) = parse_forum_mutation(arguments)?;
    let mut thread_id = None;
    let mut kind = None;
    let mut body = None;
    let mut reply_to = None;
    let mut supersedes = None;
    let mut attachments = Vec::new();
    for (flag, value) in fields {
        match flag.as_str() {
            "--thread-id" => set_once(
                &mut thread_id,
                positive_id(&value, "--thread-id")?,
                "--thread-id",
            )?,
            "--kind" => set_once(&mut kind, forum_post_kind_code(&value)?, "--kind")?,
            "--body" => set_once(&mut body, value, "--body")?,
            "--reply-to" => set_once(
                &mut reply_to,
                positive_id(&value, "--reply-to")?,
                "--reply-to",
            )?,
            "--supersedes" => set_once(
                &mut supersedes,
                positive_id(&value, "--supersedes")?,
                "--supersedes",
            )?,
            "--attachment" => attachments.push(parse_forum_attachment(&value)?),
            _ => return Err(format!("unknown flag {flag}")),
        }
    }
    Ok(CliCommand::ForumPost {
        base,
        thread_id: thread_id.ok_or_else(|| "--thread-id is required".to_owned())?,
        kind: kind.ok_or_else(|| "--kind is required".to_owned())?,
        body: body.ok_or_else(|| "--body is required".to_owned())?,
        reply_to,
        supersedes,
        attachments,
    })
}

fn parse_forum_attachment(value: &str) -> Result<ForumAttachmentWireV1, String> {
    let (artifact_id, label) = value
        .split_once(':')
        .ok_or_else(|| "--attachment must be <artifact-id>:<label>".to_owned())?;
    if label.is_empty() || label.contains('\0') {
        return Err("--attachment label must be nonempty and NUL-free".to_owned());
    }
    Ok(ForumAttachmentWireV1 {
        artifact_id: positive_id(artifact_id, "--attachment artifact ID")?,
        label: label.to_owned(),
    })
}

fn forum_post_kind_code(value: &str) -> Result<u8, String> {
    match value {
        "note" => Ok(0),
        "question" => Ok(1),
        "finding" => Ok(2),
        "proposal" => Ok(3),
        "challenge" => Ok(4),
        "correction" => Ok(5),
        "decision_link" => Ok(6),
        _ => Err(
            "--kind must be note|question|finding|proposal|challenge|correction|decision_link"
                .to_owned(),
        ),
    }
}

fn parse_application_show(
    application_key: String,
    arguments: Vec<String>,
) -> Result<ApplicationShowArgs, String> {
    let mut values = arguments.into_iter();
    let mut socket_path = None;
    let mut json = false;
    let mut application_revision_id = None;
    while let Some(flag) = values.next() {
        match flag.as_str() {
            "--socket" => set_once(
                &mut socket_path,
                PathBuf::from(next_value(&mut values, "--socket")?),
                "--socket",
            )?,
            "--application-revision-id" => set_once(
                &mut application_revision_id,
                positive_id(
                    &next_value(&mut values, "--application-revision-id")?,
                    "--application-revision-id",
                )?,
                "--application-revision-id",
            )?,
            "--format" => {
                let format = next_value(&mut values, "--format")?;
                if format != "json" || json {
                    return Err("only one `--format json` is supported".to_owned());
                }
                json = true;
            }
            _ => return Err(format!("unknown flag {flag}")),
        }
    }
    Ok(ApplicationShowArgs {
        connection: ConnectionArgs {
            socket_path: socket_path.ok_or_else(|| "--socket is required".to_owned())?,
            json,
        },
        application_key,
        application_revision_id,
    })
}

fn parse_application_register(arguments: Vec<String>) -> Result<ApplicationRegisterArgs, String> {
    let mut values = arguments.into_iter();
    let mut socket_path = None;
    let mut json = false;
    let mut client_command_id = None;
    let mut expected_revision = None;
    let mut expected_kernel_build_revision = None;
    let mut kernel_build_id = None;
    let mut source_root = None;
    let mut bundle_relative_path = None;
    let mut principal = None;
    while let Some(flag) = values.next() {
        match flag.as_str() {
            "--socket" => set_once(
                &mut socket_path,
                PathBuf::from(next_value(&mut values, "--socket")?),
                "--socket",
            )?,
            "--format" => {
                let format = next_value(&mut values, "--format")?;
                if format != "json" || json {
                    return Err("only one `--format json` is supported".to_owned());
                }
                json = true;
            }
            "--client-command-id" => set_once(
                &mut client_command_id,
                next_value(&mut values, "--client-command-id")?,
                "--client-command-id",
            )?,
            "--expected-revision" => set_once(
                &mut expected_revision,
                nonnegative_u64(
                    &next_value(&mut values, "--expected-revision")?,
                    "--expected-revision",
                )?,
                "--expected-revision",
            )?,
            "--expected-kernel-build-revision" => set_once(
                &mut expected_kernel_build_revision,
                nonnegative_u64(
                    &next_value(&mut values, "--expected-kernel-build-revision")?,
                    "--expected-kernel-build-revision",
                )?,
                "--expected-kernel-build-revision",
            )?,
            "--kernel-build-id" => set_once(
                &mut kernel_build_id,
                next_value(&mut values, "--kernel-build-id")?,
                "--kernel-build-id",
            )?,
            "--source-root" => set_absolute_path(
                &mut source_root,
                next_value(&mut values, "--source-root")?,
                "--source-root",
            )?,
            "--bundle-relative-path" => set_once(
                &mut bundle_relative_path,
                safe_relative(
                    next_value(&mut values, "--bundle-relative-path")?,
                    "--bundle-relative-path",
                )?,
                "--bundle-relative-path",
            )?,
            "--principal" => set_once(
                &mut principal,
                next_value(&mut values, "--principal")?,
                "--principal",
            )?,
            _ => return Err(format!("unknown flag {flag}")),
        }
    }
    Ok(ApplicationRegisterArgs {
        connection: ConnectionArgs {
            socket_path: socket_path.ok_or_else(|| "--socket is required".to_owned())?,
            json,
        },
        client_command_id: client_command_id
            .ok_or_else(|| "--client-command-id is required".to_owned())?,
        expected_revision: expected_revision
            .ok_or_else(|| "--expected-revision is required".to_owned())?,
        expected_kernel_build_revision: expected_kernel_build_revision
            .ok_or_else(|| "--expected-kernel-build-revision is required".to_owned())?,
        kernel_build_id: kernel_build_id
            .ok_or_else(|| "--kernel-build-id is required".to_owned())?,
        source_root: source_root.ok_or_else(|| "--source-root is required".to_owned())?,
        bundle_relative_path: bundle_relative_path
            .ok_or_else(|| "--bundle-relative-path is required".to_owned())?,
        principal: principal.ok_or_else(|| "--principal is required".to_owned())?,
    })
}

fn parse_operator_artifact_seal(
    arguments: Vec<String>,
) -> Result<OperatorArtifactSealArgs, String> {
    let mut values = arguments.into_iter();
    let mut socket_path = None;
    let mut json = false;
    let mut client_command_id = None;
    let mut expected_kernel_build_revision = None;
    let mut source_root = None;
    let mut source_relative_path = None;
    let mut principal = None;
    while let Some(flag) = values.next() {
        match flag.as_str() {
            "--socket" => set_once(
                &mut socket_path,
                PathBuf::from(next_value(&mut values, "--socket")?),
                "--socket",
            )?,
            "--format" => {
                let format = next_value(&mut values, "--format")?;
                if format != "json" || json {
                    return Err("only one `--format json` is supported".to_owned());
                }
                json = true;
            }
            "--client-command-id" => set_once(
                &mut client_command_id,
                next_value(&mut values, "--client-command-id")?,
                "--client-command-id",
            )?,
            "--expected-kernel-build-revision" => set_once(
                &mut expected_kernel_build_revision,
                nonnegative_u64(
                    &next_value(&mut values, "--expected-kernel-build-revision")?,
                    "--expected-kernel-build-revision",
                )?,
                "--expected-kernel-build-revision",
            )?,
            "--source-root" => set_absolute_path(
                &mut source_root,
                next_value(&mut values, "--source-root")?,
                "--source-root",
            )?,
            "--source-relative-path" => set_once(
                &mut source_relative_path,
                safe_relative(
                    next_value(&mut values, "--source-relative-path")?,
                    "--source-relative-path",
                )?,
                "--source-relative-path",
            )?,
            "--principal" => set_once(
                &mut principal,
                next_value(&mut values, "--principal")?,
                "--principal",
            )?,
            "--reason" => {
                return Err(
                    "inline --reason is not accepted; seal one regular evidence file".to_owned(),
                );
            }
            _ => return Err(format!("unknown flag {flag}")),
        }
    }
    Ok(OperatorArtifactSealArgs {
        connection: ConnectionArgs {
            socket_path: socket_path.ok_or_else(|| "--socket is required".to_owned())?,
            json,
        },
        client_command_id: client_command_id
            .ok_or_else(|| "--client-command-id is required".to_owned())?,
        expected_kernel_build_revision: expected_kernel_build_revision
            .ok_or_else(|| "--expected-kernel-build-revision is required".to_owned())?,
        source_root: source_root.ok_or_else(|| "--source-root is required".to_owned())?,
        source_relative_path: source_relative_path
            .ok_or_else(|| "--source-relative-path is required".to_owned())?,
        principal: principal.ok_or_else(|| "--principal is required".to_owned())?,
    })
}

fn parse_campaign_start(arguments: Vec<String>) -> Result<CampaignStartArgs, String> {
    let mut values = arguments.into_iter();
    let mut socket_path = None;
    let mut json = false;
    let mut client_command_id = None;
    let mut expected_application_revision = None;
    let mut application_revision_id = None;
    let mut aggregate_budget_micro_usd = None;
    let mut deadline_unix_millis = None;
    let mut delivery_target = None;
    let mut principal = None;
    while let Some(flag) = values.next() {
        match flag.as_str() {
            "--socket" => set_once(
                &mut socket_path,
                PathBuf::from(next_value(&mut values, "--socket")?),
                "--socket",
            )?,
            "--format" => {
                let format = next_value(&mut values, "--format")?;
                if format != "json" || json {
                    return Err("only one `--format json` is supported".to_owned());
                }
                json = true;
            }
            "--client-command-id" => set_once(
                &mut client_command_id,
                next_value(&mut values, "--client-command-id")?,
                "--client-command-id",
            )?,
            "--expected-application-revision" => set_once(
                &mut expected_application_revision,
                nonnegative_u64(
                    &next_value(&mut values, "--expected-application-revision")?,
                    "--expected-application-revision",
                )?,
                "--expected-application-revision",
            )?,
            "--application-revision-id" => set_once(
                &mut application_revision_id,
                positive_id(
                    &next_value(&mut values, "--application-revision-id")?,
                    "--application-revision-id",
                )?,
                "--application-revision-id",
            )?,
            "--aggregate-budget-micro-usd" => set_once(
                &mut aggregate_budget_micro_usd,
                nonnegative_u64(
                    &next_value(&mut values, "--aggregate-budget-micro-usd")?,
                    "--aggregate-budget-micro-usd",
                )?,
                "--aggregate-budget-micro-usd",
            )?,
            "--deadline-unix-millis" => set_once(
                &mut deadline_unix_millis,
                nonnegative_u64(
                    &next_value(&mut values, "--deadline-unix-millis")?,
                    "--deadline-unix-millis",
                )?,
                "--deadline-unix-millis",
            )?,
            "--delivery-target" => set_once(
                &mut delivery_target,
                next_value(&mut values, "--delivery-target")?
                    .parse::<u32>()
                    .ok()
                    .filter(|value| *value > 0)
                    .ok_or_else(|| "--delivery-target must be a positive integer".to_owned())?,
                "--delivery-target",
            )?,
            "--principal" => set_once(
                &mut principal,
                next_value(&mut values, "--principal")?,
                "--principal",
            )?,
            _ => return Err(format!("unknown flag {flag}")),
        }
    }
    Ok(CampaignStartArgs {
        connection: ConnectionArgs {
            socket_path: socket_path.ok_or_else(|| "--socket is required".to_owned())?,
            json,
        },
        client_command_id: client_command_id
            .ok_or_else(|| "--client-command-id is required".to_owned())?,
        expected_application_revision: expected_application_revision
            .ok_or_else(|| "--expected-application-revision is required".to_owned())?,
        application_revision_id: application_revision_id
            .ok_or_else(|| "--application-revision-id is required".to_owned())?,
        aggregate_budget_micro_usd: aggregate_budget_micro_usd
            .ok_or_else(|| "--aggregate-budget-micro-usd is required".to_owned())?,
        deadline_unix_millis: deadline_unix_millis
            .ok_or_else(|| "--deadline-unix-millis is required".to_owned())?,
        delivery_target: delivery_target
            .ok_or_else(|| "--delivery-target is required".to_owned())?,
        principal: principal.ok_or_else(|| "--principal is required".to_owned())?,
    })
}

fn parse_campaign_cancel(
    campaign_id: i64,
    arguments: Vec<String>,
) -> Result<CampaignCancelArgs, String> {
    let mut values = arguments.into_iter();
    let mut socket_path = None;
    let mut json = false;
    let mut client_command_id = None;
    let mut expected_revision = None;
    let mut principal = None;
    while let Some(flag) = values.next() {
        match flag.as_str() {
            "--socket" => set_once(
                &mut socket_path,
                PathBuf::from(next_value(&mut values, "--socket")?),
                "--socket",
            )?,
            "--format" => {
                let format = next_value(&mut values, "--format")?;
                if format != "json" || json {
                    return Err("only one `--format json` is supported".to_owned());
                }
                json = true;
            }
            "--client-command-id" => set_once(
                &mut client_command_id,
                next_value(&mut values, "--client-command-id")?,
                "--client-command-id",
            )?,
            "--expected-revision" => set_once(
                &mut expected_revision,
                nonnegative_u64(
                    &next_value(&mut values, "--expected-revision")?,
                    "--expected-revision",
                )?,
                "--expected-revision",
            )?,
            "--principal" => set_once(
                &mut principal,
                next_value(&mut values, "--principal")?,
                "--principal",
            )?,
            _ => return Err(format!("unknown flag {flag}")),
        }
    }
    Ok(CampaignCancelArgs {
        connection: ConnectionArgs {
            socket_path: socket_path.ok_or_else(|| "--socket is required".to_owned())?,
            json,
        },
        client_command_id: client_command_id
            .ok_or_else(|| "--client-command-id is required".to_owned())?,
        expected_revision: expected_revision
            .ok_or_else(|| "--expected-revision is required".to_owned())?,
        campaign_id,
        principal: principal.ok_or_else(|| "--principal is required".to_owned())?,
    })
}

fn parse_architect_base(arguments: Vec<String>) -> Result<ArchitectBaseArgs, String> {
    let mut values = arguments.into_iter();
    let mut socket_path = None;
    let mut json = false;
    let mut client_command_id = None;
    let mut expected_revision = None;
    let mut rationale_artifact_id = None;
    let mut rationale_digest = None;
    let mut rationale_byte_length = None;
    let mut principal = None;
    while let Some(flag) = values.next() {
        match flag.as_str() {
            "--socket" => set_once(
                &mut socket_path,
                PathBuf::from(next_value(&mut values, "--socket")?),
                "--socket",
            )?,
            "--format" => {
                let format = next_value(&mut values, "--format")?;
                if format != "json" || json {
                    return Err("only one `--format json` is supported".to_owned());
                }
                json = true;
            }
            "--client-command-id" => set_once(
                &mut client_command_id,
                next_value(&mut values, "--client-command-id")?,
                "--client-command-id",
            )?,
            "--expected-revision" => set_once(
                &mut expected_revision,
                nonnegative_u64(
                    &next_value(&mut values, "--expected-revision")?,
                    "--expected-revision",
                )?,
                "--expected-revision",
            )?,
            "--rationale-artifact-id" => set_once(
                &mut rationale_artifact_id,
                positive_id(
                    &next_value(&mut values, "--rationale-artifact-id")?,
                    "--rationale-artifact-id",
                )?,
                "--rationale-artifact-id",
            )?,
            "--rationale-digest" => set_once(
                &mut rationale_digest,
                next_value(&mut values, "--rationale-digest")?,
                "--rationale-digest",
            )?,
            "--rationale-byte-length" => set_once(
                &mut rationale_byte_length,
                nonnegative_u64(
                    &next_value(&mut values, "--rationale-byte-length")?,
                    "--rationale-byte-length",
                )?,
                "--rationale-byte-length",
            )?,
            "--principal" => set_once(
                &mut principal,
                next_value(&mut values, "--principal")?,
                "--principal",
            )?,
            "--reason" => {
                return Err(
                    "inline --reason is not accepted; seal the bounded rationale and pass its artifact reference"
                        .to_owned(),
                );
            }
            _ => return Err(format!("unknown flag {flag}")),
        }
    }
    Ok(ArchitectBaseArgs {
        connection: ConnectionArgs {
            socket_path: socket_path.ok_or_else(|| "--socket is required".to_owned())?,
            json,
        },
        client_command_id: client_command_id
            .ok_or_else(|| "--client-command-id is required".to_owned())?,
        expected_revision: expected_revision
            .ok_or_else(|| "--expected-revision is required".to_owned())?,
        rationale: SealedArtifactReferenceWireV1 {
            artifact_id: rationale_artifact_id
                .ok_or_else(|| "--rationale-artifact-id is required".to_owned())?,
            digest: rationale_digest.ok_or_else(|| "--rationale-digest is required".to_owned())?,
            byte_length: rationale_byte_length
                .ok_or_else(|| "--rationale-byte-length is required".to_owned())?,
        },
        principal: principal.ok_or_else(|| "--principal is required".to_owned())?,
    })
}

fn parse_candidate_decision(
    candidate_id: i64,
    arguments: Vec<String>,
) -> Result<CliCommand, String> {
    let mut decision = None;
    let mut review_id = None;
    let mut quality_override_review_id = None;
    let mut base_arguments = Vec::new();
    let mut values = arguments.into_iter();
    while let Some(flag) = values.next() {
        match flag.as_str() {
            "--deliver" => set_once(
                &mut decision,
                CandidateDecision::Deliver,
                "candidate decision",
            )?,
            "--rework" => set_once(
                &mut decision,
                CandidateDecision::Rework,
                "candidate decision",
            )?,
            "--reject" => set_once(
                &mut decision,
                CandidateDecision::Reject,
                "candidate decision",
            )?,
            "--review-id" => set_once(
                &mut review_id,
                positive_id(&next_value(&mut values, "--review-id")?, "--review-id")?,
                "--review-id",
            )?,
            "--quality-rejection-override-review-id" => set_once(
                &mut quality_override_review_id,
                positive_id(
                    &next_value(&mut values, "--quality-rejection-override-review-id")?,
                    "--quality-rejection-override-review-id",
                )?,
                "--quality-rejection-override-review-id",
            )?,
            _ => {
                base_arguments.push(flag);
                base_arguments.push(next_value(&mut values, "Architect command flag")?);
            }
        }
    }
    let decision = decision
        .ok_or_else(|| "exactly one of --deliver, --rework, or --reject is required".to_owned())?;
    if quality_override_review_id.is_some() && decision != CandidateDecision::Deliver {
        return Err(
            "--quality-rejection-override-review-id is legal only with --deliver".to_owned(),
        );
    }
    Ok(CliCommand::Decide {
        base: parse_architect_base(base_arguments)?,
        candidate_id,
        review_id: review_id.ok_or_else(|| "--review-id is required".to_owned())?,
        decision,
        quality_override_review_id,
    })
}

fn next_value(values: &mut impl Iterator<Item = String>, flag: &str) -> Result<String, String> {
    values
        .next()
        .ok_or_else(|| format!("missing value for {flag}"))
}

fn set_once<T>(slot: &mut Option<T>, value: T, flag: &str) -> Result<(), String> {
    if slot.replace(value).is_some() {
        return Err(format!("{flag} was supplied more than once"));
    }
    Ok(())
}

fn set_absolute_path(slot: &mut Option<PathBuf>, value: String, flag: &str) -> Result<(), String> {
    let path = PathBuf::from(&value);
    if value.is_empty() || value.contains('\0') || !path.is_absolute() {
        return Err(format!(
            "{flag} must be a non-empty absolute path without NUL"
        ));
    }
    set_once(slot, path, flag)
}

fn safe_relative(value: String, flag: &str) -> Result<String, String> {
    RuntimeRelativePath::parse(value)
        .map(|path| path.as_str().to_owned())
        .map_err(|error| format!("{flag} must be a safe relative path: {error}"))
}

fn parse_openrouter_credential_environment(value: String) -> Result<String, String> {
    let environment = value
        .strip_prefix("openrouter=")
        .ok_or_else(|| {
            "--provider-credential-environment must use the only MVP provider shape `openrouter=<ENVIRONMENT_NAME>`"
                .to_owned()
        })?
        .to_owned();
    CredentialDescriptorV1::Environment {
        name: environment.clone(),
    }
    .validate()
    .map_err(|_| {
        "--provider-credential-environment must name a non-empty uppercase environment variable"
            .to_owned()
    })?;
    Ok(environment)
}

fn required<T>(value: Option<T>, flag: &str) -> Result<T, String> {
    value.ok_or_else(|| format!("{flag} is required"))
}

fn positive_id(value: &str, field: &str) -> Result<i64, String> {
    let parsed = value
        .parse::<i64>()
        .map_err(|_| format!("{field} must be a positive integer"))?;
    if parsed < 1 {
        return Err(format!("{field} must be a positive integer"));
    }
    Ok(parsed)
}

fn nonnegative_u64(value: &str, field: &str) -> Result<u64, String> {
    value
        .parse::<u64>()
        .map_err(|_| format!("{field} must be a nonnegative integer"))
}

fn status_request_id() -> String {
    format!("factoryctl-status-{}", std::process::id())
}

fn architect_request_id(operation: &str) -> String {
    format!("factoryctl-architect-{operation}-{}", std::process::id())
}

fn campaign_request_id(operation: &str) -> String {
    format!("factoryctl-campaign-{operation}-{}", std::process::id())
}

fn application_request_id(operation: &str) -> String {
    format!("factoryctl-application-{operation}-{}", std::process::id())
}

fn operator_artifact_request_id() -> String {
    format!("factoryctl-operator-artifact-{}", std::process::id())
}

fn navigation_request_id(operation: &str) -> String {
    format!("factoryctl-navigation-{operation}-{}", std::process::id())
}

fn forum_request_id(operation: &str) -> String {
    format!("factoryctl-forum-{operation}-{}", std::process::id())
}

fn usage() -> &'static str {
    "usage:\n  factoryctl init --factoryd <absolute-path> --database-url <url> --runtime-root <absolute-path> --kernel-source-root <absolute-path> --kernel-source-file <safe-relative-path>... --cargo-executable <absolute-path> --git-executable <absolute-path> --deno-executable <absolute-path> --pi-host-source-root <absolute-path> --pi-host-source-file <safe-relative-path>... --pi-host-entrypoint <absolute-path> --deno-config <absolute-path> --deno-lock <absolute-path> --deno-dir <absolute-path> --dependency-graph-receipt <absolute-path> --pi-version <version> --provider-credential-environment openrouter=<UPPERCASE_ENVIRONMENT_NAME>\n  factoryctl daemon status --socket <path> [--format json]\n  factoryctl application show <key> [--application-revision-id <id>] --socket <path> [--format json]\n  factoryctl application register --socket <path> --client-command-id <id> --expected-revision <application-revision> --expected-kernel-build-revision <build-revision> --kernel-build-id <blake3> --source-root <absolute-path> --bundle-relative-path <safe-relative-path> --principal <name> [--format json]\n  factoryctl application activate <key> <revision-id> --socket <path> --client-command-id <id> --expected-revision <application-revision> --rationale-artifact-id <id> --rationale-digest <blake3> --rationale-byte-length <bytes> --principal <name> [--format json]\n  factoryctl artifact seal --socket <path> --client-command-id <id> --expected-kernel-build-revision <revision> --source-root <absolute-path> --source-relative-path <safe-relative-path> --principal <name> [--format json]\n  factoryctl campaign start --application-revision-id <id> --expected-application-revision <revision> --aggregate-budget-micro-usd <amount> --deadline-unix-millis <millis> --delivery-target <count> --socket <path> --client-command-id <id> --principal <name> [--format json]\n  factoryctl campaign status <id> --socket <path> [--format json]\n  factoryctl campaign cancel <id> --socket <path> --client-command-id <id> --expected-revision <revision> --principal <name> [--format json]\n  factoryctl ticket list [--state proposed|sponsored|in_flight|delivered|blocked|resolved|superseded|rejected] --socket <path> [--format json]\n  factoryctl ticket show <id> --socket <path> [--format json]\n  factoryctl ticket sponsor <revision> --socket <path> --client-command-id <id> --expected-revision <revision> --rationale-artifact-id <id> --rationale-digest <blake3> --rationale-byte-length <bytes> --principal <name> [--format json]\n  factoryctl ticket release <attempt> --socket <path> --client-command-id <id> --expected-revision <attempt-revision> --rationale-artifact-id <id> --rationale-digest <blake3> --rationale-byte-length <bytes> --principal <name> [--format json]\n  factoryctl candidate show <id> --socket <path> [--format json]\n  factoryctl candidate decide <candidate> --review-id <review> --deliver|--rework|--reject --socket <path> --client-command-id <id> --expected-revision <candidate-revision> --rationale-artifact-id <id> --rationale-digest <blake3> --rationale-byte-length <bytes> --principal <name> [--quality-rejection-override-review-id <review>] [--format json]\n  factoryctl audit show ticket:<id>|candidate:<id>|campaign:<id>|application-revision:<id>|audit:<id> --socket <path> [--format json]\n  factoryctl forum topics [--cursor <id>] [--limit 1..20] --socket <path> [--format json]\n  factoryctl forum threads <topic-id> [--cursor <id>] [--limit 1..20] --socket <path> [--format json]\n  factoryctl forum read <thread-id> [--after-post <id>] [--limit 1..20] --socket <path> [--format json]\n  factoryctl forum search <query> [--cursor <opaque>] [--limit 1..20] --socket <path> [--format json]\n  factoryctl forum create-topic --name <name> --description <text> --socket <path> --client-command-id <id> --expected-revision <revision> [--format json]\n  factoryctl forum create-thread --topic-id <id> --title <text> --socket <path> --client-command-id <id> --expected-revision <revision> [--format json]\n  factoryctl forum post --thread-id <id> --kind note|question|finding|proposal|challenge|correction|decision_link --body <text> [--reply-to <post-id>] [--supersedes <post-id>] [--attachment <artifact-id>:<label>]... --socket <path> --client-command-id <id> --expected-revision <revision> [--format json]"
}

#[cfg(test)]
mod tests {
    use super::*;

    fn init_arguments(factoryd: &str) -> Vec<String> {
        vec![
            "init".to_owned(),
            "--factoryd".to_owned(),
            factoryd.to_owned(),
            "--database-url".to_owned(),
            "postgresql://factory@localhost/factory_v3".to_owned(),
            "--runtime-root".to_owned(),
            "/tmp/factory-runtime".to_owned(),
            "--kernel-source-root".to_owned(),
            "/opt/factory-source".to_owned(),
            "--kernel-source-file".to_owned(),
            "crates/factoryd/src/main.rs".to_owned(),
            "--cargo-executable".to_owned(),
            "/opt/rust/bin/cargo".to_owned(),
            "--git-executable".to_owned(),
            "/opt/git/bin/git".to_owned(),
            "--deno-executable".to_owned(),
            "/opt/deno/bin/deno".to_owned(),
            "--pi-host-source-root".to_owned(),
            "/opt/factory-source".to_owned(),
            "--pi-host-source-file".to_owned(),
            "typescript/pi-host/main.ts".to_owned(),
            "--pi-host-entrypoint".to_owned(),
            "/opt/factory-source/typescript/pi-host/main.ts".to_owned(),
            "--deno-config".to_owned(),
            "/opt/factory-source/deno.json".to_owned(),
            "--deno-lock".to_owned(),
            "/opt/factory-source/deno.lock".to_owned(),
            "--deno-dir".to_owned(),
            "/opt/factory-runtime/deno-cache".to_owned(),
            "--dependency-graph-receipt".to_owned(),
            "/opt/factory-source/runtime/dependency-graph.json".to_owned(),
            "--pi-version".to_owned(),
            "0.84.1".to_owned(),
            "--provider-credential-environment".to_owned(),
            "openrouter=OPENROUTER_API_KEY".to_owned(),
        ]
    }

    fn common() -> Vec<String> {
        vec![
            "--socket".to_owned(),
            "/tmp/factory.sock".to_owned(),
            "--client-command-id".to_owned(),
            "architect-command-1".to_owned(),
            "--expected-revision".to_owned(),
            "8".to_owned(),
            "--rationale-artifact-id".to_owned(),
            "11".to_owned(),
            "--rationale-digest".to_owned(),
            "a".repeat(64),
            "--rationale-byte-length".to_owned(),
            "99".to_owned(),
            "--principal".to_owned(),
            "grand-architect".to_owned(),
        ]
    }

    #[test]
    fn daemon_status_requires_an_explicit_unix_socket_path() {
        assert!(parse_args(vec!["daemon".to_owned(), "status".to_owned()]).is_err());
        let parsed = parse_args(vec![
            "daemon".to_owned(),
            "status".to_owned(),
            "--socket".to_owned(),
            "/tmp/factory.sock".to_owned(),
            "--format".to_owned(),
            "json".to_owned(),
        ])
        .expect("valid status command");
        assert!(matches!(
            parsed,
            CliCommand::DaemonStatus(ConnectionArgs { json: true, .. })
        ));
    }

    #[test]
    fn init_is_a_closed_explicit_forwarding_contract() {
        let parsed =
            parse_args(init_arguments("/opt/factory/bin/factoryd")).expect("complete init command");
        assert!(matches!(
            parsed,
            CliCommand::Init(InitCommand {
                factoryd,
                kernel_source_files,
                pi_host_source_files,
                cargo_executable,
                git_executable,
                ..
            }) if factoryd == PathBuf::from("/opt/factory/bin/factoryd")
                && kernel_source_files == vec!["crates/factoryd/src/main.rs".to_owned()]
                && pi_host_source_files == vec!["typescript/pi-host/main.ts".to_owned()]
                && cargo_executable == PathBuf::from("/opt/rust/bin/cargo")
                && git_executable == PathBuf::from("/opt/git/bin/git")
        ));

        let mut kernel_binary = init_arguments("/opt/factory/bin/factoryd");
        kernel_binary.extend(["--kernel-binary".to_owned(), "/other/factoryd".to_owned()]);
        assert!(parse_args(kernel_binary).is_err());

        let mut unsafe_source = init_arguments("/opt/factory/bin/factoryd");
        unsafe_source.extend([
            "--kernel-source-file".to_owned(),
            "../outside.rs".to_owned(),
        ]);
        assert!(parse_args(unsafe_source).is_err());

        let mut unsupported_credential = init_arguments("/opt/factory/bin/factoryd");
        let credential = unsupported_credential
            .iter()
            .position(|value| value == "--provider-credential-environment")
            .expect("credential flag");
        unsupported_credential[credential + 1] = "other=OTHER_KEY".to_owned();
        assert!(parse_args(unsupported_credential).is_err());
    }

    #[cfg(unix)]
    #[test]
    fn init_spawns_only_the_explicit_factoryd_and_binds_its_kernel_binary() {
        use std::{
            os::unix::fs::PermissionsExt,
            time::{SystemTime, UNIX_EPOCH},
        };

        let unique = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("clock after epoch")
            .as_nanos();
        let root = std::env::temp_dir().join(format!(
            "factoryctl-init-child-{}-{unique}",
            std::process::id()
        ));
        fs::create_dir_all(&root).expect("temporary root");
        let factoryd = root.join("factoryd");
        fs::write(
            &factoryd,
            "#!/bin/sh\nprintf '%s\\n' \"$@\" > \"$(dirname \"$0\")/received-arguments\"\n",
        )
        .expect("fake factoryd");
        let mut permissions = fs::metadata(&factoryd)
            .expect("fake metadata")
            .permissions();
        permissions.set_mode(0o700);
        fs::set_permissions(&factoryd, permissions).expect("make fake executable");

        let parsed = parse_args(init_arguments(
            factoryd.to_str().expect("temporary path is UTF-8"),
        ))
        .expect("parse fake child command");
        let CliCommand::Init(command) = parsed else {
            panic!("expected init command");
        };
        spawn_factoryd_init(&command).expect("one-shot fake child succeeds");

        let arguments = fs::read_to_string(root.join("received-arguments"))
            .expect("fake child received arguments");
        let arguments: Vec<_> = arguments.lines().collect();
        assert_eq!(arguments.first(), Some(&"init"));
        assert_argument_value(
            &arguments,
            "--kernel-binary",
            fs::canonicalize(&factoryd)
                .expect("canonical fake binary")
                .to_str()
                .expect("temporary path is UTF-8"),
        );
        assert_argument_value(
            &arguments,
            "--database-url",
            "postgresql://factory@localhost/factory_v3",
        );
        assert_argument_value(&arguments, "--cargo-executable", "/opt/rust/bin/cargo");
        assert_argument_value(&arguments, "--git-executable", "/opt/git/bin/git");
        assert_argument_value(&arguments, "--pi-version", "0.84.1");
        assert_argument_value(
            &arguments,
            "--provider-credential-environment",
            "openrouter=OPENROUTER_API_KEY",
        );
        fs::remove_dir_all(root).expect("remove temporary root");
    }

    #[cfg(unix)]
    fn assert_argument_value(arguments: &[&str], flag: &str, expected: &str) {
        let index = arguments
            .iter()
            .position(|value| *value == flag)
            .expect("required child flag");
        assert_eq!(arguments.get(index + 1), Some(&expected));
    }

    #[test]
    fn sponsor_requires_a_sealed_rationale_and_explicit_revision() {
        let mut command = vec!["ticket".to_owned(), "sponsor".to_owned(), "4".to_owned()];
        command.extend(common());
        let parsed = parse_args(command).expect("valid sponsorship command");
        assert!(matches!(
            parsed,
            CliCommand::Sponsor {
                ticket_revision_id: 4,
                base: ArchitectBaseArgs {
                    expected_revision: 8,
                    ..
                },
            }
        ));
        assert!(
            parse_args(vec![
                "ticket".to_owned(),
                "sponsor".to_owned(),
                "4".to_owned(),
                "--reason".to_owned(),
                "inline text".to_owned(),
            ])
            .is_err()
        );
    }

    #[test]
    fn candidate_decision_allows_only_one_qualitative_override_shape() {
        let mut command = vec![
            "candidate".to_owned(),
            "decide".to_owned(),
            "12".to_owned(),
            "--review-id".to_owned(),
            "13".to_owned(),
            "--deliver".to_owned(),
            "--quality-rejection-override-review-id".to_owned(),
            "13".to_owned(),
        ];
        command.extend(common());
        assert!(matches!(
            parse_args(command),
            Ok(CliCommand::Decide {
                decision: CandidateDecision::Deliver,
                quality_override_review_id: Some(13),
                ..
            })
        ));

        let mut invalid = vec![
            "candidate".to_owned(),
            "decide".to_owned(),
            "12".to_owned(),
            "--review-id".to_owned(),
            "13".to_owned(),
            "--rework".to_owned(),
            "--quality-rejection-override-review-id".to_owned(),
            "13".to_owned(),
        ];
        invalid.extend(common());
        assert!(parse_args(invalid).is_err());
    }

    #[test]
    fn campaign_routes_are_socket_only_and_preserve_revision_guards() {
        let start = parse_args(vec![
            "campaign".to_owned(),
            "start".to_owned(),
            "--application-revision-id".to_owned(),
            "7".to_owned(),
            "--expected-application-revision".to_owned(),
            "3".to_owned(),
            "--aggregate-budget-micro-usd".to_owned(),
            "250000".to_owned(),
            "--deadline-unix-millis".to_owned(),
            "4000000000000".to_owned(),
            "--delivery-target".to_owned(),
            "2".to_owned(),
            "--socket".to_owned(),
            "/tmp/factory.sock".to_owned(),
            "--client-command-id".to_owned(),
            "campaign-start-1".to_owned(),
            "--principal".to_owned(),
            "operator".to_owned(),
        ])
        .expect("campaign start");
        assert!(matches!(
            start,
            CliCommand::CampaignStart(CampaignStartArgs {
                application_revision_id: 7,
                expected_application_revision: 3,
                ..
            })
        ));

        assert!(matches!(
            parse_args(vec![
                "campaign".to_owned(),
                "status".to_owned(),
                "8".to_owned(),
                "--socket".to_owned(),
                "/tmp/factory.sock".to_owned(),
                "--format".to_owned(),
                "json".to_owned(),
            ]),
            Ok(CliCommand::CampaignStatus {
                campaign_id: 8,
                connection: ConnectionArgs { json: true, .. },
            })
        ));

        let cancel = parse_args(vec![
            "campaign".to_owned(),
            "cancel".to_owned(),
            "8".to_owned(),
            "--socket".to_owned(),
            "/tmp/factory.sock".to_owned(),
            "--client-command-id".to_owned(),
            "campaign-cancel-1".to_owned(),
            "--expected-revision".to_owned(),
            "4".to_owned(),
            "--principal".to_owned(),
            "operator".to_owned(),
        ])
        .expect("campaign cancel");
        assert!(matches!(
            cancel,
            CliCommand::CampaignCancel(CampaignCancelArgs {
                campaign_id: 8,
                expected_revision: 4,
                ..
            })
        ));

        assert!(
            parse_args(vec![
                "campaign".to_owned(),
                "status".to_owned(),
                "8".to_owned(),
                "--database-url".to_owned(),
                "postgresql://not-permitted".to_owned(),
            ])
            .is_err()
        );
    }

    #[test]
    fn application_routes_keep_source_bytes_out_of_factoryctl() {
        let show = parse_args(vec![
            "application".to_owned(),
            "show".to_owned(),
            "example".to_owned(),
            "--application-revision-id".to_owned(),
            "9".to_owned(),
            "--socket".to_owned(),
            "/tmp/factory.sock".to_owned(),
            "--format".to_owned(),
            "json".to_owned(),
        ])
        .expect("application show");
        assert!(matches!(
            show,
            CliCommand::ApplicationShow(ApplicationShowArgs {
                application_key,
                application_revision_id: Some(9),
                connection: ConnectionArgs { json: true, .. },
            }) if application_key == "example"
        ));

        let register = parse_args(vec![
            "application".to_owned(),
            "register".to_owned(),
            "--socket".to_owned(),
            "/tmp/factory.sock".to_owned(),
            "--client-command-id".to_owned(),
            "application-register-1".to_owned(),
            "--expected-revision".to_owned(),
            "0".to_owned(),
            "--expected-kernel-build-revision".to_owned(),
            "1".to_owned(),
            "--kernel-build-id".to_owned(),
            "a".repeat(64),
            "--source-root".to_owned(),
            "/workspace/application-source".to_owned(),
            "--bundle-relative-path".to_owned(),
            "bundle.json".to_owned(),
            "--principal".to_owned(),
            "grand-architect".to_owned(),
        ])
        .expect("application register");
        assert!(matches!(
            register,
            CliCommand::ApplicationRegister(ApplicationRegisterArgs {
                expected_revision: 0,
                expected_kernel_build_revision: 1,
                bundle_relative_path,
                ..
            }) if bundle_relative_path == "bundle.json"
        ));

        let mut activate = vec![
            "application".to_owned(),
            "activate".to_owned(),
            "example".to_owned(),
            "9".to_owned(),
        ];
        activate.extend(common());
        assert!(matches!(
            parse_args(activate),
            Ok(CliCommand::ApplicationActivate(ApplicationActivateArgs {
                application_key,
                application_revision_id: 9,
                ..
            })) if application_key == "example"
        ));

        assert!(
            parse_args(vec![
                "application".to_owned(),
                "register".to_owned(),
                "--source-root".to_owned(),
                "relative".to_owned(),
            ])
            .is_err()
        );
    }

    #[test]
    fn read_only_navigation_and_forum_routes_are_socket_only() {
        assert!(matches!(
            parse_args(vec![
                "ticket".to_owned(), "list".to_owned(), "--state".to_owned(),
                "sponsored".to_owned(), "--socket".to_owned(), "/tmp/factory.sock".to_owned(),
                "--format".to_owned(), "json".to_owned(),
            ]),
            Ok(CliCommand::TicketList(TicketListArgs { state: Some(state), connection: ConnectionArgs { json: true, .. } })) if state == "sponsored"
        ));
        assert!(matches!(
            parse_args(vec![
                "candidate".to_owned(),
                "show".to_owned(),
                "4".to_owned(),
                "--socket".to_owned(),
                "/tmp/factory.sock".to_owned()
            ]),
            Ok(CliCommand::CandidateShow {
                candidate_id: 4,
                ..
            })
        ));
        assert!(
            parse_args(vec![
                "audit".to_owned(),
                "show".to_owned(),
                "ticket:1".to_owned(),
                "--database-url".to_owned(),
                "postgresql://forbidden".to_owned()
            ])
            .is_err()
        );
        assert!(matches!(
            parse_args(vec![
                "forum".to_owned(),
                "create-topic".to_owned(),
                "--name".to_owned(),
                "updates".to_owned(),
                "--description".to_owned(),
                "bounded".to_owned(),
                "--socket".to_owned(),
                "/tmp/factory.sock".to_owned(),
                "--client-command-id".to_owned(),
                "forum-topic-1".to_owned(),
                "--expected-revision".to_owned(),
                "0".to_owned(),
            ]),
            Ok(CliCommand::ForumCreateTopic { .. })
        ));
        assert!(
            parse_args(vec![
                "forum".to_owned(),
                "post".to_owned(),
                "--thread-id".to_owned(),
                "1".to_owned(),
                "--kind".to_owned(),
                "outside".to_owned()
            ])
            .is_err()
        );
    }
}
