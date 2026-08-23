use super::command::wait_state_matches;
use super::*;

fn snapshot(state: RuntimeTaskState) -> RuntimeSnapshot {
    RuntimeSnapshot::new(
        0,
        vec![RuntimeWindow {
            id: 7,
            focused: true,
            session_exempt: false,
            active_tab: 0,
            focused_pane_id: Some(3),
            tabs: vec![RuntimeTab {
                index: 0,
                active: true,
                label: "test".into(),
                kind: "shell".into(),
                bell: false,
                focused_pane_id: Some(3),
                layout: Some(RuntimeLayout::Pane { pane_id: 3 }),
                panes: vec![RuntimePane {
                    id: 3,
                    active: true,
                    title: "shell".into(),
                    cwd: "D:/work".into(),
                    branch: "main".into(),
                    ssh_destination: None,
                    running_program: None,
                    agent: None,
                    task_state: state,
                    state_change_seq: 0,
                    active_run: None,
                    last_run: None,
                }],
            }],
        }],
    )
}

fn detected_agent(kind: &str, session_id: Option<&str>) -> RuntimeAgent {
    RuntimeAgent {
        agent_id: None,
        generation: None,
        name: None,
        worktree: None,
        kind: kind.to_owned(),
        display_name: kind.to_owned(),
        session_id: session_id.map(str::to_owned),
        state_source: RuntimeAgentStateSource::Hook,
        state_rule: None,
        hook_seen: true,
    }
}

fn call_wait_connection(hub: &RuntimeHub, request: ApiRequest) -> ApiResponse {
    let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).unwrap();
    let mut client = TcpStream::connect(listener.local_addr().unwrap()).unwrap();
    let (mut server, _) = listener.accept().unwrap();
    wait_connection(&mut server, request, hub).unwrap();
    let mut line = String::new();
    BufReader::new(&mut client).read_line(&mut line).unwrap();
    serde_json::from_str(&line).unwrap()
}

#[test]
fn hub_revisions_change_only_when_semantic_state_changes() {
    let hub = RuntimeHub::new();
    let first = hub.publish(snapshot(RuntimeTaskState::Idle));
    let duplicate = hub.publish(snapshot(RuntimeTaskState::Idle));
    let changed = hub.publish(snapshot(RuntimeTaskState::Running));
    assert_eq!(first.revision, 1);
    assert_eq!(duplicate.revision, 1);
    assert_eq!(changed.revision, 2);
}

#[test]
fn subscribers_receive_the_canonical_revision() {
    let hub = RuntimeHub::new();
    hub.publish(snapshot(RuntimeTaskState::Idle));
    let (_, current, receiver) = hub.subscribe();
    assert_eq!(current.unwrap().revision, 1);
    hub.publish(snapshot(RuntimeTaskState::Running));
    assert_eq!(receiver.recv_timeout(Duration::from_millis(50)).unwrap().revision, 2);
}

#[test]
fn prompt_rejects_terminal_control_sequences() {
    assert!(validate_prompt("please inspect the build").is_ok());
    assert!(validate_prompt("unsafe\u{1b}[2J").is_err());
    assert!(validate_prompt("two\nlines").is_err());
}

#[test]
fn runtime_capabilities_match_the_versioned_schema() {
    let schema: Value =
        serde_json::from_str(include_str!("../../../docs/runtime-api-v1.schema.json"))
            .expect("runtime schema must be valid JSON");
    let schema_methods: std::collections::BTreeSet<_> =
        schema["$defs"]["request"]["properties"]["method"]["enum"]
            .as_array()
            .expect("schema method enum")
            .iter()
            .map(|method| method.as_str().expect("method string"))
            .collect();
    let described = runtime_description();
    let described_methods: std::collections::BTreeSet<_> = described["capabilities"]
        .as_array()
        .expect("runtime capabilities")
        .iter()
        .map(|method| method.as_str().expect("capability string"))
        .collect();
    assert_eq!(schema_methods, described_methods);
}

#[test]
fn agent_start_exposes_only_verified_launch_contracts() {
    let cold = ApiRequest::new(
        "token".into(),
        "agent.start",
        json!({ "name": "reviewer", "kind": "codex" }),
    );
    assert!(matches!(
        RuntimeCommand::from_request(&cold),
        Ok(RuntimeCommand::AgentStart {
            kind: crate::ai_agents::AgentKind::Codex,
            session_id: None,
            ref command,
            ..
        }) if command == "codex"
    ));

    let unsupported = ApiRequest::new(
        "token".into(),
        "agent.start",
        json!({ "name": "reviewer", "kind": "gemini" }),
    );
    assert_eq!(
        RuntimeCommand::from_request(&unsupported).unwrap_err().code,
        "agent_launch_unsupported"
    );

    let invalid_resume = ApiRequest::new(
        "token".into(),
        "agent.start",
        json!({
            "name": "reviewer",
            "kind": "codex",
            "resume_session_id": "thread; calc"
        }),
    );
    assert_eq!(
        RuntimeCommand::from_request(&invalid_resume).unwrap_err().code,
        "agent_resume_unsupported"
    );
}

#[test]
fn agent_fork_prepares_a_runtime_agent_start_with_provenance() {
    let repository = test_git_repository();
    let target = repository.path().join("isolated-review");
    let request = ApiRequest::new(
        "token".into(),
        "agent.fork",
        json!({
            "source_cwd": repository.path(),
            "name": "reviewer",
            "kind": "codex",
            "branch": "nebula/runtime-reviewer",
            "path": target
        }),
    );
    let parsed = RuntimeCommand::from_request(&request).expect("agent.fork should parse");
    let (prepared, transaction) = agent_api::prepare_dispatch_command(parsed, &RuntimeHub::new())
        .expect("worktree should prepare");
    match prepared {
        RuntimeCommand::AgentStart { cwd, worktree: Some(worktree), .. } => {
            assert_eq!(cwd.as_deref(), Some(worktree.path.as_path()));
            assert_eq!(worktree.branch, "nebula/runtime-reviewer");
            assert!(!worktree.base_commit.is_empty());
        },
        _ => panic!("agent.fork must become a prepared AgentStart"),
    }
    transaction
        .expect("agent.fork owns a transaction")
        .rollback()
        .expect("prepared resources should roll back");
}

