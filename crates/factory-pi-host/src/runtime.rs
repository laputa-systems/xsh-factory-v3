//! Binary-only inherited-descriptor and rooted-workspace adapters.

use factory_pi_host::{
    DaemonError, DaemonFuture, FrameClient, FramedDaemon, LocalToolExecutor,
    MAX_REQUEST_FRAME_BYTES, ToolName,
};
use factory_protocol::{PROTOCOL_VERSION_V2, RepositoryRelativePath, encode_frame};
use pi_agent_core::scheduler::CancellationToken;
use pi_agent_protocol::{JsonNumber, JsonValue};
use std::{
    collections::BTreeMap,
    fs::{self, File, OpenOptions},
    io::{self, Read},
    os::unix::process::CommandExt as _,
    path::{Path, PathBuf},
    process::{Command, Stdio},
    sync::Mutex,
    thread,
    time::{Duration, Instant},
};

pub(crate) struct InheritedDaemon {
    client: Mutex<FrameClient<File, File>>,
}

impl InheritedDaemon {
    pub(crate) fn from_fd0() -> Result<Self, DaemonError> {
        let writer = OpenOptions::new()
            .read(true)
            .write(true)
            .open("/dev/fd/0")
            .map_err(io_error)?;
        let reader = writer.try_clone().map_err(io_error)?;
        Ok(Self {
            client: Mutex::new(FrameClient::new(reader, writer)),
        })
    }

    fn exchange(
        &self,
        operation: &'static str,
        payload: JsonValue,
    ) -> Result<JsonValue, DaemonError> {
        let mut client = self
            .client
            .lock()
            .map_err(|_| DaemonError::new("actor transport mutex is poisoned"))?;
        let request_id = client.next_request_id();
        let request = request_json(operation, request_id.as_str(), payload)?;
        let frame = encode_frame(request.as_bytes(), MAX_REQUEST_FRAME_BYTES)
            .map_err(|error| DaemonError::new(error.to_string()))?;
        let response = client
            .exchange(&frame)
            .map_err(|error| DaemonError::new(error.to_string()))?;
        let text = std::str::from_utf8(&response)
            .map_err(|_| DaemonError::new("daemon response is not UTF-8"))?;
        let value = JsonValue::parse(text)
            .map_err(|_| DaemonError::new("daemon response is not valid JSON"))?;
        let object = value
            .as_object()
            .ok_or_else(|| DaemonError::new("daemon response is not an object"))?;
        FrameClient::<File, File>::validate_response_identity(
            object.get("operation").and_then(JsonValue::as_str),
            object.get("request_id").and_then(JsonValue::as_str),
            operation,
            request_id.as_str(),
        )
        .map_err(|error| DaemonError::new(error.to_string()))?;
        Ok(value)
    }
}

impl FramedDaemon for InheritedDaemon {
    fn call<'a>(&'a self, operation: &'static str, payload: JsonValue) -> DaemonFuture<'a> {
        Box::pin(async move { self.exchange(operation, payload) })
    }
}

fn request_json(
    operation: &'static str,
    request_id: &str,
    payload: JsonValue,
) -> Result<String, DaemonError> {
    let mut object = payload
        .as_object()
        .cloned()
        .ok_or_else(|| DaemonError::new("daemon request payload must be an object"))?;
    for field in ["protocol_version", "request_id", "operation"] {
        if object.contains_key(field) {
            return Err(DaemonError::new(format!("host owns request field {field}")));
        }
    }
    object.insert(
        "protocol_version".to_owned(),
        number(u64::from(PROTOCOL_VERSION_V2))?,
    );
    object.insert(
        "request_id".to_owned(),
        JsonValue::String(request_id.to_owned()),
    );
    object.insert(
        "operation".to_owned(),
        JsonValue::String(operation.to_owned()),
    );
    let mut field_names = vec!["protocol_version", "request_id", "operation"];
    field_names
        .extend_from_slice(request_field_names(operation).ok_or_else(|| {
            DaemonError::new(format!("unsupported daemon operation {operation}"))
        })?);
    canonical_object(&object, &field_names, None, operation)
}

