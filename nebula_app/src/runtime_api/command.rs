//! Runtime 请求解析、输入校验与终端采集边界。

use super::*;

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct TargetParams {
    #[serde(default)]
    window_id: Option<u64>,
    #[serde(default)]
    pane_id: Option<u64>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct WindowParams {
    #[serde(default)]
    window_id: Option<u64>,
    /// `tab.new` 可选：新标签的工作目录（Explorer 右键并入驻留实例时携带）。
    #[serde(default)]
    cwd: Option<PathBuf>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct SplitParams {
    #[serde(default)]
    window_id: Option<u64>,
    #[serde(default)]
    pane_id: Option<u64>,
    direction: RuntimeSplitDirection,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct PromptParams {
    #[serde(default)]
    window_id: Option<u64>,
    pane_id: u64,
    text: String,
    #[serde(default = "default_true")]
    submit: bool,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct ReadParams {
    #[serde(default)]
    window_id: Option<u64>,
    pane_id: u64,
    #[serde(default = "default_read_lines")]
    lines: usize,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct PaneParams {
    #[serde(default)]
    window_id: Option<u64>,
    pane_id: u64,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct SendKeyParams {
    #[serde(default)]
    window_id: Option<u64>,
    pane_id: u64,
    key: RuntimeKey,
    #[serde(default)]
    modifiers: RuntimeKeyModifiers,
    #[serde(default = "default_key_repeat")]
    repeat: u16,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct RunParams {
    #[serde(default)]
    window_id: Option<u64>,
    pane_id: u64,
    command: String,
    #[serde(default = "default_true")]
    wait: bool,
    #[serde(default = "default_run_timeout_ms")]
    timeout_ms: u64,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct SubscribeParams {
    #[serde(default)]
    pub(super) since_revision: Option<u64>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct WaitParams {
    #[serde(default)]
    pub(super) window_id: Option<u64>,
    pub(super) pane_id: u64,
    pub(super) state: RuntimeWaitState,
    pub(super) timeout_ms: u64,
    /// Baseline transition counter captured when the client submitted work.
    /// When present, the wait additionally requires the pane's counter to
    /// advance past it — so a pane that was already in the target state does
    /// not satisfy "wait until it settles again".
    #[serde(default)]
    pub(super) after_seq: Option<u64>,
}

#[derive(Debug, Clone, Copy, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
pub(super) enum RuntimeWaitState {
    Idle,
    Running,
    WaitingInput,
    Attention,
    Finished,
    Failed,
    Settled,
}

pub(super) fn default_true() -> bool {
    true
}

pub(super) fn default_read_lines() -> usize {
    DEFAULT_READ_LINES
}

fn default_key_repeat() -> u16 {
    1
}

fn default_run_timeout_ms() -> u64 {
    COMMAND_TIMEOUT.as_millis() as u64
}

impl RuntimeCommand {
    pub(super) fn from_request(request: &ApiRequest) -> Result<Self, ApiError> {
        match request.method.as_str() {
            "runtime.snapshot" => Ok(Self::Snapshot),
            "window.create" => {
                let params: WindowParams = parse_params(&request.params)?;
                Ok(Self::NewWindow { cwd: params.cwd })
            },
            "window.focus" => {
                let params: TargetParams = parse_params(&request.params)?;
                Ok(Self::Focus { window_id: params.window_id, pane_id: params.pane_id })
            },
            "tab.new" => {
                let params: WindowParams = parse_params(&request.params)?;
                Ok(Self::NewTab { window_id: params.window_id, cwd: params.cwd })
            },
            "pane.split" => {
                let params: SplitParams = parse_params(&request.params)?;
                Ok(Self::Split {
                    window_id: params.window_id,
                    pane_id: params.pane_id,
                    direction: params.direction,
                })
            },
            "pane.prompt" => {
                let params: PromptParams = parse_params(&request.params)?;
                validate_prompt(&params.text)?;
                Ok(Self::Prompt {
                    window_id: params.window_id,
                    pane_id: params.pane_id,
                    text: params.text,
                    submit: params.submit,
                })
            },
            "pane.read" => {
                let params: ReadParams = parse_params(&request.params)?;
                if params.lines == 0 || params.lines > MAX_READ_LINES {
                    return Err(ApiError::invalid_params(format!(
                        "lines must be between 1 and {MAX_READ_LINES}"
                    )));
                }
                Ok(Self::ReadPane {
                    window_id: params.window_id,
                    pane_id: params.pane_id,
                    lines: params.lines,
                })
            },
            "pane.procs" => {
                let params: PaneParams = parse_params(&request.params)?;
                Ok(Self::Procs { window_id: params.window_id, pane_id: params.pane_id })
            },
            "pane.send_key" => {
                let params: SendKeyParams = parse_params(&request.params)?;
                if params.repeat == 0 || params.repeat > MAX_KEY_REPEAT {
                    return Err(ApiError::invalid_params(format!(
                        "repeat must be between 1 and {MAX_KEY_REPEAT}"
                    )));
                }
                if params.key.letter().is_some() && !params.modifiers.control {
                    return Err(ApiError::invalid_params(
                        "letter keys require control=true; use pane.prompt for printable text",
                    ));
                }
                Ok(Self::SendKey {
                    window_id: params.window_id,
                    pane_id: params.pane_id,
                    key: params.key,
                    modifiers: params.modifiers,
                    repeat: params.repeat,
                })
            },
            "pane.run" => {
                let params: RunParams = parse_params(&request.params)?;
                validate_command_line(&params.command)?;
                if params.timeout_ms == 0 || Duration::from_millis(params.timeout_ms) > MAX_WAIT {
                    return Err(ApiError::invalid_params(
                        "timeout_ms must be between 1 and 86400000",
                    ));
                }
                Ok(Self::Run {
                    window_id: params.window_id,
                    pane_id: params.pane_id,
                    command: params.command,
                    wait: params.wait,
                    timeout_ms: params.timeout_ms,
                })
            },
            "agent.start" | "agent.fork" | "agent.prompt" | "agent.read" => {
                agent_api::command_from_request(request)
            },
            method => Err(ApiError::new(
                "method_not_found",
                format!("runtime API method {method:?} does not exist"),
            )),
        }
    }
}

/// Read the logical tail of the terminal model. The range is anchored at the
/// buffer bottom, never at `display_offset`, so a user scrolling through
/// history cannot change what an external agent observes.
pub(crate) fn capture_terminal_tail<T: EventListener>(
    term: &Term<T>,
    window_id: u64,
    pane_id: u64,
    requested_lines: usize,
    task_state: RuntimeTaskState,
    exited: bool,
    exit_reason: Option<String>,
) -> RuntimePaneRead {
    let columns = term.columns();
    let screen_lines = term.screen_lines();
    let total_lines = term.total_lines();
    let history_available = total_lines.saturating_sub(screen_lines);
    if columns == 0 || screen_lines == 0 || total_lines == 0 {
        return RuntimePaneRead {
            window_id,
            pane_id,
            text: String::new(),
            requested_lines,
            returned_lines: 0,
            history_available,
            truncated: false,
            task_state,
            exited,
            exit_reason,
        };
    }

    let mut returned_lines = requested_lines.min(total_lines);
    let end = Point::new(Line(screen_lines as i32 - 1), Column(columns - 1));
    let capture = |lines: usize| {
        let start_line = screen_lines as i64 - lines as i64;
        let start = Point::new(Line(start_line.max(-(history_available as i64)) as i32), Column(0));
        term.bounds_to_string(start, end)
    };
    let mut text = capture(returned_lines);

    // Reduce by whole terminal rows first, preserving exact returned_lines.
    // Only a pathological single row can fall through to UTF-8 byte slicing.
    while text.len() > MAX_READ_BYTES && returned_lines > 1 {
        let estimated = ((returned_lines as u128 * MAX_READ_BYTES as u128) / text.len() as u128)
            .clamp(1, (returned_lines - 1) as u128) as usize;
        returned_lines = estimated;
        text = capture(returned_lines);
    }
    let mut byte_truncated = false;
    if text.len() > MAX_READ_BYTES {
        let mut start = text.len() - MAX_READ_BYTES;
        while !text.is_char_boundary(start) {
            start += 1;
        }
        text = text[start..].to_owned();
        byte_truncated = true;
    }

    RuntimePaneRead {
        window_id,
        pane_id,
        text,
        requested_lines,
        returned_lines,
        history_available,
        truncated: returned_lines < total_lines || byte_truncated,
        task_state,
        exited,
        exit_reason,
    }
}

pub(crate) fn capture_process_tree(
    window_id: u64,
    pane_id: u64,
    root_pid: u32,
) -> Result<RuntimePaneProcesses, ApiError> {
    let entries = crate::process_tree::descendants(root_pid).map_err(|message| {
        ApiError::new("process_query_failed", "failed to read the pane process tree")
            .details(json!({ "root_pid": root_pid, "reason": message }))
    })?;
    let processes = entries
        .into_iter()
        .map(|entry| {
            let agent_kind = crate::ai_agents::AgentKind::parse(&entry.executable)
                .map(|kind| kind.slug().to_owned());
            RuntimeProcess {
                pid: entry.pid,
                parent_pid: (entry.pid != root_pid).then_some(entry.parent_pid),
                display_name: crate::process_tree::display_name(&entry.executable),
                executable: entry.executable,
                depth: entry.depth,
                agent_kind,
            }
        })
        .collect();
    Ok(RuntimePaneProcesses { window_id, pane_id, root_pid, processes })
}

pub(super) fn parse_params<T: DeserializeOwned>(value: &Value) -> Result<T, ApiError> {
    serde_json::from_value(value.clone())
        .map_err(|error| ApiError::invalid_params(format!("invalid method parameters: {error}")))
}

pub(crate) fn validate_prompt(text: &str) -> Result<(), ApiError> {
    if text.is_empty() {
        return Err(ApiError::invalid_params("prompt text must not be empty"));
    }
    if text.len() > MAX_PROMPT_BYTES {
        return Err(ApiError::invalid_params(format!(
            "prompt text exceeds the {MAX_PROMPT_BYTES}-byte limit"
        )));
    }
    if text.chars().any(char::is_control) {
        return Err(ApiError::invalid_params(
            "prompt text contains control characters; pane.prompt accepts one plain-text line",
        ));
    }
    Ok(())
}

pub(crate) fn validate_command_line(command: &str) -> Result<(), ApiError> {
    if command.trim().is_empty() {
        return Err(ApiError::invalid_params("command must not be empty"));
    }
    if command.len() > MAX_PROMPT_BYTES {
        return Err(ApiError::invalid_params(format!(
            "command exceeds the {MAX_PROMPT_BYTES}-byte limit"
        )));
    }
    if command.chars().any(char::is_control) {
        return Err(ApiError::invalid_params(
            "command contains control characters; pane.run accepts one plain-text shell line",
        ));
    }
    Ok(())
}

pub(super) fn validate_agent_name(name: &str) -> Result<(), ApiError> {
    let trimmed = name.trim();
    if trimmed.is_empty() || trimmed.len() > 64 {
        return Err(ApiError::invalid_params("agent name must contain between 1 and 64 bytes"));
    }
    if trimmed != name || name.chars().any(char::is_control) {
        return Err(ApiError::invalid_params(
            "agent name must not have surrounding whitespace or control characters",
        ));
    }
    Ok(())
}

pub(super) fn validate_agent_selector(agent: &str) -> Result<(), ApiError> {
    if agent.trim().is_empty() || agent.len() > 128 || agent.chars().any(char::is_control) {
        return Err(ApiError::invalid_params("agent selector is invalid"));
    }
    Ok(())
}

/// A wait is satisfied only when the pane both reads as the requested state and
/// has moved past the caller's baseline. Without the counter check, waiting on
/// an already-idle pane returns immediately and the caller concludes its work
/// finished before the shell even saw it.
pub(super) fn wait_matches(
    pane: &RuntimePane,
    expected: RuntimeWaitState,
    after_seq: Option<u64>,
) -> bool {
    after_seq.is_none_or(|baseline| pane.state_change_seq > baseline)
        && wait_state_matches(pane.task_state, expected)
}

pub(super) fn wait_state_matches(actual: RuntimeTaskState, expected: RuntimeWaitState) -> bool {
    match expected {
        RuntimeWaitState::Idle => actual == RuntimeTaskState::Idle,
        RuntimeWaitState::Running => actual == RuntimeTaskState::Running,
        RuntimeWaitState::WaitingInput => actual == RuntimeTaskState::WaitingInput,
        RuntimeWaitState::Attention => actual == RuntimeTaskState::Attention,
        RuntimeWaitState::Finished => actual == RuntimeTaskState::Finished,
        RuntimeWaitState::Failed => actual == RuntimeTaskState::Failed,
        RuntimeWaitState::Settled => actual != RuntimeTaskState::Running,
    }
}
