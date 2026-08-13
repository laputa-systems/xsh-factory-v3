//! Product proposal execution at the trusted actor boundary.
//!
//! Product may supply bounded proposal data and references to artifacts it
//! sealed through its inherited session connection. It may not supply trusted
//! command observations. This module resolves the exact admitted application
//! bundle, verifies every referenced artifact in PostgreSQL and CAS, runs the
//! sealed command twice against the assigned clean snapshot, seals the actual
//! observations, executes the bounded live-buffer duplicate query, and only
//! then asks [`TicketStore`] to persist a proposal.

use std::path::Path;

use factory_protocol::{
    ApplicationRevisionId, ApprovedToolV1, ArtifactId, CommandObservationV1, CommandProfileV1,
    DurationMillis, ExecutableV1, ExpectedRevision, KernelBuildId, ProductSubmitTicketRequest,
    ProductTicketProposalV1, RepositoryRelativePath, SealedArtifactReferenceV1,
    canonical_product_ticket_proposal_json_v1, parse_application_bundle_v1,
    parse_command_profile_v1,
};
use miniserde::{Serialize, json};
use thiserror::Error;

use crate::{
    cas::{CasArtifact, CasStore},
    command_supervision::{
        CommandExpectation, CommandReceipt, CommandRunner, CommandStdin, CommandSupervisionError,
        CommandTerminal, CommandWorkspace, ComparisonRevision, DeterministicCommand,
        DiscoveryClassification, ExactBytes,
    },
    process::ProcessStore,
    storage::StoreError,
    ticket_store::{SubmitTicketProposal, TicketProposalReceipt, TicketStore},
};

const PRODUCT_PROPOSAL_COMPARISON_REVISION: &str = "exact-observation-v1";

