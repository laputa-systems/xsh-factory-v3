use std::collections::{BTreeMap, BTreeSet};

use crate::{
    AbsoluteHostPath, ApplicationRelativePath, AssignmentRole, ContentDigest, ContractError,
    DurationMillis, MicroUsd, RepositoryRelativePath,
};

/// The only application-bundle format admitted by the first implementation.
pub const APPLICATION_BUNDLE_V1_FORMAT: u16 = 1;

/// Canonical, closed application policy understood by the generic kernel.
///
/// There is intentionally no metadata field, callback, predicate, or dynamic
/// tool definition. A new authority concept requires an explicit protocol
/// revision rather than an application-provided escape hatch.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ApplicationBundleV1 {
    pub format_version: u16,
    pub application_key: ApplicationKey,
    pub predecessor_bundle: Option<ContentDigest>,
    pub repository: RepositoryBindingV1,
    pub mission_template: TemplateArtifactV1,
    pub assignment_role_profiles: Vec<AssignmentRoleProfileV1>,
    pub ticket_policy: TicketPolicyV1,
    pub required_reads: Vec<RequiredReadV1>,
    pub reproducer_profiles: Vec<CommandProfileV1>,
    pub validation_profiles: ValidationProfilesV1,
    pub git_policy: GitPolicyV1,
    pub commit_message_policy: CommitMessagePolicyV1,
}