#[test]
fn agent_fork_requires_an_explicit_source() {
    let request = ApiRequest::new(
        "token".into(),
        "agent.fork",
        json!({ "name": "reviewer", "kind": "codex" }),
    );
    assert_eq!(RuntimeCommand::from_request(&request).unwrap_err().code, "invalid_params");
}

#[test]
fn managed_agent_names_are_unique_and_generations_are_stable() {
    let hub = RuntimeHub::new();
    let first = hub
        .register_agent("reviewer".into(), crate::ai_agents::AgentKind::Codex, 7, 3, None, None)
        .unwrap();
    assert_eq!(first.generation, 1);
    assert_eq!(
        hub.ensure_agent_name_available("reviewer").unwrap_err().code,
        "agent_name_conflict"
    );

    hub.close_agent(&first.agent_id, "agent_exited");
    let closed = hub.managed_agent("reviewer", Some(1), false).unwrap();
    assert!(!closed.active);
    assert_eq!(closed.closed_reason.as_deref(), Some("agent_exited"));

    let second = hub
        .register_agent("reviewer".into(), crate::ai_agents::AgentKind::Codex, 7, 4, None, None)
        .unwrap();
    assert_eq!(second.generation, 2);
    assert_eq!(hub.active_agent("reviewer", Some(1)).unwrap_err().code, "agent_exited");
    assert_eq!(hub.active_agent("reviewer", Some(99)).unwrap_err().code, "agent_identity_mismatch");
}

#[test]
fn agent_fork_rolls_back_when_ui_launch_fails() {
    let repository = test_git_repository();
    let target = repository.path().join("failed-agent");
    let request = ApiRequest::new(
        "token".into(),
        "agent.fork",
        json!({
            "source_cwd": repository.path(),
            "name": "failed-agent",
            "kind": "codex",
            "branch": "nebula/failed-agent",
            "path": target
        }),
    );
    let sink = EventSink::Callback(Arc::new(|callback| {
        if let RuntimeCallback::Control(dispatch) = callback {
            assert!(matches!(
                &dispatch.command,
                RuntimeCommand::AgentStart { worktree: Some(_), .. }
            ));
            dispatch.respond(Err(ApiError::new("action_failed", "simulated UI launch failure")));
        }
    }));
    let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).unwrap();
    let mut client = TcpStream::connect(listener.local_addr().unwrap()).unwrap();
    let (mut server, _) = listener.accept().unwrap();
    dispatch_connection(&mut server, request, &sink, &RuntimeHub::new()).unwrap();
    let mut line = String::new();
    BufReader::new(&mut client).read_line(&mut line).unwrap();
    let response: ApiResponse = serde_json::from_str(&line).unwrap();
    assert!(!response.ok);
    assert_eq!(response.error.unwrap().code, "action_failed");
    assert!(!target.exists());
    assert!(
        !std::process::Command::new("git")
            .arg("-C")
            .arg(repository.path())
            .args(["show-ref", "--verify", "--quiet", "refs/heads/nebula/failed-agent"])
            .status()
            .expect("query branch")
            .success()
    );
}

#[test]
fn managed_agent_keeps_worktree_provenance() {
    let hub = RuntimeHub::new();
    let provenance = crate::git_worktree::WorktreeProvenance {
        repo_root: PathBuf::from("D:/repo"),
        source_root: PathBuf::from("D:/repo"),
        path: PathBuf::from("D:/repo-worktrees/reviewer"),
        branch: "nebula/reviewer".into(),
        base_commit: "0123456789012345678901234567890123456789".into(),
        created: true,
    };
    let managed = hub
        .register_agent(
            "reviewer".into(),
            crate::ai_agents::AgentKind::Codex,
            7,
            3,
            None,
            Some(provenance.clone()),
        )
        .unwrap();
    assert_eq!(managed.worktree.as_ref(), Some(&provenance));
    assert_eq!(
        hub.active_agent(&managed.agent_id, Some(managed.generation)).unwrap().worktree.as_ref(),
        Some(&provenance)
    );
    let mut observed = snapshot(RuntimeTaskState::Running);
    observed.windows[0].tabs[0].panes[0].agent = Some(detected_agent("codex", None));
    let projected = hub.publish(observed);
    assert_eq!(
        projected.windows[0].tabs[0].panes[0]
            .agent
            .as_ref()
            .and_then(|agent| agent.worktree.as_ref()),
        Some(&provenance)
    );
}

fn test_git_repository() -> tempfile::TempDir {
    let directory = tempfile::tempdir().expect("create repository directory");
    let git = |args: &[&str]| {
        std::process::Command::new("git")
            .arg("-C")
            .arg(directory.path())
            .args(args)
            .output()
            .expect("run git")
    };
    assert!(git(&["init", "--initial-branch=main"]).status.success());
    std::fs::write(directory.path().join("tracked.txt"), "tracked").expect("write tracked file");
    assert!(git(&["add", "tracked.txt"]).status.success());
    assert!(
        git(&[
            "-c",
            "user.name=Nebula Test",
            "-c",
            "user.email=nebula@example.invalid",
            "commit",
            "-m",
            "initial"
        ])
        .status
        .success()
    );
    directory
}

#[test]
fn closing_an_agent_wakes_identity_aware_waiters() {
    let hub = RuntimeHub::new();
    let agent = hub
        .register_agent("reviewer".into(), crate::ai_agents::AgentKind::Codex, 7, 3, None, None)
        .unwrap();
    hub.publish(snapshot(RuntimeTaskState::Idle));
    let (_, _, receiver) = hub.subscribe();

    hub.close_agent(&agent.agent_id, "pane_closed");
    let wake = receiver.recv_timeout(Duration::from_millis(50)).unwrap();
    assert_eq!(wake.revision, 1);
    assert_eq!(
        hub.active_agent(&agent.agent_id, Some(agent.generation)).unwrap_err().code,
        "pane_closed"
    );
}