#[derive(Clone, Copy)]
enum ObjectKind {
    SealedArtifactReference,
    ContractRead,
    DuplicateSearch,
    Observation,
    Reproducer,
    InstitutionalReference,
    PublicationAttachment,
}

fn request_field_names(operation: &str) -> Option<&'static [&'static str]> {
    Some(match operation {
        "workspace.read" => &["repository_relative_path"],
        "forum.list_topics" => &["cursor", "limit"],
        "forum.list_threads" => &["topic_id", "cursor", "limit"],
        "forum.search" => &[
            "query",
            "topic_id",
            "thread_id",
            "author_office",
            "post_kind",
            "created_after_micros",
            "created_before_micros",
            "cursor",
            "limit",
        ],
        "forum.read_thread" => &["thread_id", "after_post_id", "limit"],
        "artifact.seal_workspace_file" => &[
            "client_command_id",
            "expected_revision",
            "workspace_relative_path",
            "byte_limit",
        ],
        "artifact.read" => &["artifact_id", "expected_digest"],
        "product.submit_ticket" => &[
            "client_command_id",
            "expected_revision",
            "title",
            "mission_value",
            "scope",
            "contract_owner",
            "risk",
            "narrative",
            "evidence",
            "acceptance_criteria",
            "contract_reads",
            "duplicate_search",
            "reproducer_profile",
            "reproducer",
        ],
        "candidate.checkpoint_regression" => &[
            "client_command_id",
            "expected_revision",
            "regression_command",
            "expected_failure",
        ],
        "candidate.submit" => &[
            "client_command_id",
            "expected_revision",
            "commit_subject",
            "commit_body",
            "regression_test_identity",
        ],
        "quality.run_full_suite" => &[
            "client_command_id",
            "expected_revision",
            "validation_profile",
        ],
        "quality.submit_review" => &[
            "client_command_id",
            "expected_revision",
            "full_suite_validation_id",
            "verdict",
            "rationale",
            "risks",
            "additional_probes",
        ],
        "publication.create" => &[
            "client_command_id",
            "anchor",
            "kind",
            "summary",
            "body_artifact_id",
            "attachments",
            "reply_to",
            "supersedes",
        ],
        "session.verify_packet" => &["packet_digest", "packet_bytes_b64"],
        "session.seal_artifact" => &[
            "client_command_id",
            "expected_revision",
            "staging_relative_path",
            "role",
            "byte_limit",
        ],
        "session.submit_terminal" => &[
            "client_command_id",
            "expected_revision",
            "terminal_operation",
            "terminal_payload_b64",
            "transcript_artifact_id",
            "input_tokens",
            "output_tokens",
            "cache_read_tokens",
            "cache_write_tokens",
            "reasoning_tokens",
            "reported_cost_micro_usd",
            "stop_reason",
        ],
        _ => return None,
    })
}

fn object_fields(kind: ObjectKind) -> &'static [&'static str] {
    match kind {
        ObjectKind::SealedArtifactReference => &["artifact_id", "digest", "byte_length"],
        ObjectKind::ContractRead => &["path", "reason"],
        ObjectKind::DuplicateSearch => &["query", "limit"],
        ObjectKind::Observation => &["exit_status", "stdout", "stderr"],
        ObjectKind::Reproducer => &[
            "comparison_rule_version",
            "command",
            "stdin",
            "expected_observation",
            "first_observation",
            "second_observation",
        ],
        ObjectKind::InstitutionalReference => &["kind", "id"],
        ObjectKind::PublicationAttachment => &["artifact_id", "label"],
    }
}

fn request_object_kind(operation: &str, field: &str) -> Option<ObjectKind> {
    match (operation, field) {
        ("product.submit_ticket", "narrative" | "evidence") => {
            Some(ObjectKind::SealedArtifactReference)
        }
        ("quality.submit_review", "rationale" | "risks" | "additional_probes") => {
            Some(ObjectKind::SealedArtifactReference)
        }
        ("product.submit_ticket", "duplicate_search") => Some(ObjectKind::DuplicateSearch),
        ("product.submit_ticket", "reproducer") => Some(ObjectKind::Reproducer),
        ("publication.create", "anchor") => Some(ObjectKind::InstitutionalReference),
        _ => None,
    }
}

