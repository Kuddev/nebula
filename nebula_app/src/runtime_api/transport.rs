use super::*;

pub(super) fn serve(listener: TcpListener, token: String, sink: EventSink, hub: RuntimeHub) {
    let active = Arc::new(AtomicUsize::new(0));
    for stream in listener.incoming() {
        let Ok(stream) = stream else { continue };
        if active.fetch_add(1, Ordering::AcqRel) >= MAX_CLIENTS {
            active.fetch_sub(1, Ordering::AcqRel);
            continue;
        }
        let active = active.clone();
        let token = token.clone();
        let sink = sink.clone();
        let hub = hub.clone();
        let _ = std::thread::Builder::new().name("nebula-runtime-client".into()).spawn(move || {
            let _guard = ActiveClient(active);
            if let Err(error) = handle_connection(stream, &token, &sink, &hub) {
                log::debug!("Runtime API client disconnected: {error}");
            }
        });
    }
}

struct ActiveClient(Arc<AtomicUsize>);

impl Drop for ActiveClient {
    fn drop(&mut self) {
        self.0.fetch_sub(1, Ordering::AcqRel);
    }
}

pub(super) fn handle_connection(
    mut stream: TcpStream,
    token: &str,
    sink: &EventSink,
    hub: &RuntimeHub,
) -> Result<(), IoError> {
    stream.set_read_timeout(Some(IO_TIMEOUT))?;
    stream.set_write_timeout(Some(IO_TIMEOUT))?;
    let reader = BufReader::new(stream.try_clone()?);
    let mut limited = reader.take((MAX_REQUEST_BYTES + 1) as u64);
    let mut line = String::new();
    limited.read_line(&mut line)?;
    if line.len() > MAX_REQUEST_BYTES {
        return write_response(
            &mut stream,
            &ApiResponse::failure(
                "unknown",
                ApiError::new("request_too_large", "request too large"),
            ),
        );
    }

    if !line.trim_start().starts_with('{') {
        return handle_legacy(&mut stream, &line, token, sink);
    }

    let raw: Value = match serde_json::from_str(&line) {
        Ok(raw) => raw,
        Err(error) => {
            return write_response(
                &mut stream,
                &ApiResponse::failure(
                    "unknown",
                    ApiError::new("invalid_request", format!("invalid JSON: {error}")),
                ),
            );
        },
    };
    // Authentication precedes detailed validation so an unauthenticated local
    // process cannot use parse errors as a protocol oracle.
    if raw.get("token").and_then(Value::as_str) != Some(token) {
        return Ok(());
    }
    let id = raw.get("id").and_then(Value::as_str).unwrap_or("unknown").to_owned();
    let request: ApiRequest = match serde_json::from_value(raw) {
        Ok(request) => request,
        Err(error) => {
            return write_response(
                &mut stream,
                &ApiResponse::failure(
                    id,
                    ApiError::new("invalid_request", format!("invalid request envelope: {error}")),
                ),
            );
        },
    };

    if request.protocol != PROTOCOL_NAME || request.version != PROTOCOL_VERSION {
        let error = ApiError::new(
            "protocol_version_mismatch",
            format!(
                "requested protocol {:?} v{}; this runtime supports {} v{}",
                request.protocol, request.version, PROTOCOL_NAME, PROTOCOL_VERSION
            ),
        )
        .details(json!({ "supported_versions": SUPPORTED_VERSIONS }));
        return write_response(&mut stream, &ApiResponse::failure(request.id, error));
    }

    match request.method.as_str() {
        "runtime.describe" => {
            write_response(&mut stream, &ApiResponse::success(request.id, runtime_description()))
        },
        "events.subscribe" => subscribe_connection(&mut stream, request, hub),
        "agents.list" => agent_api::agents_connection(&mut stream, request, hub),
        "agent.get" => agent_api::agent_get_connection(&mut stream, request, hub),
        "agent.delegate" => agent_api::agent_delegate_connection(&mut stream, request, sink, hub),
        "agent.wait" => agent_api::agent_wait_connection(&mut stream, request, hub),
        "pane.wait" => wait_connection(&mut stream, request, hub),
        "runtime.orchestrate" => {
            orchestrate::orchestrate_connection(&mut stream, request, sink, hub)
        },
        _ => dispatch_connection(&mut stream, request, sink, hub),
    }
}

pub(super) fn handle_legacy(
    stream: &mut TcpStream,
    line: &str,
    token: &str,
    sink: &EventSink,
) -> Result<(), IoError> {
    let mut parts = line.split_whitespace();
    let verb = parts.next().unwrap_or("");
    if parts.next() != Some(token) {
        return Ok(());
    }
    match verb {
        "ATTACH" => {
            sink.emit_attach();
            stream.write_all(b"OK\n")
        },
        "PING" => stream.write_all(b"OK\n"),
        _ => Ok(()),
    }
}