#[test]
fn managed_identity_requires_real_agent_and_session_evidence() {
    let hub = RuntimeHub::new();
    let managed = hub
        .register_agent(
            "reviewer".into(),
            crate::ai_agents::AgentKind::Codex,
            7,
            3,
            Some("thread-1".into()),
            None,
        )
        .unwrap();

    let no_evidence = hub.publish(snapshot(RuntimeTaskState::Running));
    assert!(no_evidence.pane(Some(7), 3).unwrap().agent.is_none());
    assert!(!hub.managed_agent(&managed.agent_id, None, false).unwrap().observed);

    let mut observed = snapshot(RuntimeTaskState::Running);
    observed.windows[0].tabs[0].panes[0].agent = Some(detected_agent("codex", Some("thread-1")));
    let projected = hub.publish(observed);
    let projected_agent = projected.pane(Some(7), 3).unwrap().agent.as_ref().unwrap();
    assert_eq!(projected_agent.agent_id.as_deref(), Some(managed.agent_id.as_str()));
    assert_eq!(projected_agent.generation, Some(1));
    assert_eq!(projected_agent.name.as_deref(), Some("reviewer"));

    let mut replacement = snapshot(RuntimeTaskState::Running);
    replacement.windows[0].tabs[0].panes[0].agent = Some(detected_agent("codex", Some("thread-2")));
    let replacement = hub.publish(replacement);
    assert!(replacement.pane(Some(7), 3).unwrap().agent.as_ref().unwrap().agent_id.is_none());
    let closed = hub.managed_agent(&managed.agent_id, None, false).unwrap();
    assert!(!closed.active);
    assert_eq!(closed.closed_reason.as_deref(), Some("agent_replaced"));
    assert_eq!(
        hub.active_agent(&managed.agent_id, Some(managed.generation)).unwrap_err().code,
        "agent_replaced"
    );
}

#[test]
fn removed_panes_publish_closed_tombstones() {
    let hub = RuntimeHub::new();
    hub.publish(snapshot(RuntimeTaskState::Idle));
    let (_, _, receiver) = hub.subscribe();

    let closed = hub.publish(RuntimeSnapshot::new(0, Vec::new()));
    assert_eq!(closed.revision, 2);
    assert_eq!(closed.pane_lifecycles.len(), 1);
    assert_eq!(closed.pane_lifecycles[0].window_id, 7);
    assert_eq!(closed.pane_lifecycles[0].pane_id, 3);
    assert_eq!(closed.pane_lifecycles[0].event, RuntimePaneLifecycleKind::Closed);
    assert_eq!(hub.pane_lifecycle_error(Some(7), 3).unwrap().code, "pane_closed");
    assert_eq!(receiver.recv_timeout(Duration::from_millis(50)).unwrap(), closed);
}

#[test]
fn live_pane_relocation_preserves_runtime_identity() {
    let hub = RuntimeHub::new();
    let mut before = snapshot(RuntimeTaskState::Running);
    before.windows[0].tabs[0].panes[0].active_run =
        Some(RuntimePaneRun { run_id: 51, phase: RuntimeRunPhase::Started });
    let before = hub.publish(before);
    assert_eq!(before.pane(Some(7), 3).unwrap().state_change_seq, 1);

    let agent = hub
        .register_agent("reviewer".into(), crate::ai_agents::AgentKind::Codex, 7, 3, None, None)
        .unwrap();
    let (sender, _receiver) = std::sync::mpsc::sync_channel(1);
    hub.lock().run_waiters.insert((7, 3, 51), vec![(1, sender)]);

    hub.move_panes_to_window(7, 8, &[3]);
    let mut after = snapshot(RuntimeTaskState::Running);
    after.windows[0].id = 8;
    after.windows[0].tabs[0].panes[0].active_run =
        Some(RuntimePaneRun { run_id: 51, phase: RuntimeRunPhase::Started });
    let moved = hub.publish(after);

    assert!(moved.pane_lifecycles.is_empty());
    assert!(hub.pane_lifecycle_error(Some(7), 3).is_none());
    assert_eq!(moved.pane(Some(8), 3).unwrap().state_change_seq, 1);
    let moved_agent = hub.managed_agent(&agent.agent_id, None, false).unwrap();
    assert!(moved_agent.active);
    assert_eq!(moved_agent.window_id, 8);

    let state = hub.lock();
    assert!(!state.run_waiters.contains_key(&(7, 3, 51)));
    assert!(state.run_waiters.contains_key(&(8, 3, 51)));
    assert!(state.completed_runs.is_empty());
}

#[test]
fn explicit_pane_exit_precedes_the_following_ui_close() {
    let hub = RuntimeHub::new();
    hub.publish(snapshot(RuntimeTaskState::Running));
    let (_, _, receiver) = hub.subscribe();

    hub.record_pane_exited(7, 3);
    let exited = receiver.recv_timeout(Duration::from_millis(50)).unwrap();
    assert_eq!(exited.revision, 2);
    assert_eq!(exited.pane_lifecycles[0].event, RuntimePaneLifecycleKind::Exited);
    assert_eq!(hub.pane_lifecycle_error(Some(7), 3).unwrap().code, "pane_exited");

    hub.record_pane_closed(7, 3);
    assert_eq!(hub.current().unwrap().revision, 2);
    assert_eq!(hub.current().unwrap().pane_lifecycles[0].event, RuntimePaneLifecycleKind::Exited);

    let response = call_wait_connection(
        &hub,
        ApiRequest::new(
            "token".into(),
            "pane.wait",
            json!({
                "window_id": 7,
                "pane_id": 3,
                "state": "settled",
                "timeout_ms": 1000
            }),
        ),
    );
    assert!(!response.ok);
    assert_eq!(response.error.unwrap().code, "pane_exited");
}