fn request_array_object_kind(operation: &str, field: &str) -> Option<ObjectKind> {
    match (operation, field) {
        ("product.submit_ticket", "contract_reads") => Some(ObjectKind::ContractRead),
        ("publication.create", "attachments") => Some(ObjectKind::PublicationAttachment),
        _ => None,
    }
}

fn object_field_kind(kind: ObjectKind, field: &str) -> Option<ObjectKind> {
    match (kind, field) {
        (ObjectKind::Reproducer, "command" | "stdin") => Some(ObjectKind::SealedArtifactReference),
        (
            ObjectKind::Reproducer,
            "expected_observation" | "first_observation" | "second_observation",
        ) => Some(ObjectKind::Observation),
        (ObjectKind::Observation, "stdout" | "stderr") => Some(ObjectKind::SealedArtifactReference),
        _ => None,
    }
}

fn canonical_object(
    object: &BTreeMap<String, JsonValue>,
    field_names: &[&str],
    object_kind: Option<ObjectKind>,
    operation: &str,
) -> Result<String, DaemonError> {
    if object.len() != field_names.len()
        || object
            .keys()
            .any(|field| !field_names.contains(&field.as_str()))
    {
        return Err(DaemonError::new(format!(
            "request payload fields do not match {operation}"
        )));
    }
    let mut output = String::from("{");
    for (index, field) in field_names.iter().enumerate() {
        if index != 0 {
            output.push(',');
        }
        let key = JsonValue::String((*field).to_owned())
            .to_json_string()
            .map_err(|error| DaemonError::new(error.to_string()))?;
        let value = object
            .get(*field)
            .ok_or_else(|| DaemonError::new(format!("request payload is missing {field}")))?;
        let nested_object_kind = object_kind
            .and_then(|kind| object_field_kind(kind, field))
            .or_else(|| request_object_kind(operation, field));
        let value = canonical_value(
            value,
            nested_object_kind,
            request_array_object_kind(operation, field),
            operation,
        )?;
        output.push_str(&key);
        output.push(':');
        output.push_str(&value);
    }
    output.push('}');
    Ok(output)
}

fn canonical_value(
    value: &JsonValue,
    object_kind: Option<ObjectKind>,
    array_object_kind: Option<ObjectKind>,
    operation: &str,
) -> Result<String, DaemonError> {
    match value {
        JsonValue::Object(object) => {
            let kind = object_kind.ok_or_else(|| {
                DaemonError::new(format!("unexpected nested object in {operation}"))
            })?;
            canonical_object(object, object_fields(kind), Some(kind), operation)
        }
        JsonValue::Array(values) => {
            let mut output = String::from("[");
            for (index, value) in values.iter().enumerate() {
                if index != 0 {
                    output.push(',');
                }
                output.push_str(&canonical_value(value, array_object_kind, None, operation)?);
            }
            output.push(']');
            Ok(output)
        }
        value => value
            .to_json_string()
            .map_err(|error| DaemonError::new(error.to_string())),
    }
}

pub(crate) struct RootedWorkspace {
    root: PathBuf,
}

impl RootedWorkspace {
    pub(crate) fn new(root: impl AsRef<Path>) -> Result<Self, DaemonError> {
        let root = fs::canonicalize(root).map_err(io_error)?;
        if !root.is_dir() {
            return Err(DaemonError::new("workspace root is not a directory"));
        }
        Ok(Self { root })
    }

    fn invoke_sync(
        &self,
        tool: ToolName,
        arguments: JsonValue,
        cancellation: CancellationToken,
    ) -> Result<JsonValue, DaemonError> {
        if cancellation.is_cancelled() {
            return Err(DaemonError::new("tool was cancelled"));
        }
        match tool {
            ToolName::WorkspaceWrite => self.write(arguments),
            ToolName::WorkspaceEdit => self.edit(arguments),
            ToolName::WorkspaceSearch => self.search(arguments),
            ToolName::WorkspaceList => self.list(arguments),
            ToolName::Shell => self.shell(arguments, cancellation),
            _ => Err(DaemonError::new("tool is not workspace-local")),
        }
    }