pub(super) fn runtime_description() -> Value {
    json!({
        "app_version": env!("VERSION"),
        "protocol": PROTOCOL_NAME,
        "protocol_version": PROTOCOL_VERSION,
        "supported_versions": SUPPORTED_VERSIONS,
        "schema": "docs/runtime-api-v1.schema.json",
        "capabilities": [
            "runtime.describe",
            "runtime.snapshot",
            "runtime.orchestrate",
            "events.subscribe",
            "agents.list",
            "agent.start",
            "agent.fork",
            "agent.get",
            "agent.delegate",
            "agent.prompt",
            "agent.paste",
            "agent.read",
            "agent.wait",
            "window.create",
            "window.close",
            "window.focus",
            "tab.new",
            "tab.close",
            "tab.rename",
            "tab.move",
            "pane.split",
            "pane.close",
            "pane.zoom",
            "pane.resize",
            "pane.prompt",
            "pane.paste",
            "pane.read",
            "pane.procs",
            "pane.send_key",
            "pane.run",
            "pane.exec",
            "pane.wait"
        ],
        // Additive params cannot be detected from `capabilities`: an older
        // build ignores an unknown `after_seq` and still races. Clients that
        // need the guarantee should require the feature string.
        "features": [
            "pane.wait.after_seq",
            "pane.wait.lifecycle",
            "agent.wait.identity",
            "agent.delegate.callback",
            "agent.fork.transactional_worktree",
            "agent.worktree.provenance",
            "events.pane_lifecycle"
            ,"runtime.orchestrate.typed_steps"
            ,"runtime.orchestrate.agent_ready"
            ,"env.pane_identity"
            ,"cli.resource_verbs"
            ,"cli.paste_sources"
            ,"layout.typed_mutations"
            ,"pane.exec.non_tty"
        ],
        // 环境契约：pane 里的进程靠这些变量发现自己和控制面，不必扫进程或猜
        // 端口。写进 describe 是为了让外部客户端能**探测**契约而不是硬编码变量
        // 名——旧版本没有这一段，客户端据此回落到 `nebula ctl` 即可。
        // 实现见 `crate::agent_env`。
        "env": {
            "term_program": crate::agent_env::TERM_PROGRAM,
            "pane": crate::agent_env::PANE_ENV,
            "cli": crate::agent_env::CLI_ENV,
            "bin_dir": crate::agent_env::BIN_DIR_ENV,
            // 远端 pane 只带身份、不带控制面路径：那台机器上没有这个二进制。
            "remote_marker": "NEBULA_PANE_REMOTE",
            "bin_dir_on_path": true
        },
        // 资源 + 动词命令是同一批能力的易发现入口，语义与 `capabilities` 完全一致。
        "commands": {
            "env": ["env"],
            "window": ["close"],
            "tab": ["close", "rename", "move"],
            "pane": ["list", "read", "send", "paste", "wait", "exec", "close", "zoom", "resize"],
            "agent": ["list", "send", "delegate", "paste", "read", "wait"]
        }
    })
}

pub(super) fn subscribe_connection(
    stream: &mut TcpStream,
    request: ApiRequest,
    hub: &RuntimeHub,
) -> Result<(), IoError> {
    let params: SubscribeParams = match parse_params(&request.params) {
        Ok(params) => params,
        Err(error) => return write_response(stream, &ApiResponse::failure(request.id, error)),
    };
    let (subscription_id, current, receiver) = hub.subscribe();
    write_response(
        stream,
        &ApiResponse::success(
            request.id,
            json!({
                "subscription_id": subscription_id,
                "current_revision": current.as_ref().map_or(0, |snapshot| snapshot.revision)
            }),
        ),
    )?;
    stream.set_write_timeout(None)?;
    if let Some(snapshot) = current
        && params.since_revision.is_none_or(|revision| snapshot.revision > revision)
    {
        write_json_line(stream, &ApiEvent::snapshot(snapshot))?;
    }
    while let Ok(snapshot) = receiver.recv() {
        write_json_line(stream, &ApiEvent::snapshot(snapshot))?;
    }
    Ok(())
}