#[test]
fn pane_lifecycle_closes_managed_agents_with_the_same_cause() {
    let hub = RuntimeHub::new();
    let agent = hub
        .register_agent(
            "reviewer".into(),
            crate::ai_agents::AgentKind::Codex,
            7,
            3,
            Some("thread-1".into()),
            None,
        )
        .unwrap();
    let mut running = snapshot(RuntimeTaskState::Running);
    running.windows[0].tabs[0].panes[0].agent = Some(detected_agent("codex", Some("thread-1")));
    hub.publish(running);

    hub.record_pane_exited(7, 3);
    let closed = hub.managed_agent(&agent.agent_id, None, false).unwrap();
    assert!(!closed.active);
    assert_eq!(closed.closed_reason.as_deref(), Some("pane_exited"));
    assert_eq!(
        hub.active_agent(&agent.agent_id, Some(agent.generation)).unwrap_err().code,
        "pane_exited"
    );
}

#[test]
fn pane_lifecycle_identity_is_window_local() {
    let hub = RuntimeHub::new();
    hub.record_pane_closed(7, 3);
    hub.record_pane_exited(8, 3);
    assert_eq!(hub.pane_lifecycle_error(Some(7), 3).unwrap().code, "pane_closed");
    assert_eq!(hub.pane_lifecycle_error(Some(8), 3).unwrap().code, "pane_exited");
    assert_eq!(hub.pane_lifecycle_error(None, 3).unwrap().code, "ambiguous_target");
}

#[test]
fn send_key_accepts_only_the_restricted_control_contract() {
    let valid = ApiRequest::new(
        "token".into(),
        "pane.send_key",
        json!({
            "pane_id": 3,
            "key": "c",
            "modifiers": { "control": true },
            "repeat": 2
        }),
    );
    assert!(matches!(
        RuntimeCommand::from_request(&valid),
        Ok(RuntimeCommand::SendKey { key: RuntimeKey::C, repeat: 2, .. })
    ));

    let printable =
        ApiRequest::new("token".into(), "pane.send_key", json!({ "pane_id": 3, "key": "c" }));
    assert_eq!(RuntimeCommand::from_request(&printable).unwrap_err().code, "invalid_params");

    let arbitrary_bytes = ApiRequest::new(
        "token".into(),
        "pane.send_key",
        json!({ "pane_id": 3, "key": "escape", "bytes": [27, 91, 50, 74] }),
    );
    assert_eq!(RuntimeCommand::from_request(&arbitrary_bytes).unwrap_err().code, "invalid_params");
}

#[test]
fn run_requires_one_plain_shell_line() {
    let valid = ApiRequest::new(
        "token".into(),
        "pane.run",
        json!({ "pane_id": 3, "command": "cargo test", "wait": true }),
    );
    assert!(matches!(
        RuntimeCommand::from_request(&valid),
        Ok(RuntimeCommand::Run { wait: true, .. })
    ));

    let multiline = ApiRequest::new(
        "token".into(),
        "pane.run",
        json!({ "pane_id": 3, "command": "echo one\necho two" }),
    );
    assert_eq!(RuntimeCommand::from_request(&multiline).unwrap_err().code, "invalid_params");
}

#[test]
fn run_outcome_requires_a_real_start_and_exit_code() {
    let submitted = RuntimePaneRun { run_id: 41, phase: RuntimeRunPhase::Submitted };
    let no_start = RuntimeRunOutcome::command_done(submitted, Some(0));
    assert_eq!(no_start.state, RuntimeRunState::Unavailable);
    assert_eq!(no_start.unavailable_reason.as_deref(), Some("command_start_not_observed"));

    let started = RuntimePaneRun { run_id: 42, phase: RuntimeRunPhase::Started };
    assert_eq!(RuntimeRunOutcome::command_done(started, Some(0)).state, RuntimeRunState::Finished);
    assert_eq!(RuntimeRunOutcome::command_done(started, Some(7)).state, RuntimeRunState::Failed);
    let missing_code = RuntimeRunOutcome::command_done(started, None);
    assert_eq!(missing_code.state, RuntimeRunState::Unavailable);
    assert_eq!(missing_code.exit_code_capability, ExitCodeCapability::Unavailable);
}

#[test]
fn completed_run_cache_closes_the_waiter_registration_race() {
    let hub = RuntimeHub::new();
    let mut running = snapshot(RuntimeTaskState::Running);
    running.windows[0].tabs[0].panes[0].active_run =
        Some(RuntimePaneRun { run_id: 51, phase: RuntimeRunPhase::Started });
    hub.publish(running);

    let mut done = snapshot(RuntimeTaskState::Finished);
    done.windows[0].tabs[0].panes[0].last_run = Some(RuntimeRunOutcome::command_done(
        RuntimePaneRun { run_id: 51, phase: RuntimeRunPhase::Started },
        Some(0),
    ));
    hub.publish(done);

    // The result was published before this waiter existed. The bounded
    // cache must still return the exact run rather than timing out.
    let result = hub.wait_run(7, 3, 51, Duration::from_millis(10)).unwrap();
    assert_eq!(result.outcome.exit_code, Some(0));
}

#[test]
fn settled_wait_excludes_only_running() {
    assert!(!wait_state_matches(RuntimeTaskState::Running, RuntimeWaitState::Settled));
    assert!(wait_state_matches(RuntimeTaskState::WaitingInput, RuntimeWaitState::Settled));
    assert!(wait_state_matches(RuntimeTaskState::Failed, RuntimeWaitState::Settled));
}

#[test]
fn state_change_seq_advances_only_on_transitions() {
    let hub = RuntimeHub::new();
    let seq = |snapshot: &RuntimeSnapshot| snapshot.pane(None, 3).unwrap().state_change_seq;

    let first = hub.publish(snapshot(RuntimeTaskState::Idle));
    assert_eq!(seq(&first), 1, "a newly seen pane starts at 1, never 0");
    // A duplicate publish is deduped, which only holds because the stamp
    // carried the counter forward instead of bumping it.
    assert_eq!(seq(&hub.publish(snapshot(RuntimeTaskState::Idle))), 1);
    assert_eq!(seq(&hub.publish(snapshot(RuntimeTaskState::Running))), 2);
    assert_eq!(seq(&hub.publish(snapshot(RuntimeTaskState::Idle))), 3);
}