    fn existing(&self, text: &str) -> Result<PathBuf, DaemonError> {
        let relative = RepositoryRelativePath::parse(text.to_owned())
            .map_err(|_| DaemonError::new("unsafe workspace path"))?;
        let target = fs::canonicalize(self.root.join(relative.as_str())).map_err(io_error)?;
        if !target.starts_with(&self.root) || target.starts_with(self.root.join(".git")) {
            return Err(DaemonError::new("workspace path escapes root"));
        }
        Ok(target)
    }

    fn destination(&self, text: &str) -> Result<PathBuf, DaemonError> {
        let relative = RepositoryRelativePath::parse(text.to_owned())
            .map_err(|_| DaemonError::new("unsafe workspace path"))?;
        let candidate = self.root.join(relative.as_str());
        let parent = candidate
            .parent()
            .ok_or_else(|| DaemonError::new("workspace path has no parent"))?;
        let parent = fs::canonicalize(parent).map_err(io_error)?;
        if !parent.starts_with(&self.root) || parent.starts_with(self.root.join(".git")) {
            return Err(DaemonError::new("workspace path escapes root"));
        }
        Ok(candidate)
    }

    fn write(&self, value: JsonValue) -> Result<JsonValue, DaemonError> {
        let map = map(&value)?;
        let path = required(map, "repository_relative_path")?;
        let contents = required(map, "contents")?;
        fs::write(self.destination(path)?, contents.as_bytes()).map_err(io_error)?;
        Ok(JsonValue::object([
            (
                "repository_relative_path",
                JsonValue::String(path.to_owned()),
            ),
            ("byte_length", number(contents.len() as u64)?),
        ]))
    }

    fn edit(&self, value: JsonValue) -> Result<JsonValue, DaemonError> {
        let map = map(&value)?;
        let path = required(map, "repository_relative_path")?;
        let old = required(map, "old_text")?;
        let new = required(map, "new_text")?;
        let target = self.existing(path)?;
        let source = fs::read_to_string(&target).map_err(io_error)?;
        match source.matches(old).count() {
            1 => {}
            0 => return Err(DaemonError::new("edit old_text was not found")),
            _ => return Err(DaemonError::new("edit old_text occurs more than once")),
        }
        fs::write(target, source.replacen(old, new, 1)).map_err(io_error)?;
        Ok(JsonValue::object([("replaced", JsonValue::Bool(true))]))
    }

    fn search(&self, value: JsonValue) -> Result<JsonValue, DaemonError> {
        let map = map(&value)?;
        let query = required(map, "query")?;
        let limit = map
            .get("limit")
            .and_then(JsonValue::as_u64)
            .unwrap_or(50)
            .min(200) as usize;
        let start = match map
            .get("repository_relative_path")
            .and_then(JsonValue::as_str)
        {
            Some(path) if !path.is_empty() => self.existing(path)?,
            _ => self.root.clone(),
        };
        let mut matches = Vec::new();
        search_tree(&self.root, &start, query, limit, &mut matches)?;
        Ok(JsonValue::object([("matches", JsonValue::Array(matches))]))
    }

    fn list(&self, value: JsonValue) -> Result<JsonValue, DaemonError> {
        let map = map(&value)?;
        let directory = match map
            .get("repository_relative_path")
            .and_then(JsonValue::as_str)
        {
            Some(path) if !path.is_empty() => self.existing(path)?,
            _ => self.root.clone(),
        };
        if !directory.is_dir() {
            return Err(DaemonError::new("list target is not a directory"));
        }
        let mut entries = fs::read_dir(directory)
            .map_err(io_error)?
            .filter_map(Result::ok)
            .filter_map(|entry| {
                let path = entry.path();
                Some((
                    path.strip_prefix(&self.root).ok()?.to_str()?.to_owned(),
                    entry.file_type().ok()?.is_dir(),
                ))
            })
            .filter(|(path, _)| !path.starts_with(".git/"))
            .collect::<Vec<_>>();
        entries.sort();
        entries.truncate(200);
        Ok(JsonValue::object([(
            "entries",
            JsonValue::Array(
                entries
                    .into_iter()
                    .map(|(path, directory)| {
                        JsonValue::object([
                            ("repository_relative_path", JsonValue::String(path)),
                            ("is_directory", JsonValue::Bool(directory)),
                        ])
                    })
                    .collect(),
            ),
        )]))
    }

