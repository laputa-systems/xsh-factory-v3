//! The closed, Rust-native application contract.
//!
//! An application is inert policy.  It names source artifacts and declares
//! the bounded capabilities which an actor may receive; it does not contain
//! callbacks, executable code, or an authority escape hatch.  Source bytes
//! are sealed by [`ApplicationCompilerV2`] before admission and are never
//! loaded from the application checkout by a running actor.

use std::collections::{BTreeMap, BTreeSet};

use crate::{
    AbsoluteHostPath, ApplicationRelativePath, AssignmentRole, ContentDigest, ContractError,
    DurationMillis, MicroUsd, RepositoryRelativePath,
};

/// The only application bundle format admitted by the Rust runtime.
pub const APPLICATION_BUNDLE_V2_FORMAT: u16 = 2;

/// Maximum bytes for a single application source artifact.  Individual
/// templates and policies may choose a lower bound in their declaration.
pub const MAX_APPLICATION_ARTIFACT_BYTES: usize = 8 * 1024 * 1024;
/// Policies are intentionally much smaller than templates: a policy declares
/// tools and handlers, it is not a place to hide an application.
pub const MAX_POLICY_ARTIFACT_BYTES: usize = 1024 * 1024;
/// One session transcript and its terminal streams must each fit one CAS object.
pub const MAX_SESSION_OUTPUT_BYTES: u64 = 16 * 1024 * 1024;
const MAX_APPLICATION_SOURCE_PATH_BYTES: usize = 240;

/// Canonical, closed application policy understood by the generic kernel.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ApplicationBundleV2 {
    pub format_version: u16,
    pub application_key: ApplicationKey,
    pub predecessor_bundle: Option<ContentDigest>,
    pub repository: RepositoryBindingV2,
    pub mission_template: TemplateArtifactV2,
    pub assignment_role_profiles: Vec<AssignmentRoleProfileV2>,
    pub ticket_policy: TicketPolicyV2,
    pub required_reads: Vec<RequiredReadV2>,
    pub reproducer_profiles: Vec<CommandProfileV2>,
    pub validation_profiles: ValidationProfilesV2,
    pub git_policy: GitPolicyV2,
    pub commit_message_policy: CommitMessagePolicyV2,
}

impl ApplicationBundleV2 {
    pub fn validate(&self) -> Result<(), ContractError> {
        if self.format_version != APPLICATION_BUNDLE_V2_FORMAT {
            return Err(bundle_error(
                "bundle format",
                "format_version is not the V2 format",
            ));
        }
        self.application_key.validate()?;
        self.repository.validate()?;
        self.mission_template.validate("mission_template")?;

        if self.assignment_role_profiles.len() != AssignmentRole::ALL.len() {
            return Err(bundle_error(
                "fixed assignment roles",
                "exactly one profile is required for each fixed assignment role",
            ));
        }
        let mut roles = BTreeSet::new();
        for profile in &self.assignment_role_profiles {
            profile.validate()?;
            if !roles.insert(profile.assignment_role) {
                return Err(bundle_error(
                    "fixed assignment roles",
                    "assignment-role profile is duplicated",
                ));
            }
        }
        if !AssignmentRole::ALL
            .into_iter()
            .all(|role| roles.contains(&role))
        {
            return Err(bundle_error(
                "fixed assignment roles",
                "an assignment-role profile is missing",
            ));
        }

        self.ticket_policy.validate()?;
        validate_required_reads(&self.required_reads)?;
        validate_commands("reproducer_profiles", &self.reproducer_profiles, false)?;
        self.validation_profiles.validate()?;
        self.git_policy.validate()?;
        self.commit_message_policy.validate()?;
        Ok(())
    }
}

#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[repr(transparent)]
pub struct ApplicationKey(String);