#[test]
fn wait_ignores_a_pane_that_never_left_the_target_state() {
    let hub = RuntimeHub::new();
    let idle = hub.publish(snapshot(RuntimeTaskState::Idle));
    let pane = idle.pane(None, 3).unwrap();

    // Without a baseline, an already-idle pane satisfies "wait for idle".
    assert!(wait_matches(pane, RuntimeWaitState::Idle, None));
    // With the baseline captured at submit time, it must not: this is the
    // race where a wait returned before the shell had started the command.
    assert!(!wait_matches(pane, RuntimeWaitState::Idle, Some(pane.state_change_seq)));

    let running = hub.publish(snapshot(RuntimeTaskState::Running));
    let settled = hub.publish(snapshot(RuntimeTaskState::Idle));
    assert!(!wait_matches(
        running.pane(None, 3).unwrap(),
        RuntimeWaitState::Idle,
        Some(pane.state_change_seq)
    ));
    assert!(wait_matches(
        settled.pane(None, 3).unwrap(),
        RuntimeWaitState::Idle,
        Some(pane.state_change_seq)
    ));
}

#[test]
fn state_change_seq_does_not_leak_across_windows() {
    // Pane ids are window-local, so pane 3 in window 8 must not inherit
    // window 7's counter and appear to have already transitioned.
    let hub = RuntimeHub::new();
    hub.publish(snapshot(RuntimeTaskState::Running));
    let mut relabelled = snapshot(RuntimeTaskState::Idle);
    relabelled.windows[0].id = 8;
    let published = hub.publish(relabelled);
    assert_eq!(published.pane(Some(8), 3).unwrap().state_change_seq, 1);
}

#[test]
fn protocol_schema_is_valid_json_and_tracks_v1() {
    let schema: Value =
        serde_json::from_str(include_str!("../../../docs/runtime-api-v1.schema.json")).unwrap();
    assert_eq!(schema["properties"]["version"]["const"], PROTOCOL_VERSION);
}

#[test]
fn terminal_tail_reads_buffer_bottom_with_utf8_intact() {
    let term = nebula_terminal::term::test::mock_term("old\r\n中间\r\nlatest");
    let read = capture_terminal_tail(&term, 7, 3, 2, RuntimeTaskState::Finished, false, None);
    assert_eq!(read.text, "中间\nlatest");
    assert_eq!(read.requested_lines, 2);
    assert_eq!(read.returned_lines, 2);
    assert!(read.truncated);
    assert!(std::str::from_utf8(read.text.as_bytes()).is_ok());
}

#[test]
fn agents_list_projection_keeps_window_and_tab_identity() {
    let mut snapshot = snapshot(RuntimeTaskState::Attention);
    snapshot.windows[0].tabs[0].panes[0].agent = Some(RuntimeAgent {
        agent_id: None,
        generation: None,
        name: None,
        worktree: None,
        kind: "codex".into(),
        display_name: "Codex".into(),
        session_id: Some("thread-7".into()),
        state_source: RuntimeAgentStateSource::Hook,
        state_rule: None,
        hook_seen: true,
    });
    let hub = RuntimeHub::new();
    let published = hub.publish(snapshot);
    assert_eq!(published.windows[0].tabs[0].panes[0].state_change_seq, 1);
    assert_eq!(
        published.windows[0].tabs[0].panes[0].agent.as_ref().unwrap().session_id.as_deref(),
        Some("thread-7")
    );
}

#[test]
fn orchestrate_accepts_typed_backward_references() {
    let params = json!({
        "steps": [
            { "id": "right", "op": "split", "direction": "left_right" },
            {
                "id": "weather",
                "op": "agent_launch",
                "target": { "step": "right", "field": "pane_id" },
                "name": "weather",
                "kind": "claude",
                "initial_prompt": "查询天气"
            }
        ]
    });
    super::orchestrate::validate_params(&params).unwrap();
}

#[test]
fn orchestrate_rejects_unknown_fields_duplicate_ids_and_future_references() {
    let unknown = json!({
        "steps": [
            { "id": "right", "op": "split", "direction": "left_right", "method": "pane.split" }
        ]
    });
    assert_eq!(super::orchestrate::validate_params(&unknown).unwrap_err().code, "invalid_params");

    let duplicate = json!({
        "steps": [
            { "id": "same", "op": "new_tab" },
            { "id": "same", "op": "split", "direction": "top_bottom" }
        ]
    });
    assert_eq!(super::orchestrate::validate_params(&duplicate).unwrap_err().code, "invalid_params");

    let future = json!({
        "steps": [
            {
                "id": "prompt",
                "op": "prompt",
                "target": { "step": "later", "field": "pane_id" },
                "text": "hello"
            },
            { "id": "later", "op": "new_tab" }
        ]
    });
    assert_eq!(super::orchestrate::validate_params(&future).unwrap_err().code, "invalid_reference");

    let self_reference = json!({
        "steps": [{
            "id": "self",
            "op": "prompt",
            "target": { "step": "self", "field": "pane_id" },
            "text": "hello"
        }]
    });
    assert_eq!(
        super::orchestrate::validate_params(&self_reference).unwrap_err().code,
        "invalid_reference"
    );
}

#[test]
fn orchestrate_keeps_prompt_and_command_input_boundaries() {
    let multiline_prompt = json!({
        "steps": [{
            "id": "prompt",
            "op": "prompt",
            "target": { "pane_id": 3 },
            "text": "first\nsecond"
        }]
    });
    assert_eq!(
        super::orchestrate::validate_params(&multiline_prompt).unwrap_err().code,
        "invalid_params"
    );

    let escaped_command = json!({
        "steps": [{
            "id": "run",
            "op": "run",
            "target": { "pane_id": 3 },
            "command": "echo ok\u{001b}[2J"
        }]
    });
    assert_eq!(
        super::orchestrate::validate_params(&escaped_command).unwrap_err().code,
        "invalid_params"
    );
}