    fn shell(
        &self,
        value: JsonValue,
        cancellation: CancellationToken,
    ) -> Result<JsonValue, DaemonError> {
        let map = map(&value)?;
        let command = required(map, "command")?;
        let timeout = Duration::from_millis(
            map.get("timeout_millis")
                .and_then(JsonValue::as_u64)
                .unwrap_or(30_000)
                .min(300_000),
        );
        let output = shell(&self.root, command, timeout, cancellation)?;
        Ok(JsonValue::object([
            ("exit_status", number(output.status as u64)?),
            ("stdout", JsonValue::String(output.stdout)),
            ("stderr", JsonValue::String(output.stderr)),
            ("timed_out", JsonValue::Bool(output.timed_out)),
            ("output_truncated", JsonValue::Bool(output.truncated)),
        ]))
    }
}

impl LocalToolExecutor for RootedWorkspace {
    fn invoke<'a>(
        &'a self,
        tool: ToolName,
        arguments: JsonValue,
        cancellation: CancellationToken,
    ) -> DaemonFuture<'a> {
        Box::pin(async move { self.invoke_sync(tool, arguments, cancellation) })
    }
}

fn map(value: &JsonValue) -> Result<&BTreeMap<String, JsonValue>, DaemonError> {
    value
        .as_object()
        .ok_or_else(|| DaemonError::new("tool arguments must be an object"))
}

fn required<'a>(map: &'a BTreeMap<String, JsonValue>, field: &str) -> Result<&'a str, DaemonError> {
    map.get(field)
        .and_then(JsonValue::as_str)
        .filter(|text| !text.is_empty())
        .ok_or_else(|| DaemonError::new(format!("missing string tool argument {field}")))
}

fn number(value: u64) -> Result<JsonValue, DaemonError> {
    JsonValue::number(JsonNumber::Unsigned(value))
        .map_err(|error| DaemonError::new(error.to_string()))
}

fn search_tree(
    root: &Path,
    entry: &Path,
    query: &str,
    limit: usize,
    matches: &mut Vec<JsonValue>,
) -> Result<(), DaemonError> {
    if matches.len() == limit || entry.starts_with(root.join(".git")) {
        return Ok(());
    }
    let metadata = fs::symlink_metadata(entry).map_err(io_error)?;
    if metadata.file_type().is_symlink() {
        return Ok(());
    }
    if metadata.is_dir() {
        let mut entries = fs::read_dir(entry)
            .map_err(io_error)?
            .collect::<Result<Vec<_>, _>>()
            .map_err(io_error)?;
        entries.sort_by_key(|entry| entry.file_name());
        for child in entries {
            search_tree(root, &child.path(), query, limit, matches)?;
            if matches.len() == limit {
                break;
            }
        }
    } else if metadata.is_file() && metadata.len() <= 2 * 1024 * 1024 {
        let bytes = fs::read(entry).map_err(io_error)?;
        if let Ok(text) = std::str::from_utf8(&bytes) {
            let path = entry
                .strip_prefix(root)
                .map_err(|_| DaemonError::new("search escaped workspace root"))?
                .to_string_lossy()
                .into_owned();
            for (line, text) in text.lines().enumerate() {
                if text.contains(query) {
                    matches.push(JsonValue::object([
                        ("repository_relative_path", JsonValue::String(path.clone())),
                        ("line", number((line + 1) as u64)?),
                        ("text", JsonValue::String(bound(text, 4096))),
                    ]));
                    if matches.len() == limit {
                        break;
                    }
                }
            }
        }
    }
    Ok(())
}

struct ShellOutput {
    status: i32,
    stdout: String,
    stderr: String,
    timed_out: bool,
    truncated: bool,
}

