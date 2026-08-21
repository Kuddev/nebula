//! Runtime control CLI client.

use super::*;

fn client_stream(
    endpoint: &Endpoint,
    request: &ApiRequest,
    timeout: Option<Duration>,
) -> Result<TcpStream, IoError> {
    let mut stream = TcpStream::connect_timeout(&endpoint_addr(&endpoint), CONNECT_TIMEOUT)?;
    stream.set_read_timeout(timeout)?;
    stream.set_write_timeout(Some(IO_TIMEOUT))?;
    serde_json::to_writer(&mut stream, request).map_err(IoError::other)?;
    stream.write_all(b"\n")?;
    stream.flush()?;
    stream.shutdown(Shutdown::Write)?;
    Ok(stream)
}

pub(super) fn request_once(
    method: &str,
    params: Value,
    timeout: Duration,
) -> Result<ApiResponse, Box<dyn Error>> {
    let endpoint = read_endpoint()
        .ok_or_else(|| CliError::new("runtime_unavailable", "no resident Nebula runtime found"))?;
    let request = ApiRequest::new(endpoint.token.clone(), method, params);
    let stream = client_stream(&endpoint, &request, Some(timeout))?;
    let mut line = String::new();
    BufReader::new(stream).read_line(&mut line)?;
    if line.is_empty() {
        return Err(CliError::new(
            "runtime_no_response",
            "runtime closed the connection without a response",
        )
        .into());
    }
    Ok(serde_json::from_str(&line)?)
}

fn print_response(response: &ApiResponse, pretty: bool) -> Result<(), Box<dyn Error>> {
    if pretty {
        println!("{}", serde_json::to_string_pretty(response)?);
    } else {
        println!("{}", serde_json::to_string(response)?);
    }
    if response.ok {
        Ok(())
    } else {
        let error =
            response.error.as_ref().map_or("runtime request failed", |error| &error.message);
        Err(PrintedCliError(error.to_owned()).into())
    }
}

/// CLI adapter. It deliberately speaks the same serialized protocol as any
/// external client instead of calling Processor helpers in-process.
pub fn run_cli(options: ControlOptions) -> Result<(), Box<dyn Error>> {
    let pretty = options.pretty;
    match run_cli_inner(options) {
        result @ Ok(_) => result,
        Err(error) if error.downcast_ref::<PrintedCliError>().is_some() => Err(error),
        Err(error) => {
            let code =
                error.downcast_ref::<CliError>().map_or("cli_transport_error", |error| error.code);
            let response = ApiResponse::failure("cli", ApiError::new(code, error.to_string()));
            print_response(&response, pretty)
        },
    }
}