#[test]
fn agent_start_can_bind_an_existing_pane_but_not_replace_its_cwd() {
    let existing = ApiRequest::new(
        "token".into(),
        "agent.start",
        json!({ "window_id": 7, "pane_id": 3, "name": "worker", "kind": "codex" }),
    );
    assert!(matches!(
        RuntimeCommand::from_request(&existing),
        Ok(RuntimeCommand::AgentStart { window_id: Some(7), pane_id: Some(3), .. })
    ));

    let invalid = ApiRequest::new(
        "token".into(),
        "agent.start",
        json!({
            "window_id": 7,
            "pane_id": 3,
            "name": "worker",
            "kind": "codex",
            "cwd": "D:/other"
        }),
    );
    assert_eq!(RuntimeCommand::from_request(&invalid).unwrap_err().code, "invalid_params");
}

#[test]
fn agent_ready_requires_observed_process_identity() {
    let hub = RuntimeHub::new();
    let agent = hub
        .register_agent("worker".into(), crate::ai_agents::AgentKind::Codex, 7, 3, None, None)
        .unwrap();
    hub.publish(snapshot(RuntimeTaskState::Idle));
    let error = super::orchestrate::wait_agent_ready(
        &hub,
        &agent.agent_id,
        agent.generation,
        Instant::now() + Duration::from_millis(5),
    )
    .unwrap_err();
    assert_eq!(error.code, "agent_ready_timeout");

    let mut detected = snapshot(RuntimeTaskState::Idle);
    detected.windows[0].tabs[0].panes[0].agent = Some(detected_agent("codex", None));
    hub.publish(detected);
    let (ready, state) = super::orchestrate::wait_agent_ready(
        &hub,
        &agent.agent_id,
        agent.generation,
        Instant::now() + Duration::from_millis(50),
    )
    .unwrap();
    assert!(ready.observed);
    assert_eq!(state, RuntimeTaskState::Idle);
}

#[test]
fn orchestrate_receipt_preserves_partial_success() {
    let sink = EventSink::Callback(Arc::new(|callback| {
        let RuntimeCallback::Control(dispatch) = callback else { return };
        match &dispatch.command {
            RuntimeCommand::NewTab { .. } => dispatch.respond(Ok(json!({
                "action": { "window_id": 7, "pane_id": 9 },
                "snapshot": null
            }))),
            RuntimeCommand::Prompt { .. } => {
                dispatch.respond(Err(ApiError::new("input_in_progress", "pane is busy")))
            },
            command => panic!("unexpected command: {command:?}"),
        }
    }));
    let receipt = super::orchestrate::execute_for_test(
        &json!({
            "steps": [
                { "id": "tab", "op": "new_tab" },
                {
                    "id": "prompt",
                    "op": "prompt",
                    "target": { "step": "tab", "field": "pane_id" },
                    "text": "hello"
                }
            ]
        }),
        &sink,
        &RuntimeHub::new(),
    )
    .unwrap();
    assert_eq!(receipt["ok"], false);
    assert_eq!(receipt["partial"], true);
    assert_eq!(receipt["completed"], 1);
    assert_eq!(receipt["failed_step"], "prompt");
    assert_eq!(receipt["steps"][0]["action"]["pane_id"], 9);
    assert_eq!(receipt["steps"][1]["error"]["code"], "input_in_progress");
}

#[test]
fn orchestrate_does_not_expose_agent_receipt_before_ready() {
    let hub = RuntimeHub::new();
    hub.publish(snapshot(RuntimeTaskState::Idle));
    let prompt_dispatches = Arc::new(AtomicUsize::new(0));
    let sink_hub = hub.clone();
    let sink_prompt_dispatches = prompt_dispatches.clone();
    let sink = EventSink::Callback(Arc::new(move |callback| {
        let RuntimeCallback::Control(dispatch) = callback else {
            return;
        };
        match &dispatch.command {
            RuntimeCommand::AgentStart { pane_id: Some(pane_id), name, kind, .. } => {
                let agent =
                    sink_hub.register_agent(name.clone(), *kind, 7, *pane_id, None, None).unwrap();
                dispatch.respond(Ok(json!({
                    "action": { "agent": agent, "window_id": 7, "pane_id": pane_id },
                    "snapshot": null
                })));
            },
            RuntimeCommand::Prompt { .. } | RuntimeCommand::AgentPrompt { .. } => {
                sink_prompt_dispatches.fetch_add(1, Ordering::Relaxed);
                dispatch.respond(Ok(json!({ "action": {} })));
            },
            // 就绪超时同样会捎回屏幕现场，因此这个 harness 也要能应答读取。
            RuntimeCommand::ReadPane { .. } => {
                dispatch.respond(Ok(json!({ "action": { "text": "", "returned_lines": 0 } })));
            },
            command => panic!("unexpected command: {command:?}"),
        }
    }));
    let receipt = super::orchestrate::execute_for_test(
        &json!({
            "steps": [
                {
                    "id": "agent",
                    "op": "agent_launch",
                    "target": { "window_id": 7, "pane_id": 3 },
                    "name": "worker",
                    "kind": "codex",
                    "initial_prompt": "first task",
                    "ready_timeout_ms": 5
                },
                {
                    "id": "too_early",
                    "op": "prompt",
                    "target": { "step": "agent", "field": "pane_id" },
                    "text": "must not dispatch"
                }
            ]
        }),
        &sink,
        &hub,
    )
    .unwrap();
    assert_eq!(prompt_dispatches.load(Ordering::Relaxed), 0);
    assert_eq!(receipt["failed_step"], "agent");
    assert_eq!(receipt["steps"].as_array().unwrap().len(), 1);
    assert_eq!(receipt["steps"][0]["error"]["code"], "agent_ready_timeout");
}

