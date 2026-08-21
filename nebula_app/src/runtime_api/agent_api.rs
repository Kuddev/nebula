//! 命名 Agent API 的解析、查询、等待与事务型 worktree 准备。

use super::*;

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct AgentsParams {
    #[serde(default)]
    window_id: Option<u64>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct AgentStartParams {
    #[serde(default)]
    window_id: Option<u64>,
    #[serde(default)]
    pane_id: Option<u64>,
    name: String,
    kind: String,
    #[serde(default)]
    cwd: Option<PathBuf>,
    #[serde(default)]
    resume_session_id: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct AgentForkParams {
    #[serde(default)]
    window_id: Option<u64>,
    #[serde(default)]
    source_pane_id: Option<u64>,
    #[serde(default)]
    source_cwd: Option<PathBuf>,
    name: String,
    kind: String,
    #[serde(default)]
    resume_session_id: Option<String>,
    #[serde(default)]
    branch: Option<String>,
    #[serde(default)]
    base: Option<String>,
    #[serde(default)]
    path: Option<PathBuf>,
    #[serde(default)]
    allow_dirty_source: bool,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct AgentTargetParams {
    agent: String,
    #[serde(default)]
    generation: Option<u64>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct AgentPromptParams {
    agent: String,
    #[serde(default)]
    generation: Option<u64>,
    text: String,
    #[serde(default = "default_true")]
    submit: bool,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct AgentReadParams {
    agent: String,
    #[serde(default)]
    generation: Option<u64>,
    #[serde(default = "default_read_lines")]
    lines: usize,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct AgentWaitParams {
    agent: String,
    generation: u64,
    state: RuntimeWaitState,
    timeout_ms: u64,
    #[serde(default)]
    after_seq: Option<u64>,
}

pub(super) fn command_from_request(request: &ApiRequest) -> Result<RuntimeCommand, ApiError> {
    match request.method.as_str() {
        "agent.start" => {
            let params: AgentStartParams = parse_params(&request.params)?;
            validate_agent_name(&params.name)?;
            if params.pane_id.is_some() && params.cwd.is_some() {
                return Err(ApiError::invalid_params(
                    "agent.start cannot combine pane_id with cwd; an existing pane keeps its shell cwd",
                ));
            }
            let (kind, session_id, command) =
                verified_agent_launch(&params.kind, params.resume_session_id)?;
            Ok(RuntimeCommand::AgentStart {
                window_id: params.window_id,
                pane_id: params.pane_id,
                name: params.name,
                kind,
                cwd: params.cwd,
                session_id,
                command,
                worktree: None,
            })
        },
        "agent.fork" => {
            let params: AgentForkParams = parse_params(&request.params)?;
            validate_agent_name(&params.name)?;
            if params.source_pane_id.is_none() && params.source_cwd.is_none() {
                return Err(ApiError::invalid_params("source_pane_id or source_cwd is required"));
            }
            let (kind, session_id, command) =
                verified_agent_launch(&params.kind, params.resume_session_id)?;
            Ok(RuntimeCommand::AgentFork {
                window_id: params.window_id,
                source_pane_id: params.source_pane_id,
                source_cwd: params.source_cwd,
                name: params.name,
                kind,
                session_id,
                command,
                branch: params.branch,
                base: params.base,
                path: params.path,
                allow_dirty_source: params.allow_dirty_source,
            })
        },
        "agent.prompt" => {
            let params: AgentPromptParams = parse_params(&request.params)?;
            validate_agent_selector(&params.agent)?;
            validate_prompt(&params.text)?;
            Ok(RuntimeCommand::AgentPrompt {
                agent: params.agent,
                generation: params.generation,
                text: params.text,
                submit: params.submit,
            })
        },
        "agent.read" => {
            let params: AgentReadParams = parse_params(&request.params)?;
            validate_agent_selector(&params.agent)?;
            if params.lines == 0 || params.lines > MAX_READ_LINES {
                return Err(ApiError::invalid_params(format!(
                    "lines must be between 1 and {MAX_READ_LINES}"
                )));
            }
            Ok(RuntimeCommand::AgentRead {
                agent: params.agent,
                generation: params.generation,
                lines: params.lines,
            })
        },
        _ => unreachable!("non-agent runtime method routed to agent parser"),
    }
}

fn verified_agent_launch(
    kind: &str,
    resume_session_id: Option<String>,
) -> Result<(crate::ai_agents::AgentKind, Option<String>, String), ApiError> {
    let kind = crate::ai_agents::AgentKind::parse(kind)
        .ok_or_else(|| ApiError::invalid_params(format!("unknown agent kind {kind:?}")))?;
    let command = match resume_session_id.as_deref() {
        Some(session_id) => kind.resume_command(session_id).ok_or_else(|| {
            ApiError::new(
                "agent_resume_unsupported",
                "the agent kind or session id does not have a verified resume command",
            )
        })?,
        None => kind.start_command().ok_or_else(|| {
            ApiError::new(
                "agent_launch_unsupported",
                format!("cold start is not verified for agent kind {:?}", kind.slug()),
            )
        })?,
    };
    Ok((kind, resume_session_id, command))
}

pub(super) fn prepare_dispatch_command(
    command: RuntimeCommand,
    hub: &RuntimeHub,
) -> Result<(RuntimeCommand, Option<crate::git_worktree::WorktreeTransaction>), ApiError> {
    let RuntimeCommand::AgentFork {
        window_id,
        source_pane_id,
        source_cwd,
        name,
        kind,
        session_id,
        command,
        branch,
        base,
        path,
        allow_dirty_source,
    } = command
    else {
        return Ok((command, None));
    };

    hub.ensure_agent_name_available(&name)?;
    let (window_id, source_cwd) = match source_pane_id {
        Some(pane_id) => {
            let snapshot = hub.current().ok_or_else(|| {
                ApiError::new(
                    "runtime_unavailable",
                    "no runtime snapshot is available to resolve source_pane_id",
                )
            })?;
            let (resolved_window, pane) = snapshot.pane_target(window_id, pane_id)?;
            if pane.ssh_destination.is_some() {
                return Err(ApiError::new(
                    "remote_worktree_unsupported",
                    "agent.fork cannot create a local Git worktree from an SSH pane",
                )
                .details(json!({ "window_id": resolved_window, "pane_id": pane_id })));
            }
            if pane.cwd.trim().is_empty() {
                return Err(ApiError::new(
                    "cwd_unavailable",
                    "the source pane has not reported a working directory",
                )
                .details(json!({ "window_id": resolved_window, "pane_id": pane_id })));
            }
            let cwd = PathBuf::from(&pane.cwd);
            if !cwd.is_absolute() {
                return Err(ApiError::new(
                    "cwd_unavailable",
                    "the source pane did not report an absolute working directory",
                )
                .details(json!({
                    "window_id": resolved_window,
                    "pane_id": pane_id,
                    "cwd": pane.cwd
                })));
            }
            (Some(resolved_window), cwd)
        },
        None => (
            window_id,
            source_cwd.expect("agent.fork validation requires source_cwd without a pane"),
        ),
    };

    let transaction =
        crate::git_worktree::WorktreeTransaction::prepare(crate::git_worktree::WorktreeRequest {
            source_cwd,
            agent_name: name.clone(),
            branch,
            base,
            path,
            allow_dirty_source,
        })?;
    let provenance = transaction.provenance().clone();
    let cwd = provenance.path.clone();
    Ok((
        RuntimeCommand::AgentStart {
            window_id,
            pane_id: None,
            name,
            kind,
            cwd: Some(cwd),
            session_id,
            command,
            worktree: Some(provenance),
        },
        Some(transaction),
    ))
}

pub(super) fn rollback_prepared_worktree(
    mut error: ApiError,
    transaction: Option<crate::git_worktree::WorktreeTransaction>,
) -> ApiError {
    if let Some(transaction) = transaction
        && let Err(rollback) = transaction.rollback()
    {
        error.details = Some(json!({
            "operation": error.details,
            "rollback": rollback
        }));
    }
    error
}

pub(super) fn attach_worktree_result(
    result: &mut Value,
    provenance: &crate::git_worktree::WorktreeProvenance,
) {
    let encoded = serde_json::to_value(provenance).unwrap_or(Value::Null);
    if let Some(Value::Object(action)) = result.get_mut("action") {
        action.insert("worktree".to_owned(), encoded);
    } else if let Value::Object(action) = result {
        action.insert("worktree".to_owned(), encoded);
    }
}

pub(super) fn agents_connection(
    stream: &mut TcpStream,
    request: ApiRequest,
    hub: &RuntimeHub,
) -> Result<(), IoError> {
    let params: AgentsParams = match parse_params(&request.params) {
        Ok(params) => params,
        Err(error) => return write_response(stream, &ApiResponse::failure(request.id, error)),
    };
    let Some(snapshot) = hub.current() else {
        return write_response(
            stream,
            &ApiResponse::failure(
                request.id,
                ApiError::new(
                    "runtime_unavailable",
                    "Nebula has not published its first workspace snapshot yet",
                ),
            ),
        );
    };
    if let Some(window_id) = params.window_id
        && !snapshot.windows.iter().any(|window| window.id == window_id)
    {
        return write_response(
            stream,
            &ApiResponse::failure(
                request.id,
                ApiError::new("target_not_found", format!("window {window_id} does not exist")),
            ),
        );
    }

    let mut agents = Vec::new();
    for window in
        snapshot.windows.iter().filter(|window| params.window_id.is_none_or(|id| window.id == id))
    {
        for tab in &window.tabs {
            for pane in &tab.panes {
                let Some(agent) = pane.agent.clone() else { continue };
                agents.push(RuntimeAgentPane {
                    window_id: window.id,
                    tab_index: tab.index,
                    tab_active: tab.active,
                    pane_id: pane.id,
                    pane_active: pane.active,
                    title: pane.title.clone(),
                    cwd: pane.cwd.clone(),
                    branch: pane.branch.clone(),
                    ssh_destination: pane.ssh_destination.clone(),
                    agent,
                    task_state: pane.task_state,
                    state_change_seq: pane.state_change_seq,
                });
            }
        }
    }
    write_response(
        stream,
        &ApiResponse::success(
            request.id,
            json!({ "revision": snapshot.revision, "agents": agents }),
        ),
    )
}

pub(super) fn agent_get_connection(
    stream: &mut TcpStream,
    request: ApiRequest,
    hub: &RuntimeHub,
) -> Result<(), IoError> {
    let params: AgentTargetParams = match parse_params(&request.params) {
        Ok(params) => params,
        Err(error) => return write_response(stream, &ApiResponse::failure(request.id, error)),
    };
    if let Err(error) = validate_agent_selector(&params.agent) {
        return write_response(stream, &ApiResponse::failure(request.id, error));
    }
    info!(
        "runtime agent.get request_id={} agent={} generation={:?}",
        request.id, params.agent, params.generation
    );
    let agent = match hub.managed_agent(&params.agent, params.generation, false) {
        Ok(agent) => agent,
        Err(error) => return write_response(stream, &ApiResponse::failure(request.id, error)),
    };
    let pane = hub
        .current()
        .and_then(|snapshot| snapshot.pane(Some(agent.window_id), agent.pane_id).ok().cloned());
    write_response(
        stream,
        &ApiResponse::success(request.id, json!({ "agent": agent, "pane": pane })),
    )
}

pub(super) fn agent_wait_connection(
    stream: &mut TcpStream,
    request: ApiRequest,
    hub: &RuntimeHub,
) -> Result<(), IoError> {
    let params: AgentWaitParams = match parse_params(&request.params) {
        Ok(params) => params,
        Err(error) => return write_response(stream, &ApiResponse::failure(request.id, error)),
    };
    if let Err(error) = validate_agent_selector(&params.agent) {
        return write_response(stream, &ApiResponse::failure(request.id, error));
    }
    info!(
        "runtime agent.wait request_id={} agent={} generation={} state={:?} after_seq={:?}",
        request.id, params.agent, params.generation, params.state, params.after_seq
    );
    let timeout = Duration::from_millis(params.timeout_ms);
    if timeout.is_zero() || timeout > MAX_WAIT {
        return write_response(
            stream,
            &ApiResponse::failure(
                request.id,
                ApiError::invalid_params("timeout_ms must be between 1 and 86400000"),
            ),
        );
    }
    let agent = match hub.active_agent(&params.agent, Some(params.generation)) {
        Ok(agent) => agent,
        Err(error) => return write_response(stream, &ApiResponse::failure(request.id, error)),
    };
    let (_, current, receiver) = hub.subscribe();
    let deadline = Instant::now() + timeout;
    let mut snapshot = current;
    let mut observed = None;
    loop {
        if let Err(error) = hub.active_agent(&agent.agent_id, Some(agent.generation)) {
            return write_response(stream, &ApiResponse::failure(request.id, error));
        }
        if let Some(current) = snapshot.take() {
            match current.pane(Some(agent.window_id), agent.pane_id) {
                Ok(pane) if wait_matches(pane, params.state, params.after_seq) => {
                    let active = match hub.active_agent(&agent.agent_id, Some(agent.generation)) {
                        Ok(active) => active,
                        Err(error) => {
                            return write_response(
                                stream,
                                &ApiResponse::failure(request.id, error),
                            );
                        },
                    };
                    return write_response(
                        stream,
                        &ApiResponse::success(
                            request.id,
                            json!({ "agent": active, "snapshot": current }),
                        ),
                    );
                },
                Ok(pane) => observed = Some((pane.task_state, pane.state_change_seq)),
                Err(error) => {
                    let error = hub
                        .active_agent(&agent.agent_id, Some(agent.generation))
                        .err()
                        .unwrap_or_else(|| {
                            ApiError::new("agent_closed", error.message).details(json!({
                                "agent_id": agent.agent_id,
                                "generation": agent.generation
                            }))
                        });
                    return write_response(stream, &ApiResponse::failure(request.id, error));
                },
            }
        }
        let remaining = deadline.saturating_duration_since(Instant::now());
        if remaining.is_zero() {
            break;
        }
        match receiver.recv_timeout(remaining) {
            Ok(next) => snapshot = Some(next),
            Err(RecvTimeoutError::Timeout) => break,
            Err(RecvTimeoutError::Disconnected) => {
                return write_response(
                    stream,
                    &ApiResponse::failure(
                        request.id,
                        ApiError::new("runtime_unavailable", "runtime subscription disconnected"),
                    ),
                );
            },
        }
    }
    write_response(
        stream,
        &ApiResponse::failure(
            request.id,
            ApiError::new(
                "timeout",
                format!("agent {:?} did not reach the requested state before timeout", agent.name),
            )
            .details(json!({
                "agent_id": agent.agent_id,
                "generation": agent.generation,
                "after_seq": params.after_seq,
                "observed_state": observed.map(|(state, _)| state),
                "observed_state_change_seq": observed.map(|(_, seq)| seq)
            })),
        ),
    )
}

pub(super) fn absolute_cli_path(path: PathBuf) -> Result<PathBuf, IoError> {
    if path.is_absolute() { Ok(path) } else { Ok(std::env::current_dir()?.join(path)) }
}