#[derive(Debug, Error)]
pub enum ProductRuntimeError {
    #[error(transparent)]
    Store(#[from] StoreError),
    #[error(transparent)]
    Command(#[from] CommandSupervisionError),
    #[error("the admitted application bundle is invalid: {0}")]
    ApplicationBundle(String),
    #[error("Product proposal contract is invalid: {0}")]
    ProposalContract(String),
    #[error("Product referenced an artifact whose registered identity does not match")]
    ArtifactIdentityMismatch,
    #[error("Product named a reproducer profile that is not in the admitted application revision")]
    UnknownReproducerProfile,
    #[error("the sealed reproducer command differs from its named admitted profile")]
    ReproducerProfileMismatch,
    #[error("kernel-owned discovery did not reproduce one exact failure twice")]
    ReproducerNotStableFailure,
    #[error("Product-supplied observations differ from kernel-owned observations")]
    UntrustedObservationMismatch,
    #[error("the duplicate query matches live ticket proposal {artifact_id}")]
    DuplicateLiveProposal { artifact_id: ArtifactId },
}

#[derive(Clone, Debug)]
pub struct ExecuteProductProposal<'a> {
    pub principal: &'a str,
    pub request: &'a ProductSubmitTicketRequest,
    pub application_revision_id: ApplicationRevisionId,
    pub kernel_build_id: KernelBuildId,
    pub workspace_root: &'a Path,
}

pub async fn execute_product_proposal(
    process: &ProcessStore,
    tickets: &TicketStore,
    cas: &CasStore,
    runner: &CommandRunner,
    input: ExecuteProductProposal<'_>,
) -> Result<TicketProposalReceipt, ProductRuntimeError> {
    let context = tickets
        .proposal_admission_context(input.application_revision_id)
        .await?;
    let bundle_bytes = registered_bytes(process, cas, context.bundle_artifact_id).await?;
    let bundle = parse_application_bundle_v1(&bundle_bytes)
        .map_err(|error| ProductRuntimeError::ApplicationBundle(error.to_string()))?;
    let proposal = input
        .request
        .proposal(&context.ticket_bounds)
        .map_err(|error| ProductRuntimeError::ProposalContract(error.to_string()))?;
    verify_proposal_artifacts(process, cas, input.principal, &proposal).await?;

    let command_bytes =
        registered_reference_bytes(process, cas, input.principal, &proposal.reproducer.command)
            .await?;
    let sealed_profile = parse_command_profile_v1(&command_bytes)
        .map_err(|error| ProductRuntimeError::ProposalContract(error.to_string()))?;
    let admitted_profile = bundle
        .reproducer_profiles
        .iter()
        .find(|profile| profile.name == proposal.reproducer_profile)
        .ok_or(ProductRuntimeError::UnknownReproducerProfile)?;
    if admitted_profile != &sealed_profile {
        return Err(ProductRuntimeError::ReproducerProfileMismatch);
    }

    let expected_stdout = expected_bytes(
        process,
        cas,
        input.principal,
        &proposal.reproducer.expected_observation.stdout,
    )
    .await?;
    let expected_stderr = expected_bytes(
        process,
        cas,
        input.principal,
        &proposal.reproducer.expected_observation.stderr,
    )
    .await?;
    let command_stdin = match &proposal.reproducer.stdin {
        Some(stdin) => {
            CommandStdin::Artifact(expected_bytes(process, cas, input.principal, stdin).await?)
        }
        None => CommandStdin::Empty,
    };
    let mut profile = sealed_profile;
    profile.expected_exit_status = proposal.reproducer.expected_observation.exit_status;
    let command = DeterministicCommand::new(
        profile,
        command_stdin,
        CommandExpectation::new(
            ComparisonRevision::parse(PRODUCT_PROPOSAL_COMPARISON_REVISION)?,
            Some(expected_stdout),
            Some(expected_stderr),
        ),
    )?;
    let workspace = CommandWorkspace::open(input.workspace_root)?;
    let (discovery_commit, discovery_tree) = discover_clean_snapshot(runner, &workspace)?;
    let reproduction = runner.run_discovery_reproducer(&workspace, &command)?;
    if reproduction.classification() != DiscoveryClassification::ReproducibleFailure {
        return Err(ProductRuntimeError::ReproducerNotStableFailure);
    }
    if discover_clean_snapshot(runner, &workspace)?
        != (discovery_commit.clone(), discovery_tree.clone())
    {
        return Err(ProductRuntimeError::ProposalContract(
            "discovery reproducer changed the clean snapshot".to_owned(),
        ));
    }

    let first = seal_observation(
        process,
        cas,
        input.principal,
        &format!("{}-discovery-first", input.request.client_command_id),
        input.kernel_build_id,
        reproduction.first(),
    )
    .await?;
    let second = seal_observation(
        process,
        cas,
        input.principal,
        &format!("{}-discovery-second", input.request.client_command_id),
        input.kernel_build_id,
        reproduction.second(),
    )
    .await?;
    if !same_observation(&first.observation, &proposal.reproducer.first_observation)
        || !same_observation(&second.observation, &proposal.reproducer.second_observation)
    {
        return Err(ProductRuntimeError::UntrustedObservationMismatch);
    }

    let proposal_bytes = canonical_product_ticket_proposal_json_v1(input.request);
    execute_duplicate_query(
        process,
        tickets,
        cas,
        input.application_revision_id,
        &proposal.duplicate_search.query,
        proposal.duplicate_search.limit,
        &proposal_bytes,
    )
    .await?;
    let (_, proposal_receipt) = process
        .adopt_and_register_kernel_bytes(
            cas,
            input.principal,
            &format!("{}-proposal-envelope", input.request.client_command_id),
            input.kernel_build_id,
            &proposal_bytes,
        )
        .await?;
    let (_, reproducer_receipt) = process
        .adopt_and_register_kernel_bytes(
            cas,
            input.principal,
            &format!("{}-reproducer-contract", input.request.client_command_id),
            input.kernel_build_id,
            &command_bytes,
        )
        .await?;
    let expected_manifest_artifact_id = seal_existing_observation_manifest(
        process,
        cas,
        input.principal,
        &format!("{}-expected-observation", input.request.client_command_id),
        input.kernel_build_id,
        &proposal.reproducer.expected_observation,
    )
    .await?;

    tickets
        .submit_ticket_proposal(&SubmitTicketProposal {
            principal: input.principal.to_owned(),
            command_id: input.request.client_command_id.clone(),
            expected_application_revision: ExpectedRevision::new(context.aggregate_revision),
            application_revision_id: input.application_revision_id,
            proposal_artifact_id: proposal_receipt.artifact_id,
            reproducer_artifact_id: reproducer_receipt.artifact_id,
            expected_observation_artifact_id: expected_manifest_artifact_id,
            first_actual_observation_artifact_id: first.manifest_artifact_id,
            second_actual_observation_artifact_id: second.manifest_artifact_id,
            discovery_commit,
            discovery_tree,
        })
        .await
        .map_err(ProductRuntimeError::from)
}

fn discover_clean_snapshot(
    runner: &CommandRunner,
    workspace: &CommandWorkspace,
) -> Result<(String, String), ProductRuntimeError> {
    // Product seals its report and reproducer bytes from the assigned
    // workspace. Those untracked evidence files are not product-source
    // mutations; tracked changes remain a fail-closed discovery violation.
    let status = git_probe(
        runner,
        workspace,
        &["status", "--porcelain=v1", "--untracked-files=no"],
    )?;
    if !status.is_empty() {
        return Err(ProductRuntimeError::ProposalContract(
            "discovery workspace is not clean".to_owned(),
        ));
    }
    let commit = git_probe(
        runner,
        workspace,
        &["rev-parse", "--verify", "HEAD^{commit}"],
    )?;
    let tree = git_probe(runner, workspace, &["rev-parse", "--verify", "HEAD^{tree}"])?;
    for value in [&commit, &tree] {
        if !matches!(value.len(), 40 | 64)
            || !value
                .bytes()
                .all(|byte| byte.is_ascii_digit() || matches!(byte, b'a'..=b'f'))
        {
            return Err(ProductRuntimeError::ProposalContract(
                "discovery Git identity is invalid".to_owned(),
            ));
        }
    }
    Ok((commit, tree))
}

fn git_probe(
    runner: &CommandRunner,
    workspace: &CommandWorkspace,
    argv: &[&str],
) -> Result<String, ProductRuntimeError> {
    let command = DeterministicCommand::new(
        CommandProfileV1 {
            name: format!("product-discovery-git-{}", argv[0]),
            executable: ExecutableV1::ApprovedTool(ApprovedToolV1::Git),
            argv: argv.iter().map(|value| (*value).to_owned()).collect(),
            working_directory: RepositoryRelativePath::parse(".")
                .expect("static repository root path is valid"),
            environment: Vec::new(),
            timeout: DurationMillis::new(30_000),
            stdout_byte_limit: 64 * 1024,
            stderr_byte_limit: 64 * 1024,
            expected_exit_status: 0,
        },
        CommandStdin::Empty,
        CommandExpectation::new(
            ComparisonRevision::parse(PRODUCT_PROPOSAL_COMPARISON_REVISION)?,
            None,
            None,
        ),
    )?;
    let receipt = runner.run(workspace, &command)?;
    if !receipt.matches_expectation() {
        return Err(ProductRuntimeError::ProposalContract(
            "discovery Git probe failed".to_owned(),
        ));
    }
    let output = std::str::from_utf8(receipt.stdout()).map_err(|_| {
        ProductRuntimeError::ProposalContract("discovery Git output is not UTF-8".to_owned())
    })?;
    Ok(output.strip_suffix('\n').unwrap_or(output).to_owned())
}

async fn registered_bytes(
    process: &ProcessStore,
    cas: &CasStore,
    artifact_id: ArtifactId,
) -> Result<Vec<u8>, ProductRuntimeError> {
    let seal = process.registered_artifact(cas, artifact_id).await?;
    cas.read_verified(seal.digest())
        .map_err(StoreError::from)
        .map_err(Into::into)
}

async fn registered_reference_bytes(
    process: &ProcessStore,
    cas: &CasStore,
    principal: &str,
    reference: &SealedArtifactReferenceV1,
) -> Result<Vec<u8>, ProductRuntimeError> {
    let seal = process
        .registered_artifact_for_principal(cas, principal, reference.artifact_id)
        .await?;
    if seal.digest() != reference.digest || seal.byte_length() != reference.byte_length {
        return Err(ProductRuntimeError::ArtifactIdentityMismatch);
    }
    cas.read_verified(seal.digest())
        .map_err(StoreError::from)
        .map_err(Into::into)
}

async fn expected_bytes(
    process: &ProcessStore,
    cas: &CasStore,
    principal: &str,
    reference: &SealedArtifactReferenceV1,
) -> Result<ExactBytes, ProductRuntimeError> {
    Ok(ExactBytes::from_artifact(
        reference.digest,
        registered_reference_bytes(process, cas, principal, reference).await?,
    )?)
}

async fn verify_proposal_artifacts(
    process: &ProcessStore,
    cas: &CasStore,
    principal: &str,
    proposal: &ProductTicketProposalV1,
) -> Result<(), ProductRuntimeError> {
    for reference in proposal_artifacts(proposal) {
        let _ = registered_reference_bytes(process, cas, principal, reference).await?;
    }
    Ok(())
}

fn proposal_artifacts(proposal: &ProductTicketProposalV1) -> Vec<&SealedArtifactReferenceV1> {
    let mut artifacts = vec![
        &proposal.narrative,
        &proposal.evidence,
        &proposal.reproducer.command,
        &proposal.reproducer.expected_observation.stdout,
        &proposal.reproducer.expected_observation.stderr,
        &proposal.reproducer.first_observation.stdout,
        &proposal.reproducer.first_observation.stderr,
        &proposal.reproducer.second_observation.stdout,
        &proposal.reproducer.second_observation.stderr,
    ];
    if let Some(stdin) = &proposal.reproducer.stdin {
        artifacts.push(stdin);
    }
    artifacts
}

#[derive(Serialize)]
struct ObservationBytes<'a> {
    exit_status: i32,
    stdout_digest: &'a str,
    stdout_byte_length: u64,
    stderr_digest: &'a str,
    stderr_byte_length: u64,
}