impl ApplicationKey {
    pub fn parse(value: impl Into<String>) -> Result<Self, ContractError> {
        let value = value.into();
        if value.is_empty() || value.len() > 80 {
            return Err(ContractError::InvalidValue {
                field: "application key",
                reason: "must contain 1 through 80 bytes",
            });
        }
        if !value
            .bytes()
            .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'-')
        {
            return Err(ContractError::InvalidValue {
                field: "application key",
                reason: "must use lower-case ASCII letters, digits, or hyphens",
            });
        }
        Ok(Self(value))
    }

    fn validate(&self) -> Result<(), ContractError> {
        Self::parse(self.0.clone()).map(|_| ())
    }

    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RepositoryBindingV2 {
    pub repository_key: String,
    pub canonical_local_path: AbsoluteHostPath,
    pub default_branch: String,
    pub delivery_mode: DeliveryModeV2,
}

impl RepositoryBindingV2 {
    fn validate(&self) -> Result<(), ContractError> {
        validate_nonempty_bounded("repository_key", &self.repository_key, 160)?;
        validate_nonempty_bounded("default_branch", &self.default_branch, 240)?;
        if self.default_branch.contains(char::is_whitespace)
            || self.default_branch.contains("..")
            || self.default_branch.ends_with('/')
        {
            return Err(bundle_error(
                "repository binding",
                "default branch is not a safe Git reference name",
            ));
        }
        if self.delivery_mode != DeliveryModeV2::LocalFastForwardOnly {
            return Err(bundle_error(
                "repository binding",
                "only guarded local fast-forward delivery is admitted",
            ));
        }
        Ok(())
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DeliveryModeV2 {
    LocalFastForwardOnly,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TemplateArtifactV2 {
    pub source_path: ApplicationRelativePath,
    pub digest: ContentDigest,
    pub placeholders: Vec<TemplatePlaceholderV2>,
    pub rendered_byte_limit: u32,
}

impl TemplateArtifactV2 {
    fn validate(&self, field: &'static str) -> Result<(), ContractError> {
        validate_application_source_path(&self.source_path, field, false)?;
        if self.source_path.as_str().starts_with("policies/") {
            return Err(bundle_error(
                field,
                "template source must not share the policy source namespace",
            ));
        }
        if self.rendered_byte_limit == 0
            || self.rendered_byte_limit as usize > MAX_APPLICATION_ARTIFACT_BYTES
        {
            return Err(bundle_error(
                field,
                "rendered byte limit must be positive and within the application ceiling",
            ));
        }
        let mut placeholders = BTreeSet::new();
        for placeholder in &self.placeholders {
            placeholder.validate()?;
            if !placeholders.insert(placeholder.as_str()) {
                return Err(bundle_error(field, "template placeholder is duplicated"));
            }
        }
        Ok(())
    }
}

/// Renders the closed `${PLACEHOLDER}` language in one pass. Replacement text
/// is appended as data and is never scanned again, so a value containing a
/// placeholder cannot introduce a second expansion.
pub fn render_template_v2(
    template: &TemplateArtifactV2,
    source: &str,
    values: &BTreeMap<String, String>,
) -> Result<Vec<u8>, ContractError> {
    template.validate("template")?;
    let declared: BTreeSet<&str> = template
        .placeholders
        .iter()
        .map(TemplatePlaceholderV2::as_str)
        .collect();
    for (name, value) in values {
        if !declared.contains(name.as_str()) {
            return Err(ContractError::InvalidValue {
                field: "template value",
                reason: "value supplied for an undeclared placeholder",
            });
        }
        if value.contains('\0') {
            return Err(ContractError::InvalidValue {
                field: "template value",
                reason: "value must not contain NUL",
            });
        }
    }

    let mut found = BTreeSet::new();
    let mut rendered = String::with_capacity(source.len());
    let mut cursor = 0;
    while cursor < source.len() {
        let Some(relative_start) = source[cursor..].find("${") else {
            rendered.push_str(&source[cursor..]);
            break;
        };
        let start = cursor + relative_start;
        rendered.push_str(&source[cursor..start]);
        let Some(relative_end) = source[start + 2..].find('}') else {
            return Err(ContractError::InvalidValue {
                field: "template",
                reason: "placeholder is unterminated",
            });
        };
        let end = start + 2 + relative_end;
        let name = &source[start + 2..end];
        let valid_name = !name.is_empty()
            && name.len() <= 64
            && name
                .bytes()
                .all(|byte| byte.is_ascii_uppercase() || byte.is_ascii_digit() || byte == b'_');
        if !valid_name || !declared.contains(name) {
            return Err(ContractError::InvalidValue {
                field: "template placeholder",
                reason: "placeholder is malformed or undeclared",
            });
        }
        let Some(value) = values.get(name) else {
            return Err(ContractError::InvalidValue {
                field: "template value",
                reason: "declared placeholder has no value",
            });
        };
        rendered.push_str(value);
        found.insert(name);
        cursor = end + 1;
    }
    if declared.iter().any(|name| !found.contains(name)) {
        return Err(ContractError::InvalidValue {
            field: "template placeholder",
            reason: "declared placeholder is absent from source",
        });
    }
    let bytes = rendered.into_bytes();
    if bytes.len() > template.rendered_byte_limit as usize {
        return Err(ContractError::ByteLimitExceeded {
            field: "rendered template",
            maximum: template.rendered_byte_limit as usize,
        });
    }
    Ok(bytes)
}

#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[repr(transparent)]
pub struct TemplatePlaceholderV2(String);

impl TemplatePlaceholderV2 {
    pub fn parse(value: impl Into<String>) -> Result<Self, ContractError> {
        let value = value.into();
        if value.is_empty()
            || value.len() > 64
            || !value
                .bytes()
                .all(|byte| byte.is_ascii_uppercase() || byte.is_ascii_digit() || byte == b'_')
        {
            return Err(ContractError::InvalidValue {
                field: "template placeholder",
                reason: "must use 1 through 64 upper-case ASCII letters, digits, or underscores",
            });
        }
        Ok(Self(value))
    }

    fn validate(&self) -> Result<(), ContractError> {
        Self::parse(self.0.clone()).map(|_| ())
    }

    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AssignmentRoleProfileV2 {
    pub assignment_role: AssignmentRole,
    pub system_template: TemplateArtifactV2,
    pub assignment_template: TemplateArtifactV2,
    pub policy: ActorPolicyArtifactV2,
    pub tools: Vec<ActorToolV2>,
    pub model: ModelProfileV2,
    pub limits: SessionLimitsV2,
}

impl AssignmentRoleProfileV2 {
    fn validate(&self) -> Result<(), ContractError> {
        self.system_template
            .validate("assignment-role system template")?;
        self.assignment_template
            .validate("assignment-role assignment template")?;
        self.policy.validate()?;
        self.model.validate()?;
        self.limits.validate()?;
        if self.tools.is_empty() {
            return Err(bundle_error(
                "assignment-role tools",
                "at least one tool is required",
            ));
        }
        let mut seen = BTreeSet::new();
        for tool in &self.tools {
            if !seen.insert(*tool) {
                return Err(bundle_error("assignment-role tools", "tool is duplicated"));
            }
            if !tool.is_allowed_for(self.assignment_role) {
                return Err(bundle_error(
                    "assignment-role tools",
                    "tool exceeds this assignment role's fixed authority",
                ));
            }
        }
        Ok(())
    }
}

/// Fixed common and terminal actor tools. New tools require a protocol
/// revision and a corresponding Rust capability binding.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum ActorToolV2 {
    WorkspaceRead,
    WorkspaceWrite,
    WorkspaceEdit,
    WorkspaceSearch,
    WorkspaceList,
    Shell,
    ForumSearch,
    ForumListTopics,
    ForumListThreads,
    ForumReadThread,
    PublicationCreate,
    ArtifactSeal,
    ArtifactRead,
    ProductSubmitTicket,
    CandidateCheckpointRegression,
    CandidateSubmit,
    QualityRunFullSuite,
    QualitySubmitReview,
    WorkComplete,
}

impl ActorToolV2 {
    fn is_allowed_for(self, assignment_role: AssignmentRole) -> bool {
        match self {
            Self::ProductSubmitTicket => assignment_role == AssignmentRole::ProductResearch,
            Self::CandidateCheckpointRegression | Self::CandidateSubmit => {
                assignment_role == AssignmentRole::Engineering
            }
            Self::QualityRunFullSuite | Self::QualitySubmitReview => {
                assignment_role == AssignmentRole::Quality
            }
            Self::WorkspaceRead
            | Self::WorkspaceWrite
            | Self::WorkspaceEdit
            | Self::WorkspaceSearch
            | Self::WorkspaceList
            | Self::Shell
            | Self::ForumSearch
            | Self::ForumListTopics
            | Self::ForumListThreads
            | Self::ForumReadThread
            | Self::PublicationCreate
            | Self::ArtifactSeal
            | Self::ArtifactRead
            | Self::WorkComplete => true,
        }
    }
}

/// A sealed Luau source artifact declaration.  The source is deliberately not
/// embedded here: application admission stores the bytes in CAS and this
/// contract carries only the digest and bounded, safe lookup path.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ActorPolicyArtifactV2 {
    pub source_path: ApplicationRelativePath,
    pub digest: ContentDigest,
    pub byte_limit: u32,
    pub entrypoint: PolicyEntrypointV2,
}

impl ActorPolicyArtifactV2 {
    fn validate(&self) -> Result<(), ContractError> {
        validate_application_source_path(&self.source_path, "actor policy", true)?;
        if !self.source_path.as_str().starts_with("policies/")
            || !self.source_path.as_str().ends_with(".luau")
        {
            return Err(bundle_error(
                "actor policy",
                "policy source must be a .luau file below policies/",
            ));
        }
        if self.byte_limit == 0 || self.byte_limit as usize > MAX_POLICY_ARTIFACT_BYTES {
            return Err(bundle_error(
                "actor policy",
                "byte limit must be positive and within the policy ceiling",
            ));
        }
        self.entrypoint.validate()
    }
}

/// The only entrypoint shape accepted by Factory policies.  Keeping this as a
/// closed enum prevents a source artifact from selecting an ambient module or
/// arbitrary function at actor runtime.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum PolicyEntrypointV2 {
    FactoryPolicy,
}

impl PolicyEntrypointV2 {
    pub const FACTORY_POLICY: &'static str = "factory_policy";