fn run_cli_inner(options: ControlOptions) -> Result<(), Box<dyn Error>> {
    if options.timeout_ms == 0 || Duration::from_millis(options.timeout_ms) > MAX_WAIT {
        return Err(
            CliError::new("invalid_params", "--timeout-ms must be between 1 and 86400000").into()
        );
    }
    let timeout = Duration::from_millis(options.timeout_ms);
    match options.command {
        CliCommand::Describe => {
            let response = request_once("runtime.describe", json!({}), timeout)?;
            print_response(&response, options.pretty)
        },
        CliCommand::Snapshot => {
            let response = request_once("runtime.snapshot", json!({}), timeout)?;
            print_response(&response, options.pretty)
        },
        CliCommand::Orchestrate { spec, file } => {
            let source = match (spec, file) {
                (Some(spec), None) => spec,
                (None, Some(path)) => std::fs::read_to_string(path)?,
                _ => {
                    return Err(CliError::new(
                        "invalid_params",
                        "exactly one of --spec or --file is required",
                    )
                    .into());
                },
            };
            let params: Value = serde_json::from_str(&source).map_err(|error| {
                CliError::new(
                    "invalid_params",
                    format!("workflow is not valid UTF-8 JSON: {error}"),
                )
            })?;
            let response = request_once(
                "runtime.orchestrate",
                params,
                timeout.saturating_add(Duration::from_secs(1)),
            )?;
            print_response(&response, options.pretty)
        },
        CliCommand::Agents { window } => {
            let response = request_once("agents.list", json!({ "window_id": window }), timeout)?;
            print_response(&response, options.pretty)
        },
        CliCommand::AgentStart { window, name, kind, cwd, resume_session_id } => {
            let response = request_once(
                "agent.start",
                json!({
                    "window_id": window,
                    "name": name,
                    "kind": kind,
                    "cwd": cwd,
                    "resume_session_id": resume_session_id
                }),
                timeout,
            )?;
            print_response(&response, options.pretty)
        },
        CliCommand::AgentFork {
            window,
            source_pane,
            source_cwd,
            name,
            kind,
            resume_session_id,
            branch,
            base,
            path,
            allow_dirty_source,
        } => {
            let source_cwd = source_cwd.map(agent_api::absolute_cli_path).transpose()?;
            let path = path.map(agent_api::absolute_cli_path).transpose()?;
            let response = request_once(
                "agent.fork",
                json!({
                    "window_id": window,
                    "source_pane_id": source_pane,
                    "source_cwd": source_cwd,
                    "name": name,
                    "kind": kind,
                    "resume_session_id": resume_session_id,
                    "branch": branch,
                    "base": base,
                    "path": path,
                    "allow_dirty_source": allow_dirty_source
                }),
                timeout.saturating_add(COMMAND_TIMEOUT),
            )?;
            print_response(&response, options.pretty)
        },
        CliCommand::AgentGet { agent, generation } => {
            let response = request_once(
                "agent.get",
                json!({ "agent": agent, "generation": generation }),
                timeout,
            )?;
            print_response(&response, options.pretty)
        },
        CliCommand::AgentPrompt { agent, generation, text, no_submit } => {
            let response = request_once(
                "agent.prompt",
                json!({
                    "agent": agent,
                    "generation": generation,
                    "text": text,
                    "submit": !no_submit
                }),
                timeout,
            )?;
            print_response(&response, options.pretty)
        },
        CliCommand::AgentRead { agent, generation, lines } => {
            let response = request_once(
                "agent.read",
                json!({ "agent": agent, "generation": generation, "lines": lines }),
                timeout,
            )?;
            print_response(&response, options.pretty)
        },
        CliCommand::AgentWait { agent, generation, state, after_seq } => {
            let response = request_once(
                "agent.wait",
                json!({
                    "agent": agent,
                    "generation": generation,
                    "state": wait_state_name(state),
                    "timeout_ms": timeout.as_millis() as u64,
                    "after_seq": after_seq
                }),
                timeout.saturating_add(Duration::from_secs(1)),
            )?;
            print_response(&response, options.pretty)
        },
        CliCommand::Subscribe { since } => subscribe_cli(since, timeout),
        CliCommand::NewWindow => {
            let response = request_once("window.create", json!({}), timeout)?;
            print_response(&response, options.pretty)
        },
        CliCommand::Focus { window, pane } => {
            let response = request_once(
                "window.focus",
                json!({ "window_id": window, "pane_id": pane }),
                timeout,
            )?;
            print_response(&response, options.pretty)
        },
        CliCommand::NewTab { window } => {
            let response = request_once("tab.new", json!({ "window_id": window }), timeout)?;
            print_response(&response, options.pretty)
        },
        CliCommand::Split { window, pane, direction } => {
            let direction = match direction {
                ControlSplitDirection::Right => RuntimeSplitDirection::LeftRight,
                ControlSplitDirection::Down => RuntimeSplitDirection::TopBottom,
            };
            let response = request_once(
                "pane.split",
                json!({ "window_id": window, "pane_id": pane, "direction": direction }),
                timeout,
            )?;
            print_response(&response, options.pretty)
        },
        CliCommand::Prompt { window, pane, text, no_submit, wait } => {
            let response = request_once(
                "pane.prompt",
                json!({
                    "window_id": window,
                    "pane_id": pane,
                    "text": text,
                    "submit": !no_submit
                }),
                timeout,
            )?;
            if !response.ok || wait.is_none() {
                return print_response(&response, options.pretty);
            }
            // The prompt response carries the snapshot taken immediately after
            // submission. Using its counter as the baseline is what makes the
            // follow-up wait mean "settled again", not "already settled".
            let baseline = pane_state_change_seq(&response, window, pane);
            wait_cli(window, pane, wait.expect("checked above"), baseline, timeout, options.pretty)
        },
        CliCommand::Read { window, pane, lines } => {
            let response = request_once(
                "pane.read",
                json!({ "window_id": window, "pane_id": pane, "lines": lines }),
                timeout,
            )?;
            print_response(&response, options.pretty)
        },
        CliCommand::Procs { window, pane } => {
            let response = request_once(
                "pane.procs",
                json!({ "window_id": window, "pane_id": pane }),
                timeout,
            )?;
            print_response(&response, options.pretty)
        },
        CliCommand::SendKey { window, pane, key, shift, alt, control, repeat } => {
            let response = request_once(
                "pane.send_key",
                json!({
                    "window_id": window,
                    "pane_id": pane,
                    "key": key,
                    "modifiers": {
                        "shift": shift,
                        "alt": alt,
                        "control": control
                    },
                    "repeat": repeat
                }),
                timeout,
            )?;
            print_response(&response, options.pretty)
        },
        CliCommand::Run { window, pane, command, no_wait } => {
            let response = request_once(
                "pane.run",
                json!({
                    "window_id": window,
                    "pane_id": pane,
                    "command": command,
                    "wait": !no_wait,
                    "timeout_ms": options.timeout_ms
                }),
                timeout.saturating_add(Duration::from_secs(1)),
            )?;
            print_response(&response, options.pretty)
        },
        CliCommand::Wait { window, pane, state, after_seq } => {
            wait_cli(window, pane, state, after_seq, timeout, options.pretty)
        },
    }
}