struct SealedObservation {
    observation: CommandObservationV1,
    manifest_artifact_id: ArtifactId,
}

async fn seal_observation(
    process: &ProcessStore,
    cas: &CasStore,
    principal: &str,
    command_id: &str,
    kernel_build_id: KernelBuildId,
    receipt: &CommandReceipt,
) -> Result<SealedObservation, ProductRuntimeError> {
    let stdout = seal_bytes(
        process,
        cas,
        principal,
        &format!("{command_id}-stdout"),
        kernel_build_id,
        receipt.stdout(),
    )
    .await?;
    let stderr = seal_bytes(
        process,
        cas,
        principal,
        &format!("{command_id}-stderr"),
        kernel_build_id,
        receipt.stderr(),
    )
    .await?;
    let exit_status = match receipt.terminal() {
        CommandTerminal::Exited { exit_code } => exit_code,
        _ => return Err(ProductRuntimeError::ReproducerNotStableFailure),
    };
    let stdout_digest = stdout.digest.to_hex();
    let stderr_digest = stderr.digest.to_hex();
    let manifest = observation_manifest_bytes(
        exit_status,
        &stdout_digest,
        stdout.byte_length,
        &stderr_digest,
        stderr.byte_length,
    );
    let (_, manifest_receipt) = process
        .adopt_and_register_kernel_bytes(
            cas,
            principal,
            &format!("{command_id}-manifest"),
            kernel_build_id,
            &manifest,
        )
        .await?;
    Ok(SealedObservation {
        observation: CommandObservationV1 {
            exit_status,
            stdout,
            stderr,
        },
        manifest_artifact_id: manifest_receipt.artifact_id,
    })
}