fn shell(
    root: &Path,
    command: &str,
    timeout: Duration,
    cancellation: CancellationToken,
) -> Result<ShellOutput, DaemonError> {
    let kernel_path = std::env::var_os("PATH").unwrap_or_else(|| "/usr/bin:/bin".into());
    let mut child = Command::new("/bin/sh")
        .arg("-lc")
        .arg(command)
        .current_dir(root)
        .env_clear()
        .env("NO_COLOR", "1")
        .env("PATH", kernel_path)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .process_group(0)
        .spawn()
        .map_err(io_error)?;
    let process_group = child.id();
    let stdout = child
        .stdout
        .take()
        .ok_or_else(|| DaemonError::new("missing shell stdout"))?;
    let stderr = child
        .stderr
        .take()
        .ok_or_else(|| DaemonError::new("missing shell stderr"))?;
    let stdout = thread::spawn(move || drain(stdout, 128 * 1024));
    let stderr = thread::spawn(move || drain(stderr, 128 * 1024));
    let deadline = Instant::now() + timeout;
    let mut timed_out = false;
    loop {
        if child.try_wait().map_err(io_error)?.is_some() {
            terminate_shell_process_group(process_group);
            break;
        }
        if cancellation.is_cancelled() {
            terminate_shell_process_group(process_group);
            return Err(DaemonError::new("shell invocation was cancelled"));
        }
        if Instant::now() >= deadline {
            timed_out = true;
            terminate_shell_process_group(process_group);
            break;
        }
        thread::sleep(Duration::from_millis(10));
    }
    let status = child.wait().map_err(io_error)?;
    let stdout = stdout
        .join()
        .map_err(|_| DaemonError::new("shell stdout reader panicked"))?;
    let stderr = stderr
        .join()
        .map_err(|_| DaemonError::new("shell stderr reader panicked"))?;
    Ok(ShellOutput {
        status: status.code().unwrap_or(128),
        stdout: String::from_utf8_lossy(&stdout.0).into_owned(),
        stderr: String::from_utf8_lossy(&stderr.0).into_owned(),
        timed_out,
        truncated: stdout.1 || stderr.1,
    })
}

fn terminate_shell_process_group(process_group: u32) {
    let group = format!("-{process_group}");
    let _ = Command::new("/bin/kill")
        .args(["-TERM", "--", group.as_str()])
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status();
    let _ = Command::new("/bin/kill")
        .args(["-KILL", "--", group.as_str()])
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status();
}

fn drain(mut input: impl Read, limit: usize) -> (Vec<u8>, bool) {
    let mut output = Vec::with_capacity(limit.min(4096));
    let mut buffer = [0_u8; 8192];
    let mut truncated = false;
    loop {
        match input.read(&mut buffer) {
            Ok(0) | Err(_) => break,
            Ok(length) => {
                let take = length.min(limit.saturating_sub(output.len()));
                output.extend_from_slice(&buffer[..take]);
                truncated |= take != length;
            }
        }
    }
    (output, truncated)
}

fn bound(text: &str, limit: usize) -> String {
    if text.len() <= limit {
        return text.to_owned();
    }
    let mut end = limit;
    while !text.is_char_boundary(end) {
        end -= 1;
    }
    format!("{}…", &text[..end])
}

fn io_error(error: io::Error) -> DaemonError {
    DaemonError::new(error.to_string())
}

#[cfg(test)]
mod tests {
    use super::{request_json, shell};
    use factory_protocol::{
        CandidateSubmitRequest, InstitutionalReferenceWireV2, PROTOCOL_VERSION_V2,
        PublicationAttachmentWireV2, PublicationCreateRequest, QualitySubmitReviewRequest,
        REQUEST_FRAME_MAX_BYTES, SessionVerifyPacketRequest, decode_operation_request,
        decode_product_submit_ticket_request_v2, encode_frame,
    };
    use pi_agent_core::scheduler::CancellationToken;
    use pi_agent_protocol::{JsonNumber, JsonValue};
    use std::{path::Path, time::Duration};

    #[test]
    fn shell_reaps_background_descendants_before_joining_pipes() {
        let started = std::time::Instant::now();
        let output = shell(
            Path::new("."),
            "sleep 60 & exit 0",
            Duration::from_secs(2),
            CancellationToken::new(),
        )
        .expect("shell should reap its process group");

        assert_eq!(output.status, 0);
        assert!(!output.timed_out);
        assert!(started.elapsed() < Duration::from_secs(1));
    }