    pub fn parse(value: &str) -> Result<Self, ContractError> {
        if value == Self::FACTORY_POLICY {
            Ok(Self::FactoryPolicy)
        } else {
            Err(ContractError::InvalidValue {
                field: "policy entrypoint",
                reason: "only factory_policy is admitted",
            })
        }
    }

    #[must_use]
    pub const fn as_str(self) -> &'static str {
        Self::FACTORY_POLICY
    }

    fn validate(self) -> Result<(), ContractError> {
        let _ = self;
        Ok(())
    }
}

/// Verifies and copies source bytes into a sealed artifact.  This function is
/// intentionally pure; CAS adoption and persistence remain kernel concerns.
pub fn seal_application_artifact_v2(
    path: &ApplicationRelativePath,
    expected_digest: ContentDigest,
    byte_limit: usize,
    source: &[u8],
) -> Result<Vec<u8>, ContractError> {
    validate_application_source_path(path, "application source", false)?;
    if byte_limit == 0 || byte_limit > MAX_APPLICATION_ARTIFACT_BYTES {
        return Err(ContractError::InvalidValue {
            field: "application source byte limit",
            reason: "must be positive and within the application ceiling",
        });
    }
    if source.len() > byte_limit {
        return Err(ContractError::ByteLimitExceeded {
            field: "application source",
            maximum: byte_limit,
        });
    }
    if source.contains(&0) {
        return Err(ContractError::InvalidValue {
            field: "application source",
            reason: "must not contain NUL",
        });
    }
    if ContentDigest::of_bytes(source) != expected_digest {
        return Err(ContractError::InvalidValue {
            field: "application source digest",
            reason: "source bytes do not match declared BLAKE3 digest",
        });
    }
    Ok(source.to_vec())
}