async fn seal_existing_observation_manifest(
    process: &ProcessStore,
    cas: &CasStore,
    principal: &str,
    command_id: &str,
    kernel_build_id: KernelBuildId,
    observation: &CommandObservationV1,
) -> Result<ArtifactId, ProductRuntimeError> {
    let stdout_digest = observation.stdout.digest.to_hex();
    let stderr_digest = observation.stderr.digest.to_hex();
    let bytes = observation_manifest_bytes(
        observation.exit_status,
        &stdout_digest,
        observation.stdout.byte_length,
        &stderr_digest,
        observation.stderr.byte_length,
    );
    let (_, receipt) = process
        .adopt_and_register_kernel_bytes(cas, principal, command_id, kernel_build_id, &bytes)
        .await?;
    Ok(receipt.artifact_id)
}

fn observation_manifest_bytes(
    exit_status: i32,
    stdout_digest: &str,
    stdout_byte_length: u64,
    stderr_digest: &str,
    stderr_byte_length: u64,
) -> Vec<u8> {
    json::to_string(&ObservationBytes {
        exit_status,
        stdout_digest,
        stdout_byte_length,
        stderr_digest,
        stderr_byte_length,
    })
    .into_bytes()
}

async fn seal_bytes(
    process: &ProcessStore,
    cas: &CasStore,
    principal: &str,
    command_id: &str,
    kernel_build_id: KernelBuildId,
    bytes: &[u8],
) -> Result<SealedArtifactReferenceV1, ProductRuntimeError> {
    let (seal, receipt) = process
        .adopt_and_register_kernel_bytes(cas, principal, command_id, kernel_build_id, bytes)
        .await?;
    Ok(reference(receipt.artifact_id, seal))
}