/// 停在需要作答的画面上的 Agent 不是就绪的 Agent。此前判据只排除 Running，于是
/// 登录页、更新确认页都算就绪，initial_prompt 直接打进那个弹窗。
#[test]
fn agent_stopped_on_a_blocking_screen_never_receives_its_initial_prompt() {
    for blocked in [RuntimeTaskState::Attention, RuntimeTaskState::WaitingInput] {
        let hub = RuntimeHub::new();
        hub.publish(snapshot(RuntimeTaskState::Idle));
        let prompt_dispatches = Arc::new(AtomicUsize::new(0));
        let sink_hub = hub.clone();
        let sink_prompt_dispatches = prompt_dispatches.clone();
        let sink = EventSink::Callback(Arc::new(move |callback| {
            let RuntimeCallback::Control(dispatch) = callback else {
                return;
            };
            match &dispatch.command {
                RuntimeCommand::AgentStart { pane_id: Some(pane_id), name, kind, .. } => {
                    let agent = sink_hub
                        .register_agent(name.clone(), *kind, 7, *pane_id, None, None)
                        .unwrap();
                    // 进程身份要等下一次 publish 才被观察到，顺序与真实启动一致：
                    // 先登记，再由快照确认这个 pane 上确实跑着该 Agent。
                    let mut stuck = snapshot(blocked);
                    stuck.windows[0].tabs[0].panes[0].agent = Some(detected_agent("codex", None));
                    sink_hub.publish(stuck);
                    dispatch.respond(Ok(json!({
                        "action": { "agent": agent, "window_id": 7, "pane_id": pane_id },
                        "snapshot": null
                    })));
                },
                RuntimeCommand::ReadPane { .. } => {
                    dispatch.respond(Ok(json!({
                        "action": { "text": "Sign in to continue\n> ", "returned_lines": 2 }
                    })));
                },
                RuntimeCommand::Prompt { .. } | RuntimeCommand::AgentPrompt { .. } => {
                    sink_prompt_dispatches.fetch_add(1, Ordering::Relaxed);
                    dispatch.respond(Ok(json!({ "action": {} })));
                },
                command => panic!("unexpected command: {command:?}"),
            }
        }));
        let receipt = super::orchestrate::execute_for_test(
            &json!({
                "steps": [{
                    "id": "agent",
                    "op": "agent_launch",
                    "target": { "window_id": 7, "pane_id": 3 },
                    "name": "worker",
                    "kind": "codex",
                    "initial_prompt": "review the diff",
                    "ready_timeout_ms": 5_000
                }]
            }),
            &sink,
            &hub,
        )
        .unwrap();
        assert_eq!(prompt_dispatches.load(Ordering::Relaxed), 0, "{blocked:?} must not be prompted");
        let error = &receipt["steps"][0]["error"];
        assert_eq!(error["code"], "agent_not_ready");
        assert_eq!(error["details"]["task_state"], serde_json::to_value(blocked).unwrap());
        assert_eq!(error["details"]["submitted"], false);
        // 现场必须随错误一起回来：调用方判断"登录页还是网络挂住"只能靠这个。
        assert_eq!(error["details"]["tail"]["text"], "Sign in to continue\n> ");
    }
}

/// 提交之后既没有 CommandStart、屏幕也不在产出，就不该烧完整个超时。反过来只要
/// 还在产出就继续等——那可能只是这个 shell 缺少 OSC 133 集成。
#[test]
fn run_reports_a_swallowed_submission_without_burning_the_whole_timeout() {
    let hub = RuntimeHub::new();
    let mut submitted = snapshot(RuntimeTaskState::Running);
    submitted.windows[0].tabs[0].panes[0].active_run =
        Some(RuntimePaneRun { run_id: 11, phase: RuntimeRunPhase::Submitted });
    hub.publish(submitted);

    let quiet = EventSink::Callback(Arc::new(|callback| {
        let RuntimeCallback::Control(dispatch) = callback else {
            return;
        };
        match &dispatch.command {
            RuntimeCommand::ReadPane { .. } => dispatch.respond(Ok(json!({
                "action": { "text": "Update available. Press Enter to install.", "returned_lines": 1 }
            }))),
            command => panic!("unexpected command: {command:?}"),
        }
    }));
    let started = Instant::now();
    let error = super::wait_run_phased_with_grace(
        &hub,
        &quiet,
        7,
        3,
        11,
        Duration::from_secs(30),
        Duration::from_millis(50),
    )
    .unwrap_err();
    assert_eq!(error.code, "run_not_started");
    let details = error.details.unwrap();
    assert_eq!(details["tail"], "Update available. Press Enter to install.");
    // 静默的慢命令在缺少 133 集成的 shell 里与被挡住无法区分，因此只报证据不定性。
    assert_eq!(details["shell_integration"], "unconfirmed");
    assert!(started.elapsed() < Duration::from_secs(5), "must not burn the caller's timeout");

    // 屏幕在产出时不许打断：两次取样不同即视为仍在推进，等待照原超时继续。
    let churn = Arc::new(AtomicUsize::new(0));
    let sink_churn = churn.clone();
    let noisy = EventSink::Callback(Arc::new(move |callback| {
        let RuntimeCallback::Control(dispatch) = callback else {
            return;
        };
        match &dispatch.command {
            RuntimeCommand::ReadPane { .. } => {
                let seen = sink_churn.fetch_add(1, Ordering::Relaxed);
                dispatch.respond(Ok(json!({
                    "action": { "text": format!("compiling unit {seen}"), "returned_lines": 1 }
                })));
            },
            command => panic!("unexpected command: {command:?}"),
        }
    }));
    let error = super::wait_run_phased_with_grace(
        &hub,
        &noisy,
        7,
        3,
        11,
        Duration::from_millis(300),
        Duration::from_millis(50),
    )
    .unwrap_err();
    assert_eq!(error.code, "run_start_timeout", "progress must keep the original wait alive");
}