    #[test]
    fn request_json_matches_session_verify_wire_order() {
        let request_id = "factory-pi-host-request-1";
        let actual = request_json(
            "session.verify_packet",
            request_id,
            JsonValue::object([
                ("packet_bytes_b64", JsonValue::String("YWJj".to_owned())),
                ("packet_digest", JsonValue::String("a".repeat(64))),
            ]),
        )
        .expect("canonical request");
        let expected = miniserde::json::to_string(&SessionVerifyPacketRequest {
            protocol_version: PROTOCOL_VERSION_V2,
            request_id: request_id.to_owned(),
            operation: "session.verify_packet".to_owned(),
            packet_digest: "a".repeat(64),
            packet_bytes_b64: "YWJj".to_owned(),
        });
        assert_eq!(actual, expected);

        let frame = encode_frame(actual.as_bytes(), REQUEST_FRAME_MAX_BYTES)
            .expect("encode canonical request");
        decode_operation_request::<SessionVerifyPacketRequest>(
            &frame,
            REQUEST_FRAME_MAX_BYTES,
            "session.verify_packet",
        )
        .expect("daemon accepts canonical request");
    }

    #[test]
    fn request_json_supports_terminal_service_payloads() {
        let candidate = request_json(
            "candidate.submit",
            "factory-pi-host-request-1",
            JsonValue::object([
                (
                    "client_command_id",
                    JsonValue::String("candidate-command".to_owned()),
                ),
                (
                    "expected_revision",
                    JsonValue::Number(JsonNumber::Unsigned(4)),
                ),
                (
                    "commit_subject",
                    JsonValue::String("Fix redirection".to_owned()),
                ),
                (
                    "commit_body",
                    JsonValue::String("Preserve the structured error.".to_owned()),
                ),
                (
                    "regression_test_identity",
                    JsonValue::String("reproducer".to_owned()),
                ),
            ]),
        )
        .expect("canonical candidate request");
        let candidate_frame = encode_frame(candidate.as_bytes(), REQUEST_FRAME_MAX_BYTES)
            .expect("encode candidate request");
        decode_operation_request::<CandidateSubmitRequest>(
            &candidate_frame,
            REQUEST_FRAME_MAX_BYTES,
            "candidate.submit",
        )
        .expect("daemon accepts candidate request");

        let sealed = |artifact_id| {
            JsonValue::object([
                (
                    "artifact_id",
                    JsonValue::Number(JsonNumber::Unsigned(artifact_id)),
                ),
                ("digest", JsonValue::String("a".repeat(64))),
                ("byte_length", JsonValue::Number(JsonNumber::Unsigned(0))),
            ])
        };
        let review = request_json(
            "quality.submit_review",
            "factory-pi-host-request-2",
            JsonValue::object([
                (
                    "client_command_id",
                    JsonValue::String("quality-command".to_owned()),
                ),
                (
                    "expected_revision",
                    JsonValue::Number(JsonNumber::Unsigned(7)),
                ),
                (
                    "full_suite_validation_id",
                    JsonValue::Number(JsonNumber::Unsigned(11)),
                ),
                ("verdict", JsonValue::String("accept".to_owned())),
                ("rationale", sealed(12)),
                ("risks", sealed(13)),
                ("additional_probes", sealed(14)),
            ]),
        )
        .expect("canonical Quality review request");
        let review_frame = encode_frame(review.as_bytes(), REQUEST_FRAME_MAX_BYTES)
            .expect("encode Quality review request");
        decode_operation_request::<QualitySubmitReviewRequest>(
            &review_frame,
            REQUEST_FRAME_MAX_BYTES,
            "quality.submit_review",
        )
        .expect("daemon accepts Quality review request");
    }

