//! Binary-only inherited-descriptor and rooted-workspace adapters.

use factory_pi_host::{
    DaemonError, DaemonFuture, FrameClient, FramedDaemon, LocalToolExecutor,
    MAX_REQUEST_FRAME_BYTES, ToolName,
};
use factory_protocol::{PROTOCOL_VERSION_V1, RepositoryRelativePath, encode_frame};
use pi_agent_core::scheduler::CancellationToken;
use pi_agent_protocol::{JsonNumber, JsonValue};
use std::{
    collections::BTreeMap,
    fs::{self, File, OpenOptions},
    io::{self, Read},
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
        number(u64::from(PROTOCOL_VERSION_V1))?,
    );
    object.insert(
        "request_id".to_owned(),
        JsonValue::String(request_id.to_owned()),
    );
    object.insert(
        "operation".to_owned(),
        JsonValue::String(operation.to_owned()),
    );
    JsonValue::Object(object)
        .to_json_string()
        .map_err(|error| DaemonError::new(error.to_string()))
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
    let mut child = Command::new("/bin/sh")
        .arg("-lc")
        .arg(command)
        .current_dir(root)
        .env_clear()
        .env("NO_COLOR", "1")
        .env("PATH", "/usr/bin:/bin")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(io_error)?;
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
            break;
        }
        if cancellation.is_cancelled() {
            let _ = child.kill();
            return Err(DaemonError::new("shell invocation was cancelled"));
        }
        if Instant::now() >= deadline {
            timed_out = true;
            let _ = child.kill();
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