pub(super) fn wait_connection(
    stream: &mut TcpStream,
    request: ApiRequest,
    hub: &RuntimeHub,
) -> Result<(), IoError> {
    let params: WaitParams = match parse_params(&request.params) {
        Ok(params) => params,
        Err(error) => return write_response(stream, &ApiResponse::failure(request.id, error)),
    };
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

    let (_, current, receiver) = hub.subscribe();
    // Remember what the pane last looked like so a timeout can report why it
    // never matched rather than only that it did not.
    let mut observed = None;
    let mut target_window = params.window_id;
    if let Some(snapshot) = current {
        match snapshot.pane_target(target_window, params.pane_id) {
            Ok((window_id, pane)) => {
                target_window = Some(window_id);
                if let Some(error) = hub.pane_lifecycle_error(target_window, params.pane_id) {
                    return write_response(stream, &ApiResponse::failure(request.id, error));
                }
                if wait_matches(pane, params.state, params.after_seq) {
                    return write_response(
                        stream,
                        &ApiResponse::success(
                            request.id,
                            serde_json::to_value(snapshot).map_err(IoError::other)?,
                        ),
                    );
                }
                observed = Some((pane.task_state, pane.state_change_seq));
            },
            Err(error) if error.code == "target_not_found" => {
                if let Some(lifecycle) = hub.pane_lifecycle_error(target_window, params.pane_id) {
                    return write_response(stream, &ApiResponse::failure(request.id, lifecycle));
                }
            },
            Err(error) => {
                return write_response(stream, &ApiResponse::failure(request.id, error));
            },
        }
    } else if let Some(error) = hub.pane_lifecycle_error(target_window, params.pane_id) {
        return write_response(stream, &ApiResponse::failure(request.id, error));
    }

    let deadline = Instant::now() + timeout;
    loop {
        let remaining = deadline.saturating_duration_since(Instant::now());
        if remaining.is_zero() {
            break;
        }
        let snapshot = match receiver.recv_timeout(remaining) {
            Ok(snapshot) => snapshot,
            Err(RecvTimeoutError::Timeout | RecvTimeoutError::Disconnected) => break,
        };
        if let Some(error) = hub.pane_lifecycle_error(target_window, params.pane_id) {
            return write_response(stream, &ApiResponse::failure(request.id, error));
        }
        match snapshot.pane_target(target_window, params.pane_id) {
            Ok((window_id, pane)) => {
                target_window = Some(window_id);
                if wait_matches(pane, params.state, params.after_seq) {
                    return write_response(
                        stream,
                        &ApiResponse::success(
                            request.id,
                            serde_json::to_value(snapshot).map_err(IoError::other)?,
                        ),
                    );
                }
                observed = Some((pane.task_state, pane.state_change_seq));
            },
            Err(error) => {
                return write_response(stream, &ApiResponse::failure(request.id, error));
            },
        }
    }

    if let Some(error) = hub.pane_lifecycle_error(target_window, params.pane_id) {
        return write_response(stream, &ApiResponse::failure(request.id, error));
    }

    let detail = match observed {
        Some((state, seq)) => format!("last observed state {state:?} at state_change_seq {seq}"),
        None => "the pane was never present in a published snapshot".to_owned(),
    };
    write_response(
        stream,
        &ApiResponse::failure(
            request.id,
            ApiError::new(
                "timeout",
                format!(
                    "pane {} did not reach the requested state before timeout; {detail}",
                    params.pane_id
                ),
            )
            .details(json!({
                "pane_id": params.pane_id,
                "window_id": target_window,
                "after_seq": params.after_seq,
                "observed_state_change_seq": observed.map(|(_, seq)| seq)
            })),
        ),
    )
}

pub(super) fn dispatch_connection(
    stream: &mut TcpStream,
    request: ApiRequest,
    sink: &EventSink,
    hub: &RuntimeHub,
) -> Result<(), IoError> {
    let command = match RuntimeCommand::from_request(&request) {
        Ok(command) => command,
        Err(error) => return write_response(stream, &ApiResponse::failure(request.id, error)),
    };
    match &command {
        RuntimeCommand::SendKey { window_id, pane_id, key, modifiers, repeat } => {
            info!(
                "runtime pane.send_key request_id={} window_id={window_id:?} pane_id={pane_id} key={} shift={} alt={} control={} repeat={repeat}",
                request.id,
                key.as_str(),
                modifiers.shift,
                modifiers.alt,
                modifiers.control,
            );
        },
        RuntimeCommand::AgentStart {
            window_id, pane_id, name, kind, session_id, worktree, ..
        } => {
            info!(
                "runtime agent.start request_id={} window_id={window_id:?} pane_id={pane_id:?} name={} kind={} resume={} worktree={}",
                request.id,
                name,
                kind.slug(),
                session_id.is_some(),
                worktree.is_some()
            );
        },
        RuntimeCommand::AgentFork { name, kind, .. } => {
            info!(
                "runtime agent.fork request_id={} name={} kind={}",
                request.id,
                name,
                kind.slug()
            );
        },
        RuntimeCommand::AgentPrompt { agent, generation, text, submit } => {
            info!(
                "runtime agent.prompt request_id={} agent={} generation={generation:?} submit={submit} prompt_bytes={}",
                request.id,
                agent,
                text.len()
            );
        },
        RuntimeCommand::AgentPaste { agent, generation, text, submit } => {
            info!(
                "runtime agent.paste request_id={} agent={} generation={generation:?} submit={submit} paste_bytes={}",
                request.id,
                agent,
                text.len()
            );
        },
        RuntimeCommand::AgentRead { agent, generation, lines } => {
            info!(
                "runtime agent.read request_id={} agent={} generation={generation:?} lines={lines}",
                request.id, agent
            );
        },
        RuntimeCommand::Exec { window_id, pane_id, argv, timeout_ms, max_output_bytes } => {
            info!(
                "runtime pane.exec request_id={} window_id={window_id:?} pane_id={pane_id} argv_len={} timeout_ms={timeout_ms} max_output_bytes={max_output_bytes}",
                request.id,
                argv.len()
            );
        },
        _ => {},
    }
    let response = match dispatch_runtime_command(command, sink, hub) {
        Ok(result) => ApiResponse::success(request.id, result),
        Err(error) => ApiResponse::failure(request.id, error),
    };
    write_response(stream, &response)
}