/// Verifies a policy source against its closed artifact declaration.
pub fn seal_policy_artifact_v2(
    artifact: &ActorPolicyArtifactV2,
    source: &[u8],
) -> Result<Vec<u8>, ContractError> {
    artifact.validate()?;
    if source.len() > artifact.byte_limit as usize {
        return Err(ContractError::ByteLimitExceeded {
            field: "actor policy source",
            maximum: artifact.byte_limit as usize,
        });
    }
    let source = seal_application_artifact_v2(
        &artifact.source_path,
        artifact.digest,
        artifact.byte_limit as usize,
        source,
    )?;
    std::str::from_utf8(&source).map_err(|_| ContractError::InvalidValue {
        field: "actor policy source",
        reason: "Luau policy source must be valid UTF-8",
    })?;
    Ok(source)
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ModelProfileV2 {
    pub provider: String,
    pub model_id: String,
    pub thinking_level: ThinkingLevelV2,
    pub context_token_limit: u32,
    pub output_token_limit: u32,
    pub price_input_micro_usd_per_million_tokens: MicroUsd,
    pub price_output_micro_usd_per_million_tokens: MicroUsd,
    pub price_cache_read_micro_usd_per_million_tokens: MicroUsd,
    pub price_cache_write_micro_usd_per_million_tokens: MicroUsd,
    pub capability_flags: Vec<ModelCapabilityV2>,
}

impl ModelProfileV2 {
    fn validate(&self) -> Result<(), ContractError> {
        validate_nonempty_bounded("model provider", &self.provider, 160)?;
        validate_nonempty_bounded("model ID", &self.model_id, 240)?;
        if self.context_token_limit == 0 || self.output_token_limit == 0 {
            return Err(bundle_error(
                "model profile",
                "token limits must be positive",
            ));
        }
        let mut flags = BTreeSet::new();
        for flag in &self.capability_flags {
            if !flags.insert(*flag) {
                return Err(bundle_error(
                    "model profile",
                    "capability flags must be unique",
                ));
            }
        }
        Ok(())
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum ModelCapabilityV2 {
    Reasoning,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ThinkingLevelV2 {
    None,
    Low,
    Medium,
    High,
    XHigh,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SessionLimitsV2 {
    pub wall_limit: DurationMillis,
    pub output_byte_limit: u32,
}

impl SessionLimitsV2 {
    fn validate(&self) -> Result<(), ContractError> {
        if self.wall_limit.get() == 0
            || self.output_byte_limit == 0
            || u64::from(self.output_byte_limit) > MAX_SESSION_OUTPUT_BYTES
        {
            return Err(bundle_error(
                "session limits",
                "limits must be positive and output must fit one CAS object",
            ));
        }
        Ok(())
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TicketPolicyV2 {
    pub low_water: u16,
    pub target: u16,
    pub maximum: u16,
    pub proposal_maximum: u16,
    pub ticket_bounds: TicketBoundsV2,
}

impl TicketPolicyV2 {
    fn validate(&self) -> Result<(), ContractError> {
        if self.low_water == 0 || self.low_water > self.target || self.target > self.maximum {
            return Err(bundle_error(
                "ticket buffer",
                "expected 0 < low_water <= target <= maximum",
            ));
        }
        if self.proposal_maximum == 0 {
            return Err(bundle_error(
                "ticket buffer",
                "proposal maximum must be positive",
            ));
        }
        self.ticket_bounds.validate()
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TicketBoundsV2 {
    pub narrative_byte_limit: u32,
    pub acceptance_criteria_limit: u16,
    pub contract_read_limit: u16,
}

impl TicketBoundsV2 {
    fn validate(&self) -> Result<(), ContractError> {
        if self.narrative_byte_limit == 0
            || self.acceptance_criteria_limit == 0
            || self.contract_read_limit == 0
        {
            return Err(bundle_error("ticket bounds", "all bounds must be positive"));
        }
        Ok(())
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RequiredReadV2 {
    pub path: RepositoryRelativePath,
    pub reason: String,
}

fn validate_required_reads(reads: &[RequiredReadV2]) -> Result<(), ContractError> {
    if reads.is_empty() {
        return Err(bundle_error(
            "required reads",
            "at least one read is required",
        ));
    }
    let mut paths = BTreeSet::new();
    for read in reads {
        validate_nonempty_bounded("required read reason", &read.reason, 240)?;
        if !paths.insert(read.path.as_str()) {
            return Err(bundle_error("required reads", "path is duplicated"));
        }
    }
    Ok(())
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CommandProfileV2 {
    pub name: String,
    pub executable: ExecutableV2,
    pub argv: Vec<String>,
    pub working_directory: RepositoryRelativePath,
    pub environment: Vec<EnvironmentAdditionV2>,
    pub timeout: DurationMillis,
    pub stdout_byte_limit: u32,
    pub stderr_byte_limit: u32,
    pub expected_exit_status: i32,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ExecutableV2 {
    ApprovedTool(ApprovedToolV2),
    RepositoryPath(RepositoryRelativePath),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ApprovedToolV2 {
    Cargo,
    Git,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct EnvironmentAdditionV2 {
    pub name: String,
    pub value: String,
}

fn validate_commands(
    field: &'static str,
    commands: &[CommandProfileV2],
    nonempty: bool,
) -> Result<(), ContractError> {
    if nonempty && commands.is_empty() {
        return Err(bundle_error(field, "at least one command is required"));
    }
    let mut names = BTreeSet::new();
    for command in commands {
        validate_nonempty_bounded("command name", &command.name, 160)?;
        if !names.insert(command.name.as_str()) {
            return Err(bundle_error(field, "command name is duplicated"));
        }
        if command.timeout.get() == 0
            || command.stdout_byte_limit == 0
            || command.stderr_byte_limit == 0
        {
            return Err(bundle_error(field, "command limits must be positive"));
        }
        if command.argv.iter().any(|argument| argument.contains('\0')) {
            return Err(bundle_error(field, "argv contains NUL"));
        }
        let mut environment_names = BTreeSet::new();
        for addition in &command.environment {
            validate_environment_addition(addition)?;
            if !environment_names.insert(addition.name.as_str()) {
                return Err(bundle_error(field, "environment name is duplicated"));
            }
        }
    }
    Ok(())
}

fn validate_environment_addition(addition: &EnvironmentAdditionV2) -> Result<(), ContractError> {
    if addition.name.is_empty()
        || addition.name.len() > 160
        || !addition
            .name
            .bytes()
            .all(|byte| byte.is_ascii_uppercase() || byte.is_ascii_digit() || byte == b'_')
    {
        return Err(ContractError::InvalidValue {
            field: "environment name",
            reason: "must use up to 160 upper-case ASCII letters, digits, or underscores",
        });
    }
    if addition.value.contains('\0') {
        return Err(ContractError::InvalidValue {
            field: "environment value",
            reason: "must not contain NUL",
        });
    }
    Ok(())
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ValidationProfilesV2 {
    pub focused: Vec<CommandProfileV2>,
    pub full: Vec<CommandProfileV2>,
}

impl ValidationProfilesV2 {
    fn validate(&self) -> Result<(), ContractError> {
        validate_commands("focused validation", &self.focused, true)?;
        validate_commands("full validation", &self.full, true)
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct GitPolicyV2 {
    pub forbidden_paths: Vec<RepositoryRelativePath>,
    pub delivery_mode: DeliveryModeV2,
    pub provenance_trailers_required: bool,
}

impl GitPolicyV2 {
    fn validate(&self) -> Result<(), ContractError> {
        let mut paths = BTreeSet::new();
        for path in &self.forbidden_paths {
            if !paths.insert(path.as_str()) {
                return Err(bundle_error("Git policy", "forbidden path is duplicated"));
            }
        }
        if self.delivery_mode != DeliveryModeV2::LocalFastForwardOnly {
            return Err(bundle_error(
                "Git policy",
                "remote delivery is not admitted",
            ));
        }
        if !self.provenance_trailers_required {
            return Err(bundle_error(
                "Git policy",
                "provenance trailers are mandatory",
            ));
        }
        Ok(())
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CommitMessagePolicyV2 {
    pub subject_byte_limit: u16,
    pub body_byte_limit: u16,
}

impl CommitMessagePolicyV2 {
    fn validate(&self) -> Result<(), ContractError> {
        if self.subject_byte_limit == 0 || self.body_byte_limit == 0 {
            return Err(bundle_error(
                "commit message policy",
                "limits must be positive",
            ));
        }
        Ok(())
    }
}

/// A source file used by the pure Rust application compiler.  The compiler
/// consumes explicit bytes so source lookup and source identity are testable
/// and deterministic; it never follows a path implicitly.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ApplicationSourceFileV2 {
    pub path: ApplicationRelativePath,
    pub bytes: Vec<u8>,
}

impl ApplicationSourceFileV2 {
    pub fn new(
        path: ApplicationRelativePath,
        bytes: impl Into<Vec<u8>>,
    ) -> Result<Self, ContractError> {
        let bytes = bytes.into();
        validate_application_source_path(&path, "application source", false)?;
        if bytes.len() > MAX_APPLICATION_ARTIFACT_BYTES {
            return Err(ContractError::ByteLimitExceeded {
                field: "application source",
                maximum: MAX_APPLICATION_ARTIFACT_BYTES,
            });
        }
        if bytes.contains(&0) {
            return Err(ContractError::InvalidValue {
                field: "application source",
                reason: "must not contain NUL",
            });
        }
        Ok(Self { path, bytes })
    }
}

/// The materialized result of an application compiler invocation.  Files are
/// sorted by canonical path, and the identity is the digest of their
/// deterministic `path\0bytes` stream.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CompiledApplicationV2 {
    pub bundle: ApplicationBundleV2,
    pub files: Vec<ApplicationSourceFileV2>,
    pub source_digest: ContentDigest,
}

impl CompiledApplicationV2 {
    #[must_use]
    pub fn file(&self, path: &ApplicationRelativePath) -> Option<&[u8]> {
        self.files
            .binary_search_by_key(path, |file| file.path.clone())
            .ok()
            .map(|index| self.files[index].bytes.as_slice())
    }
}

/// Pure Rust application compiler. It validates the closed bundle, resolves
/// every declared template and policy against an explicit source map, checks
/// each digest/ceiling, and emits a deterministic source identity.
pub struct ApplicationCompilerV2;

impl ApplicationCompilerV2 {
    pub fn compile(
        bundle: ApplicationBundleV2,
        source_files: impl IntoIterator<Item = ApplicationSourceFileV2>,
    ) -> Result<CompiledApplicationV2, ContractError> {
        bundle.validate()?;
        let mut sources = BTreeMap::new();
        for source in source_files {
            if sources.insert(source.path.clone(), source).is_some() {
                return Err(bundle_error(
                    "application source",
                    "source path is duplicated",
                ));
            }
        }

        // A path may be shared by multiple roles, but every declaration must
        // describe the same artifact.  Otherwise selecting the first
        // declaration would make source identity depend on declaration order.
        validate_declared_artifact_identities(&bundle)?;

        let mut required = BTreeSet::new();
        required.insert(bundle.mission_template.source_path.clone());
        for profile in &bundle.assignment_role_profiles {
            required.insert(profile.system_template.source_path.clone());
            required.insert(profile.assignment_template.source_path.clone());
            required.insert(profile.policy.source_path.clone());
        }

        let mut files = Vec::with_capacity(required.len());
        for path in required {
            let Some(source) = sources.remove(&path) else {
                return Err(ContractError::InvalidValue {
                    field: "application source",
                    reason: "declared artifact has no source bytes",
                });
            };
            let expected_digest =
                declared_digest(&bundle, &path).ok_or(ContractError::InvalidValue {
                    field: "application source",
                    reason: "source path is not a declared artifact",
                })?;
            if let Some(policy) = bundle
                .assignment_role_profiles
                .iter()
                .map(|profile| &profile.policy)
                .find(|policy| policy.source_path == path)
            {
                seal_policy_artifact_v2(policy, &source.bytes)?;
            } else {
                seal_application_artifact_v2(
                    &source.path,
                    expected_digest,
                    MAX_APPLICATION_ARTIFACT_BYTES,
                    &source.bytes,
                )?;
                // Markdown templates are consumed as UTF-8 by the renderer;
                // reject invalid bytes during compilation rather than later
                // in an actor process.
                std::str::from_utf8(&source.bytes).map_err(|_| ContractError::InvalidValue {
                    field: "template source",
                    reason: "template source must be valid UTF-8",
                })?;
            }
            files.push(source);
        }

        if !sources.is_empty() {
            return Err(ContractError::InvalidValue {
                field: "application source",
                reason: "source bundle contains an undeclared file",
            });
        }
        let mut identity = Vec::new();
        for file in &files {
            identity.extend_from_slice(file.path.as_str().as_bytes());
            identity.push(0);
            identity.extend_from_slice(&file.bytes);
            identity.push(0);
        }
        Ok(CompiledApplicationV2 {
            bundle,
            files,
            source_digest: ContentDigest::of_bytes(&identity),
        })
    }
}

fn declared_digest(
    bundle: &ApplicationBundleV2,
    path: &ApplicationRelativePath,
) -> Option<ContentDigest> {
    if bundle.mission_template.source_path == *path {
        return Some(bundle.mission_template.digest);
    }
    for profile in &bundle.assignment_role_profiles {
        if profile.system_template.source_path == *path {
            return Some(profile.system_template.digest);
        }
        if profile.assignment_template.source_path == *path {
            return Some(profile.assignment_template.digest);
        }
        if profile.policy.source_path == *path {
            return Some(profile.policy.digest);
        }
    }
    None
}

fn validate_declared_artifact_identities(
    bundle: &ApplicationBundleV2,
) -> Result<(), ContractError> {
    let mut declarations: BTreeMap<ApplicationRelativePath, ContentDigest> = BTreeMap::new();
    let mut add =
        |path: &ApplicationRelativePath, digest: ContentDigest| match declarations.get(path) {
            Some(previous) if *previous != digest => Err(bundle_error(
                "application artifacts",
                "one source path has conflicting digests",
            )),
            _ => {
                declarations.insert(path.clone(), digest);
                Ok(())
            }
        };
    add(
        &bundle.mission_template.source_path,
        bundle.mission_template.digest,
    )?;
    for profile in &bundle.assignment_role_profiles {
        add(
            &profile.system_template.source_path,
            profile.system_template.digest,
        )?;
        add(
            &profile.assignment_template.source_path,
            profile.assignment_template.digest,
        )?;
        add(&profile.policy.source_path, profile.policy.digest)?;
    }
    Ok(())
}

fn validate_application_source_path(
    path: &ApplicationRelativePath,
    field: &'static str,
    policy: bool,
) -> Result<(), ContractError> {
    let value = path.as_str();
    if value.len() > MAX_APPLICATION_SOURCE_PATH_BYTES {
        return Err(ContractError::ByteLimitExceeded {
            field,
            maximum: MAX_APPLICATION_SOURCE_PATH_BYTES,
        });
    }
    if value.starts_with("/") || value.contains('\\') || value.contains('\0') {
        return Err(ContractError::UnsafeRelativePath {
            field,
            reason: "source path is not canonical",
        });
    }
    if policy && (!value.starts_with("policies/") || !value.ends_with(".luau")) {
        return Err(ContractError::UnsafeRelativePath {
            field,
            reason: "policy path must be a .luau file beneath policies/",
        });
    }
    Ok(())
}

fn validate_nonempty_bounded(
    field: &'static str,
    value: &str,
    maximum: usize,
) -> Result<(), ContractError> {
    if value.is_empty() {
        return Err(ContractError::InvalidValue {
            field,
            reason: "must not be empty",
        });
    }
    if value.len() > maximum {
        return Err(ContractError::ByteLimitExceeded { field, maximum });
    }
    if value.contains('\0') {
        return Err(ContractError::InvalidValue {
            field,
            reason: "must not contain NUL",
        });
    }
    Ok(())
}

fn bundle_error(invariant: &'static str, evidence: &'static str) -> ContractError {
    ContractError::BundleInvariant {
        invariant,
        evidence,
    }
}