/// "跑命令 + 看输出"必须是一次请求。此前 run 回执只有退出码没有输出，调用方还得
/// 再发一轮 pane.read 才知道失败原因。
#[test]
fn run_step_brings_back_its_own_output_in_one_request() {
    let hub = RuntimeHub::new();
    // 预置这条 run 的完成结果：命令已经跑完并拿到退出码，本测试要验的是回执里
    // 是否同时带回了输出，而不是等待机制本身。
    let mut running = snapshot(RuntimeTaskState::Running);
    running.windows[0].tabs[0].panes[0].active_run =
        Some(RuntimePaneRun { run_id: 77, phase: RuntimeRunPhase::Started });
    hub.publish(running);
    let mut done = snapshot(RuntimeTaskState::Finished);
    done.windows[0].tabs[0].panes[0].last_run = Some(RuntimeRunOutcome::command_done(
        RuntimePaneRun { run_id: 77, phase: RuntimeRunPhase::Started },
        Some(101),
    ));
    hub.publish(done);

    let sink = EventSink::Callback(Arc::new(|callback| {
        let RuntimeCallback::Control(dispatch) = callback else {
            return;
        };
        match &dispatch.command {
            RuntimeCommand::Run { window_id, pane_id, .. } => dispatch.respond(Ok(json!({
                "action": { "window_id": window_id, "pane_id": pane_id, "run_id": 77 },
                "snapshot": null
            }))),
            RuntimeCommand::ReadPane { lines, .. } => dispatch.respond(Ok(json!({
                "action": {
                    "text": "test result: FAILED. 1 failed",
                    "returned_lines": lines,
                    "truncated": false
                }
            }))),
            command => panic!("unexpected command: {command:?}"),
        }
    }));
    let receipt = super::orchestrate::execute_for_test(
        &json!({
            "steps": [{
                "id": "tests",
                "op": "run",
                "target": { "window_id": 7, "pane_id": 3 },
                "command": "cargo test",
                "wait": false,
                "tail_lines": 20
            }]
        }),
        &sink,
        &hub,
    );
    // 不等待就没有"命令结束"这一刻，读到的只会是提交瞬间的画面。
    assert_eq!(receipt.unwrap_err().code, "invalid_params");

    let receipt = super::orchestrate::execute_for_test(
        &json!({
            "steps": [{
                "id": "tests",
                "op": "run",
                "target": { "window_id": 7, "pane_id": 3 },
                "command": "cargo test",
                "tail_lines": 20
            }]
        }),
        &sink,
        &hub,
    )
    .unwrap();
    assert_eq!(receipt["ok"], true);
    assert_eq!(receipt["steps"].as_array().unwrap().len(), 1);
    // 退出码和输出在同一份回执里，不需要第二轮 pane.read。
    assert_eq!(receipt["steps"][0]["action"]["exit_code"], 101);
    assert_eq!(receipt["steps"][0]["action"]["tail"]["text"], "test result: FAILED. 1 failed");
    // 观察窗口开 20 行，实际只有一行有内容——回执报的是真正读回来的行数。
    assert_eq!(receipt["steps"][0]["action"]["tail"]["returned_lines"], 1);
    assert_eq!(receipt["steps"][0]["action"]["tail"]["requested_lines"], 20);
}

/// 动态输出共享一份字节预算：观察窗口(tail_lines)开得大不等于允许把调用方的
/// 上下文吃光，而截断必须是可见的。
#[test]
fn receipt_tail_budget_truncates_from_the_end_and_says_so() {
    let mut object = serde_json::Map::new();
    object.insert("text".to_owned(), Value::String("汉字abc".repeat(400)));
    let mut tail = Value::Object(object);
    let original_bytes = tail["text"].as_str().unwrap().len();
    super::orchestrate::truncate_tail_for_test(&mut tail, 64);

    let kept = tail["text"].as_str().unwrap();
    assert!(kept.len() <= 64, "budget must hold, got {}", kept.len());
    assert!(original_bytes > kept.len(), "this fixture must actually exceed the budget");
    // 结论和报错都在输出末尾，所以保留的是尾巴。
    assert!("汉字abc".repeat(400).ends_with(kept), "must keep the trailing bytes");
    assert_eq!(tail["truncated"], true);
    assert_eq!(tail["original_bytes"], original_bytes);
    // 多字节字符不能被切成半个。
    assert!(std::str::from_utf8(kept.as_bytes()).is_ok());
}

/// "发 prompt 然后等它干完"必须真的等到新变化。基线缺失时 settled 会立刻命中
/// 提交之前那个还没动的空闲态，等待形同虚设——这是最容易悄悄退化的一处。
#[test]
fn wait_step_takes_its_baseline_from_the_step_it_references() {
    let hub = RuntimeHub::new();
    hub.publish(snapshot(RuntimeTaskState::Idle));
    let baseline = hub
        .current()
        .unwrap()
        .windows
        .into_iter()
        .flat_map(|window| window.tabs)
        .flat_map(|tab| tab.panes)
        .find(|pane| pane.id == 3)
        .unwrap()
        .state_change_seq;

    let sink_hub = hub.clone();
    let sink = EventSink::Callback(Arc::new(move |callback| {
        let RuntimeCallback::Control(dispatch) = callback else {
            return;
        };
        match &dispatch.command {
            RuntimeCommand::Prompt { window_id, pane_id, .. } => dispatch.respond(Ok(json!({
                "action": { "window_id": window_id, "pane_id": pane_id },
                "snapshot": sink_hub.current()
            }))),
            RuntimeCommand::ReadPane { .. } => dispatch
                .respond(Ok(json!({ "action": { "text": "› ", "returned_lines": 1 } }))),
            command => panic!("unexpected command: {command:?}"),
        }
    }));
    let receipt = super::orchestrate::execute_for_test(
        &json!({
            "steps": [
                {
                    "id": "ask",
                    "op": "prompt",
                    "target": { "window_id": 7, "pane_id": 3 },
                    "text": "review the diff"
                },
                {
                    "id": "settle",
                    "op": "wait",
                    "target": { "step": "ask", "field": "pane_id" },
                    "state": "settled",
                    "timeout_ms": 150,
                    "tail_lines": 5
                }
            ]
        }),
        &sink,
        &hub,
    )
    .unwrap();

    // prompt 步必须把提交那一刻的序号留在回执里，否则下游无从取基线。
    assert_eq!(receipt["steps"][0]["action"]["state_change_seq"], baseline);
    // pane 仍是 Idle（本身满足 settled），但序号没往前走，所以不算等到了新变化。
    assert_eq!(receipt["steps"][1]["ok"], false);
    assert_eq!(receipt["steps"][1]["error"]["code"], "timeout");
    assert_eq!(receipt["steps"][1]["error"]["details"]["after_seq"], baseline);
    // 等不到同样要给现场。
    assert_eq!(receipt["steps"][1]["error"]["details"]["tail"]["text"], "› ");
}