/// 执行一个已经解析的 Runtime 原语。普通请求与编排步骤共享这里，避免
/// worktree 事务、run 等待和 UI 超时在两条路径上逐渐产生不同语义。
pub(super) fn dispatch_runtime_command(
    command: RuntimeCommand,
    sink: &EventSink,
    hub: &RuntimeHub,
) -> Result<Value, ApiError> {
    let (command, mut worktree_transaction) = agent_api::prepare_dispatch_command(command, hub)?;
    let run_wait = match &command {
        RuntimeCommand::Run { wait: true, timeout_ms, .. } => {
            Some(Duration::from_millis(*timeout_ms))
        },
        _ => None,
    };
    let dispatch_timeout = match &command {
        RuntimeCommand::Exec { timeout_ms, .. } => {
            Duration::from_millis(*timeout_ms).saturating_add(Duration::from_secs(2))
        },
        _ => COMMAND_TIMEOUT,
    };
    let (dispatch, receiver) = RuntimeDispatch::new(command);
    if !sink.emit_control(dispatch) {
        return Err(agent_api::rollback_prepared_worktree(
            ApiError::new("runtime_unavailable", "Nebula's event loop is not available"),
            worktree_transaction.take(),
        ));
    }
    match receiver.recv_timeout(dispatch_timeout) {
        Ok(Ok(mut result)) => {
            if let Some(transaction) = worktree_transaction.take() {
                let provenance = transaction.commit();
                agent_api::attach_worktree_result(&mut result, &provenance);
            }
            if let Some(timeout) = run_wait {
                let action = result.get("action").unwrap_or(&result);
                let target = (
                    action.get("window_id").and_then(Value::as_u64),
                    action.get("pane_id").and_then(Value::as_u64),
                    action.get("run_id").and_then(Value::as_u64),
                );
                match target {
                    (Some(window_id), Some(pane_id), Some(run_id)) => {
                        let run = wait_run_phased(hub, sink, window_id, pane_id, run_id, timeout)?;
                        Ok(json!({ "run": run, "snapshot": hub.current() }))
                    },
                    _ => Err(ApiError::new(
                        "invalid_runtime_response",
                        "pane.run did not return its window, pane, and run identity",
                    )),
                }
            } else {
                Ok(result)
            }
        },
        Ok(Err(error)) => {
            Err(agent_api::rollback_prepared_worktree(error, worktree_transaction.take()))
        },
        Err(_) => {
            let mut error =
                ApiError::new("runtime_timeout", "runtime event thread did not answer in time");
            if let Some(transaction) = worktree_transaction.take() {
                // UI dispatch may still arrive after the response channel times out. The
                // checkout must stay alive because a late-created PTY can already use it.
                let provenance = transaction.commit();
                error.details = Some(json!({
                    "worktree": provenance,
                    "cleanup_deferred": true,
                    "reason": "the UI outcome is unknown; Nebula did not remove the worktree"
                }));
            }
            Err(error)
        },
    }
}

pub(super) fn write_response(
    stream: &mut TcpStream,
    response: &ApiResponse,
) -> Result<(), IoError> {
    write_json_line(stream, response)
}

pub(super) fn write_json_line<T: Serialize>(
    stream: &mut TcpStream,
    value: &T,
) -> Result<(), IoError> {
    serde_json::to_writer(&mut *stream, value).map_err(IoError::other)?;
    stream.write_all(b"\n")?;
    stream.flush()
}