    #[test]
    fn request_json_orders_nested_publication_objects() {
        let actual = request_json(
            "publication.create",
            "factory-pi-host-request-1",
            JsonValue::object([
                (
                    "client_command_id",
                    JsonValue::String("publication-command".to_owned()),
                ),
                (
                    "anchor",
                    JsonValue::object([
                        ("id", JsonValue::Number(JsonNumber::Unsigned(7))),
                        ("kind", JsonValue::String("ticket".to_owned())),
                    ]),
                ),
                ("kind", JsonValue::String("Finding".to_owned())),
                ("summary", JsonValue::String("summary".to_owned())),
                (
                    "body_artifact_id",
                    JsonValue::Number(JsonNumber::Unsigned(8)),
                ),
                (
                    "attachments",
                    JsonValue::Array(vec![JsonValue::object([
                        ("label", JsonValue::String("attachment".to_owned())),
                        ("artifact_id", JsonValue::Number(JsonNumber::Unsigned(9))),
                    ])]),
                ),
                ("reply_to", JsonValue::Null),
                ("supersedes", JsonValue::Null),
            ]),
        )
        .expect("canonical nested request");
        let expected = miniserde::json::to_string(&PublicationCreateRequest {
            protocol_version: PROTOCOL_VERSION_V2,
            request_id: "factory-pi-host-request-1".to_owned(),
            operation: "publication.create".to_owned(),
            client_command_id: "publication-command".to_owned(),
            anchor: InstitutionalReferenceWireV2 {
                kind: "ticket".to_owned(),
                id: 7,
            },
            kind: "Finding".to_owned(),
            summary: "summary".to_owned(),
            body_artifact_id: 8,
            attachments: vec![PublicationAttachmentWireV2 {
                artifact_id: 9,
                label: "attachment".to_owned(),
            }],
            reply_to: None,
            supersedes: None,
        });
        assert_eq!(actual, expected);
    }

    #[test]
    fn request_json_orders_nested_product_observation_references() {
        let sealed = |artifact_id| {
            JsonValue::object([
                (
                    "artifact_id",
                    JsonValue::Number(JsonNumber::Unsigned(artifact_id)),
                ),
                ("digest", JsonValue::String("a".repeat(64))),
                ("byte_length", JsonValue::Number(JsonNumber::Unsigned(0))),
            ])
        };
        let observation = |stdout_id, stderr_id| {
            JsonValue::object([
                ("exit_status", JsonValue::Number(JsonNumber::Signed(0))),
                ("stdout", sealed(stdout_id)),
                ("stderr", sealed(stderr_id)),
            ])
        };
        let actual = request_json(
            "product.submit_ticket",
            "factory-pi-host-request-1",
            JsonValue::object([
                (
                    "client_command_id",
                    JsonValue::String("product-command".to_owned()),
                ),
                (
                    "expected_revision",
                    JsonValue::Number(JsonNumber::Unsigned(0)),
                ),
                ("title", JsonValue::String("title".to_owned())),
                ("mission_value", JsonValue::String("mission".to_owned())),
                ("scope", JsonValue::String("scope".to_owned())),
                ("contract_owner", JsonValue::String("owner".to_owned())),
                ("risk", JsonValue::String("risk".to_owned())),
                ("narrative", sealed(1)),
                ("evidence", sealed(2)),
                (
                    "acceptance_criteria",
                    JsonValue::Array(vec![JsonValue::String("criterion".to_owned())]),
                ),
                (
                    "contract_reads",
                    JsonValue::Array(vec![JsonValue::object([
                        ("path", JsonValue::String("AGENTS.md".to_owned())),
                        ("reason", JsonValue::String("reason".to_owned())),
                    ])]),
                ),
                (
                    "duplicate_search",
                    JsonValue::object([
                        ("query", JsonValue::String("query".to_owned())),
                        ("limit", JsonValue::Number(JsonNumber::Unsigned(1))),
                    ]),
                ),
                (
                    "reproducer_profile",
                    JsonValue::String("reproducer".to_owned()),
                ),
                (
                    "reproducer",
                    JsonValue::object([
                        (
                            "comparison_rule_version",
                            JsonValue::Number(JsonNumber::Unsigned(1)),
                        ),
                        ("command", sealed(3)),
                        ("stdin", JsonValue::Null),
                        ("expected_observation", observation(4, 5)),
                        ("first_observation", observation(6, 7)),
                        ("second_observation", observation(8, 9)),
                    ]),
                ),
            ]),
        )
        .expect("canonical product request");
        let frame = encode_frame(actual.as_bytes(), REQUEST_FRAME_MAX_BYTES)
            .expect("encode product request");
        decode_product_submit_ticket_request_v2(&frame)
            .expect("daemon accepts canonical product request");
    }
}