impl ApplicationBundleV1 {
    pub fn validate(&self) -> Result<(), ContractError> {
        if self.format_version != APPLICATION_BUNDLE_V1_FORMAT {
            return Err(bundle_error(
                "bundle format",
                "format_version is not the V1 format",
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
pub struct RepositoryBindingV1 {
    pub repository_key: String,
    pub canonical_local_path: AbsoluteHostPath,
    pub default_branch: String,
    pub delivery_mode: DeliveryModeV1,
}

impl RepositoryBindingV1 {
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
        if self.delivery_mode != DeliveryModeV1::LocalFastForwardOnly {
            return Err(bundle_error(
                "repository binding",
                "only guarded local fast-forward delivery is admitted",
            ));
        }
        Ok(())
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DeliveryModeV1 {
    LocalFastForwardOnly,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TemplateArtifactV1 {
    pub source_path: ApplicationRelativePath,
    pub digest: ContentDigest,
    pub placeholders: Vec<TemplatePlaceholderV1>,
    pub rendered_byte_limit: u32,
}

impl TemplateArtifactV1 {
    fn validate(&self, field: &'static str) -> Result<(), ContractError> {
        if self.rendered_byte_limit == 0 {
            return Err(bundle_error(field, "rendered byte limit must be positive"));
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

/// Renders the closed `${PLACEHOLDER}` language in one pass.  Replacement
/// text is appended as data and is never scanned again, so a value containing
/// `${OTHER}` cannot introduce a second expansion.  The same declaration and
/// byte-limit rules used by application admission apply here as well.
pub fn render_template_v1(
    template: &TemplateArtifactV1,
    source: &str,
    values: &BTreeMap<String, String>,
) -> Result<Vec<u8>, ContractError> {
    template.validate("template")?;
    let declared: BTreeSet<&str> = template
        .placeholders
        .iter()
        .map(TemplatePlaceholderV1::as_str)
        .collect();
    for name in values.keys() {
        if !declared.contains(name.as_str()) {
            return Err(ContractError::InvalidValue {
                field: "template value",
                reason: "value supplied for an undeclared placeholder",
            });
        }
        if values[name].contains('\0') {
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
pub struct TemplatePlaceholderV1(String);

impl TemplatePlaceholderV1 {
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
pub struct AssignmentRoleProfileV1 {
    pub assignment_role: AssignmentRole,
    pub system_template: TemplateArtifactV1,
    pub assignment_template: TemplateArtifactV1,
    pub tools: Vec<ActorToolV1>,
    pub model: ModelProfileV1,
    pub limits: SessionLimitsV1,
}

impl AssignmentRoleProfileV1 {
    fn validate(&self) -> Result<(), ContractError> {
        self.system_template
            .validate("assignment-role system template")?;
        self.assignment_template
            .validate("assignment-role assignment template")?;
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

/// Fixed common and terminal actor tools. New tools require a protocol change.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum ActorToolV1 {
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
    ForumCreateTopic,
    ForumCreateThread,
    ForumPost,
    ArtifactSeal,
    /// Reads only a daemon-derived sealed evidence closure for this exact
    /// assignment target; the actor cannot name arbitrary CAS objects.
    ArtifactRead,
    ProductSubmitTicket,
    CandidateCheckpointRegression,
    CandidateSubmit,
    QualityRunFullSuite,
    QualitySubmitReview,
    WorkComplete,
}

impl ActorToolV1 {
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
            | Self::ForumCreateTopic
            | Self::ForumCreateThread
            | Self::ForumPost
            | Self::ArtifactSeal
            | Self::ArtifactRead
            | Self::WorkComplete => true,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ModelProfileV1 {
    pub provider: String,
    pub model_id: String,
    pub thinking_level: ThinkingLevelV1,
    pub context_token_limit: u32,
    pub output_token_limit: u32,
    pub price_input_micro_usd_per_million_tokens: MicroUsd,
    pub price_output_micro_usd_per_million_tokens: MicroUsd,
    pub price_cache_read_micro_usd_per_million_tokens: MicroUsd,
    pub price_cache_write_micro_usd_per_million_tokens: MicroUsd,
    pub capability_flags: Vec<ModelCapabilityV1>,
}

impl ModelProfileV1 {
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

/// Closed model capabilities which affect host construction and are therefore
/// part of the immutable application/assignment identity.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum ModelCapabilityV1 {
    Reasoning,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ThinkingLevelV1 {
    None,
    Low,
    Medium,
    High,
    /// Pi's exact spelling for the reasoning level above `high`. This is not
    /// normalized because the selected model descriptor is pinned verbatim.
    XHigh,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SessionLimitsV1 {
    pub turn_limit: u32,
    pub wall_limit: DurationMillis,
    pub output_byte_limit: u32,
}

impl SessionLimitsV1 {
    fn validate(&self) -> Result<(), ContractError> {
        if self.turn_limit == 0 || self.wall_limit.get() == 0 || self.output_byte_limit == 0 {
            return Err(bundle_error(
                "session limits",
                "all limits must be positive",
            ));
        }
        Ok(())
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TicketPolicyV1 {
    pub low_water: u16,
    pub target: u16,
    pub maximum: u16,
    pub proposal_maximum: u16,
    pub ticket_bounds: TicketBoundsV1,
}

impl TicketPolicyV1 {
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
pub struct TicketBoundsV1 {
    pub narrative_byte_limit: u32,
    pub acceptance_criteria_limit: u16,
    pub contract_read_limit: u16,
}

impl TicketBoundsV1 {
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
pub struct RequiredReadV1 {
    pub path: RepositoryRelativePath,
    pub reason: String,
}

fn validate_required_reads(reads: &[RequiredReadV1]) -> Result<(), ContractError> {
    if reads.is_empty() {
        return Err(bundle_error(
            "required reads",
            "at least one read is required",
        ));
    }
    let mut paths = BTreeSet::new();
    for read in reads {
        // Required-read reasons are copied verbatim into the immutable
        // assignment packet. Keep one packet-compatible bound at admission
        // rather than accepting a bundle which cannot be materialized.
        validate_nonempty_bounded("required read reason", &read.reason, 240)?;
        if !paths.insert(read.path.as_str()) {
            return Err(bundle_error("required reads", "path is duplicated"));
        }
    }
    Ok(())
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CommandProfileV1 {
    pub name: String,
    pub executable: ExecutableV1,
    pub argv: Vec<String>,
    pub working_directory: RepositoryRelativePath,
    pub environment: Vec<EnvironmentAdditionV1>,
    pub timeout: DurationMillis,
    pub stdout_byte_limit: u32,
    pub stderr_byte_limit: u32,
    pub expected_exit_status: i32,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ExecutableV1 {
    ApprovedTool(ApprovedToolV1),
    RepositoryPath(RepositoryRelativePath),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ApprovedToolV1 {
    Cargo,
    Git,
    Deno,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct EnvironmentAdditionV1 {
    pub name: String,
    pub value: String,
}

fn validate_commands(
    field: &'static str,
    commands: &[CommandProfileV1],
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

fn validate_environment_addition(addition: &EnvironmentAdditionV1) -> Result<(), ContractError> {
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
pub struct ValidationProfilesV1 {
    pub focused: Vec<CommandProfileV1>,
    pub full: Vec<CommandProfileV1>,
}

impl ValidationProfilesV1 {
    fn validate(&self) -> Result<(), ContractError> {
        validate_commands("focused validation", &self.focused, true)?;
        validate_commands("full validation", &self.full, true)
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct GitPolicyV1 {
    pub forbidden_paths: Vec<RepositoryRelativePath>,
    pub delivery_mode: DeliveryModeV1,
    pub provenance_trailers_required: bool,
}

impl GitPolicyV1 {
    fn validate(&self) -> Result<(), ContractError> {
        let mut paths = BTreeSet::new();
        for path in &self.forbidden_paths {
            if !paths.insert(path.as_str()) {
                return Err(bundle_error("Git policy", "forbidden path is duplicated"));
            }
        }
        if self.delivery_mode != DeliveryModeV1::LocalFastForwardOnly {
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
pub struct CommitMessagePolicyV1 {
    pub subject_byte_limit: u16,
    pub body_byte_limit: u16,
}

impl CommitMessagePolicyV1 {
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