fn reference(artifact_id: ArtifactId, seal: CasArtifact) -> SealedArtifactReferenceV1 {
    SealedArtifactReferenceV1 {
        artifact_id,
        digest: seal.digest(),
        byte_length: seal.byte_length(),
    }
}

fn same_observation(left: &CommandObservationV1, right: &CommandObservationV1) -> bool {
    left.exit_status == right.exit_status
        && left.stdout.digest == right.stdout.digest
        && left.stdout.byte_length == right.stdout.byte_length
        && left.stderr.digest == right.stderr.digest
        && left.stderr.byte_length == right.stderr.byte_length
}

async fn execute_duplicate_query(
    process: &ProcessStore,
    tickets: &TicketStore,
    cas: &CasStore,
    application_revision_id: ApplicationRevisionId,
    query: &str,
    limit: u8,
    current_proposal: &[u8],
) -> Result<(), ProductRuntimeError> {
    let terms: Vec<String> = query
        .split(|character: char| !character.is_alphanumeric())
        .filter(|term| term.len() >= 3)
        .map(str::to_lowercase)
        .collect();
    if terms.is_empty() {
        return Err(ProductRuntimeError::ProposalContract(
            "duplicate search query has no searchable term".to_owned(),
        ));
    }
    let current = String::from_utf8_lossy(current_proposal).to_lowercase();
    for candidate in tickets
        .live_ticket_proposal_artifacts(application_revision_id)
        .await?
        .into_iter()
        .take(usize::from(limit))
    {
        let bytes = registered_bytes(process, cas, candidate.proposal_artifact_id).await?;
        let text = String::from_utf8_lossy(&bytes).to_lowercase();
        if terms.iter().all(|term| text.contains(term))
            && terms.iter().all(|term| current.contains(term))
        {
            return Err(ProductRuntimeError::DuplicateLiveProposal {
                artifact_id: candidate.proposal_artifact_id,
            });
        }
    }
    Ok(())
}