/// Dig a pane's transition counter out of a command response's embedded
/// snapshot. Returns `None` when the shape is unexpected, which degrades the
/// follow-up wait to plain state matching rather than failing the command.
fn pane_state_change_seq(
    response: &ApiResponse,
    window_id: Option<u64>,
    pane_id: u64,
) -> Option<u64> {
    let snapshot = response.result.as_ref()?.get("snapshot")?;
    let snapshot: RuntimeSnapshot = serde_json::from_value(snapshot.clone()).ok()?;
    snapshot.pane(window_id, pane_id).ok().map(|pane| pane.state_change_seq)
}

fn wait_cli(
    window: Option<u64>,
    pane: u64,
    state: ControlWaitState,
    after_seq: Option<u64>,
    timeout: Duration,
    pretty: bool,
) -> Result<(), Box<dyn Error>> {
    let state = wait_state_name(state);
    let response = request_once(
        "pane.wait",
        json!({
            "window_id": window,
            "pane_id": pane,
            "state": state,
            "timeout_ms": timeout.as_millis() as u64,
            "after_seq": after_seq
        }),
        timeout.saturating_add(Duration::from_secs(1)),
    )?;
    print_response(&response, pretty)
}

fn wait_state_name(state: ControlWaitState) -> &'static str {
    match state {
        ControlWaitState::Idle => "idle",
        ControlWaitState::Running => "running",
        ControlWaitState::WaitingInput => "waiting_input",
        ControlWaitState::Attention => "attention",
        ControlWaitState::Finished => "finished",
        ControlWaitState::Failed => "failed",
        ControlWaitState::Settled => "settled",
    }
}

fn subscribe_cli(since: Option<u64>, timeout: Duration) -> Result<(), Box<dyn Error>> {
    let endpoint = read_endpoint()
        .ok_or_else(|| CliError::new("runtime_unavailable", "no resident Nebula runtime found"))?;
    let request = ApiRequest::new(
        endpoint.token.clone(),
        "events.subscribe",
        json!({ "since_revision": since }),
    );
    let stream = client_stream(&endpoint, &request, Some(timeout))?;
    let mut reader = BufReader::new(stream);
    let mut line = String::new();
    if reader.read_line(&mut line)? == 0 {
        return Err(CliError::new(
            "runtime_no_response",
            "runtime closed the subscription without an acknowledgement",
        )
        .into());
    }
    print!("{line}");
    std::io::stdout().flush()?;
    reader.get_mut().set_read_timeout(None)?;
    loop {
        line.clear();
        if reader.read_line(&mut line)? == 0 {
            return Ok(());
        }
        print!("{line}");
        std::io::stdout().flush()?;
    }
}

#[derive(Debug)]
struct CliError {
    code: &'static str,
    message: String,
}

impl CliError {
    fn new(code: &'static str, message: impl Into<String>) -> Self {
        Self { code, message: message.into() }
    }
}

impl fmt::Display for CliError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.message)
    }
}

impl Error for CliError {}

#[derive(Debug)]
struct PrintedCliError(String);

impl fmt::Display for PrintedCliError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.0)
    }
}

impl Error for PrintedCliError {}
