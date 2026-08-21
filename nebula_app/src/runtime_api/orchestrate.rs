//! 强类型单请求编排：模型只提交一次意图，Runtime 在本地完成确定性控制步骤。

use std::collections::{HashMap, HashSet};

use super::*;

const MAX_ORCHESTRATE_STEPS: usize = 32;
const MAX_ORCHESTRATE_BYTES: usize = 64 * 1024;
const DEFAULT_READY_TIMEOUT_MS: u64 = 10_000;

static NEXT_WORKFLOW_ID: AtomicU64 = AtomicU64::new(1);

#[derive(Debug, Clone, Copy, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
enum OnError {
    Stop,
    Continue,
}

impl Default for OnError {
    fn default() -> Self {
        Self::Stop
    }
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct OrchestrateParams {
    steps: Vec<OrchestrateStep>,
    #[serde(default)]
    on_error: OnError,
}

#[derive(Debug, Deserialize)]
#[serde(tag = "op", rename_all = "snake_case", deny_unknown_fields)]
enum OrchestrateStep {
    NewTab {
        id: String,
        #[serde(default)]
        window_id: Option<u64>,
        #[serde(default)]
        cwd: Option<PathBuf>,
    },
    Focus {
        id: String,
        target: PaneTarget,
    },
    Split {
        id: String,
        #[serde(default)]
        window_id: Option<u64>,
        #[serde(default)]
        target: Option<PaneTarget>,
        direction: RuntimeSplitDirection,
    },
    Prompt {
        id: String,
        target: PaneTarget,
        text: String,
        #[serde(default = "default_true")]
        submit: bool,
    },
    Run {
        id: String,
        target: PaneTarget,
        command: String,
        #[serde(default = "default_true")]
        wait: bool,
        #[serde(default = "default_command_timeout_ms")]
        timeout_ms: u64,
    },
    AgentLaunch {
        id: String,
        target: PaneTarget,
        name: String,
        kind: String,
        #[serde(default)]
        resume_session_id: Option<String>,
        initial_prompt: String,
        #[serde(default = "default_ready_timeout_ms")]
        ready_timeout_ms: u64,
    },
}

impl OrchestrateStep {
    fn id(&self) -> &str {
        match self {
            Self::NewTab { id, .. }
            | Self::Focus { id, .. }
            | Self::Split { id, .. }
            | Self::Prompt { id, .. }
            | Self::Run { id, .. }
            | Self::AgentLaunch { id, .. } => id,
        }
    }

    fn op(&self) -> &'static str {
        match self {
            Self::NewTab { .. } => "new_tab",
            Self::Focus { .. } => "focus",
            Self::Split { .. } => "split",
            Self::Prompt { .. } => "prompt",
            Self::Run { .. } => "run",
            Self::AgentLaunch { .. } => "agent_launch",
        }
    }

    fn target(&self) -> Option<&PaneTarget> {
        match self {
            Self::Focus { target, .. }
            | Self::Prompt { target, .. }
            | Self::Run { target, .. }
            | Self::AgentLaunch { target, .. } => Some(target),
            Self::Split { target, .. } => target.as_ref(),
            Self::NewTab { .. } => None,
        }
    }

    fn referenced_step(&self) -> Option<&str> {
        match self.target() {
            Some(PaneTarget::Reference(reference)) => Some(&reference.step),
            Some(PaneTarget::Existing(_)) | None => None,
        }
    }
}

#[derive(Debug, Deserialize)]
#[serde(untagged)]
enum PaneTarget {
    Reference(StepReference),
    Existing(ExistingPane),
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct StepReference {
    step: String,
    field: ReferenceField,
}

#[derive(Debug, Clone, Copy, Deserialize)]
#[serde(rename_all = "snake_case")]
enum ReferenceField {
    PaneId,
}

impl ReferenceField {
    fn as_str(self) -> &'static str {
        match self {
            Self::PaneId => "pane_id",
        }
    }
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct ExistingPane {
    pane_id: u64,
    #[serde(default)]
    window_id: Option<u64>,
}

#[derive(Debug, Serialize)]
struct WorkflowReceipt {
    workflow_id: String,
    ok: bool,
    duration_ms: u64,
    partial: bool,
    completed: usize,
    failed_step: Option<String>,
    steps: Vec<StepReceipt>,
}

#[derive(Debug, Serialize)]
struct StepReceipt {
    id: String,
    op: &'static str,
    ok: bool,
    duration_ms: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    action: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    error: Option<ApiError>,
}

struct PendingAgentLaunch {
    receipt_index: usize,
    step_id: String,
    started_at: Instant,
    deadline: Instant,
    agent_id: String,
    generation: u64,
    initial_prompt: String,
    action: Value,
}

struct AgentFinalization {
    receipt_index: usize,
    step_id: String,
    duration_ms: u64,
    outcome: Result<Value, ApiError>,
}

pub(super) fn orchestrate_connection(
    stream: &mut TcpStream,
    request: ApiRequest,
    sink: &EventSink,
    hub: &RuntimeHub,
) -> Result<(), IoError> {
    let params = match parse_and_validate(&request.params) {
        Ok(params) => params,
        Err(error) => return write_response(stream, &ApiResponse::failure(request.id, error)),
    };
    info!(
        "runtime orchestrate request_id={} steps={} on_error={:?}",
        request.id,
        params.steps.len(),
        params.on_error
    );
    let receipt = execute(params, sink, hub);
    let result = serde_json::to_value(receipt).map_err(IoError::other)?;
    write_response(stream, &ApiResponse::success(request.id, result))
}

pub(super) fn validate_params(value: &Value) -> Result<(), ApiError> {
    parse_and_validate(value).map(|_| ())
}

#[cfg(test)]
pub(super) fn execute_for_test(
    value: &Value,
    sink: &EventSink,
    hub: &RuntimeHub,
) -> Result<Value, ApiError> {
    let params = parse_and_validate(value)?;
    serde_json::to_value(execute(params, sink, hub))
        .map_err(|error| ApiError::new("serialization_failed", error.to_string()))
}

fn parse_and_validate(value: &Value) -> Result<OrchestrateParams, ApiError> {
    let encoded = serde_json::to_vec(value)
        .map_err(|error| ApiError::invalid_params(format!("invalid orchestration: {error}")))?;
    if encoded.len() > MAX_ORCHESTRATE_BYTES {
        return Err(ApiError::invalid_params(format!(
            "runtime.orchestrate params exceed the {MAX_ORCHESTRATE_BYTES}-byte limit"
        )));
    }
    let params: OrchestrateParams = parse_params(value)?;
    if params.steps.is_empty() || params.steps.len() > MAX_ORCHESTRATE_STEPS {
        return Err(ApiError::invalid_params(format!(
            "steps must contain between 1 and {MAX_ORCHESTRATE_STEPS} entries"
        )));
    }

    let mut seen = HashSet::new();
    for step in &params.steps {
        validate_step_id(step.id())?;
        if seen.contains(step.id()) {
            return Err(ApiError::invalid_params(format!(
                "duplicate orchestration step id {:?}",
                step.id()
            )));
        }
        if let Some(PaneTarget::Reference(reference)) = step.target()
            && !seen.contains(&reference.step)
        {
            return Err(invalid_reference(
                step.id(),
                &reference.step,
                "references must point to an earlier successful step",
            ));
        }
        validate_step_contract(step)?;
        seen.insert(step.id().to_owned());
    }
    Ok(params)
}

fn validate_step_id(id: &str) -> Result<(), ApiError> {
    let valid = !id.is_empty()
        && id.len() <= 64
        && id.bytes().enumerate().all(|(index, byte)| {
            if index == 0 {
                byte == b'_' || byte.is_ascii_alphabetic()
            } else {
                byte == b'_' || byte == b'-' || byte.is_ascii_alphanumeric()
            }
        });
    if valid {
        Ok(())
    } else {
        Err(ApiError::invalid_params(
            "step id must be 1-64 ASCII letters, digits, '_' or '-', and start with a letter or '_'",
        ))
    }
}

fn validate_step_contract(step: &OrchestrateStep) -> Result<(), ApiError> {
    match step {
        OrchestrateStep::Split { window_id: Some(_), target: Some(_), .. } => {
            Err(ApiError::invalid_params(
                "a split step cannot combine window_id with target; put window_id inside a direct target",
            ))
        },
        OrchestrateStep::Prompt { text, .. } => validate_prompt(text),
        OrchestrateStep::Run { command, timeout_ms, .. } => {
            validate_command_line(command)?;
            validate_timeout(*timeout_ms, "run timeout_ms")
        },
        OrchestrateStep::AgentLaunch {
            name,
            kind,
            resume_session_id,
            initial_prompt,
            ready_timeout_ms,
            ..
        } => {
            validate_prompt(initial_prompt)?;
            validate_timeout(*ready_timeout_ms, "ready_timeout_ms")?;
            agent_start_command(None, 1, name.clone(), kind.clone(), resume_session_id.clone())?;
            Ok(())
        },
        OrchestrateStep::NewTab { .. }
        | OrchestrateStep::Focus { .. }
        | OrchestrateStep::Split { .. } => Ok(()),
    }
}

fn validate_timeout(timeout_ms: u64, label: &str) -> Result<(), ApiError> {
    if timeout_ms == 0 || Duration::from_millis(timeout_ms) > MAX_WAIT {
        Err(ApiError::invalid_params(format!("{label} must be between 1 and 86400000")))
    } else {
        Ok(())
    }
}

fn execute(params: OrchestrateParams, sink: &EventSink, hub: &RuntimeHub) -> WorkflowReceipt {
    let workflow_started = Instant::now();
    let workflow_sequence = NEXT_WORKFLOW_ID.fetch_add(1, Ordering::Relaxed);
    let workflow_id = format!("workflow-{}-{workflow_sequence}", std::process::id());
    let mut actions = HashMap::<String, Value>::new();
    let mut receipts = Vec::with_capacity(params.steps.len());
    let mut pending_agents = Vec::new();

    for step in &params.steps {
        if let Some(referenced_step) = step.referenced_step()
            && let Some(index) = pending_agents
                .iter()
                .position(|pending: &PendingAgentLaunch| pending.step_id == referenced_step)
        {
            let finalization = finalize_agent(pending_agents.remove(index), sink, hub);
            let dependency_failed = finalization.outcome.is_err();
            apply_agent_finalization(finalization, &mut receipts, &mut actions);
            if dependency_failed && params.on_error == OnError::Stop {
                break;
            }
        }
        let started_at = Instant::now();
        let outcome = execute_step(step, sink, hub, &actions);
        match outcome {
            Ok(StepOutcome::Complete(action)) => {
                actions.insert(step.id().to_owned(), action.clone());
                receipts.push(StepReceipt {
                    id: step.id().to_owned(),
                    op: step.op(),
                    ok: true,
                    duration_ms: elapsed_ms(started_at),
                    action: Some(action),
                    error: None,
                });
            },
            Ok(StepOutcome::AgentPending(mut pending)) => {
                let action = pending.action.clone();
                pending.receipt_index = receipts.len();
                receipts.push(StepReceipt {
                    id: step.id().to_owned(),
                    op: step.op(),
                    ok: true,
                    duration_ms: elapsed_ms(started_at),
                    action: Some(action),
                    error: None,
                });
                pending_agents.push(pending);
            },
            Err(error) => {
                receipts.push(StepReceipt {
                    id: step.id().to_owned(),
                    op: step.op(),
                    ok: false,
                    duration_ms: elapsed_ms(started_at),
                    action: None,
                    error: Some(error),
                });
                if params.on_error == OnError::Stop {
                    break;
                }
            },
        }
    }

    // 所有 Agent 先启动，再并行等待 ready；这样多个 CLI 的冷启动互相重叠，
    // 而主线程仍然只收到彼此独立的单步 prompt dispatch。
    let finalized = finalize_agents(pending_agents, sink, hub);
    for finalization in finalized {
        apply_agent_finalization(finalization, &mut receipts, &mut actions);
    }

    let completed = receipts.iter().filter(|receipt| receipt.ok).count();
    let failed_step = receipts.iter().find(|receipt| !receipt.ok).map(|receipt| receipt.id.clone());
    let ok = failed_step.is_none();
    WorkflowReceipt {
        workflow_id,
        ok,
        duration_ms: elapsed_ms(workflow_started),
        partial: !ok && completed > 0,
        completed,
        failed_step,
        steps: receipts,
    }
}

fn apply_agent_finalization(
    finalization: AgentFinalization,
    receipts: &mut [StepReceipt],
    actions: &mut HashMap<String, Value>,
) {
    let Some(receipt) = receipts.get_mut(finalization.receipt_index) else {
        return;
    };
    receipt.duration_ms = finalization.duration_ms;
    match finalization.outcome {
        Ok(action) => {
            receipt.action = Some(action.clone());
            actions.insert(finalization.step_id, action);
        },
        Err(error) => {
            actions.remove(&finalization.step_id);
            receipt.ok = false;
            receipt.error = Some(error);
        },
    }
}

enum StepOutcome {
    Complete(Value),
    AgentPending(PendingAgentLaunch),
}

fn execute_step(
    step: &OrchestrateStep,
    sink: &EventSink,
    hub: &RuntimeHub,
    actions: &HashMap<String, Value>,
) -> Result<StepOutcome, ApiError> {
    if let OrchestrateStep::AgentLaunch {
        id,
        target,
        name,
        kind,
        resume_session_id,
        initial_prompt,
        ready_timeout_ms,
    } = step
    {
        let (window_id, pane_id) = resolve_target(id, target, actions)?;
        let command = agent_start_command(
            window_id,
            pane_id,
            name.clone(),
            kind.clone(),
            resume_session_id.clone(),
        )?;
        let started_at = Instant::now();
        let result = dispatch_runtime_command(command, sink, hub)?;
        let action = essential_action(&result);
        let agent_id = action
            .get("agent_id")
            .and_then(Value::as_str)
            .ok_or_else(|| invalid_runtime_response("agent.start did not return agent_id"))?
            .to_owned();
        let generation = action
            .get("generation")
            .and_then(Value::as_u64)
            .ok_or_else(|| invalid_runtime_response("agent.start did not return generation"))?;
        return Ok(StepOutcome::AgentPending(PendingAgentLaunch {
            receipt_index: 0,
            step_id: id.clone(),
            started_at,
            deadline: started_at + Duration::from_millis(*ready_timeout_ms),
            agent_id,
            generation,
            initial_prompt: initial_prompt.clone(),
            action,
        }));
    }

    let command = match step {
        OrchestrateStep::NewTab { window_id, cwd, .. } => {
            RuntimeCommand::NewTab { window_id: *window_id, cwd: cwd.clone() }
        },
        OrchestrateStep::Focus { id, target } => {
            let (window_id, pane_id) = resolve_target(id, target, actions)?;
            RuntimeCommand::Focus { window_id, pane_id: Some(pane_id) }
        },
        OrchestrateStep::Split { id, window_id, target, direction } => {
            let (window_id, pane_id) = match target {
                Some(target) => {
                    let (window_id, pane_id) = resolve_target(id, target, actions)?;
                    (window_id, Some(pane_id))
                },
                None => (*window_id, None),
            };
            RuntimeCommand::Split { window_id, pane_id, direction: *direction }
        },
        OrchestrateStep::Prompt { id, target, text, submit } => {
            let (window_id, pane_id) = resolve_target(id, target, actions)?;
            RuntimeCommand::Prompt { window_id, pane_id, text: text.clone(), submit: *submit }
        },
        OrchestrateStep::Run { id, target, command, wait, timeout_ms } => {
            let (window_id, pane_id) = resolve_target(id, target, actions)?;
            RuntimeCommand::Run {
                window_id,
                pane_id,
                command: command.clone(),
                wait: *wait,
                timeout_ms: *timeout_ms,
            }
        },
        OrchestrateStep::AgentLaunch { .. } => unreachable!("handled above"),
    };
    let result = dispatch_runtime_command(command, sink, hub)?;
    Ok(StepOutcome::Complete(essential_action(&result)))
}

fn resolve_target(
    step_id: &str,
    target: &PaneTarget,
    actions: &HashMap<String, Value>,
) -> Result<(Option<u64>, u64), ApiError> {
    match target {
        PaneTarget::Existing(existing) => Ok((existing.window_id, existing.pane_id)),
        PaneTarget::Reference(reference) => {
            let action = actions.get(&reference.step).ok_or_else(|| {
                invalid_reference(
                    step_id,
                    &reference.step,
                    "the referenced step did not complete successfully",
                )
            })?;
            let field = reference.field.as_str();
            let pane_id = action.get(field).and_then(Value::as_u64).ok_or_else(|| {
                invalid_reference(
                    step_id,
                    &reference.step,
                    &format!("the referenced receipt has no integer {field}"),
                )
            })?;
            Ok((action.get("window_id").and_then(Value::as_u64), pane_id))
        },
    }
}

fn agent_start_command(
    window_id: Option<u64>,
    pane_id: u64,
    name: String,
    kind: String,
    resume_session_id: Option<String>,
) -> Result<RuntimeCommand, ApiError> {
    let request = ApiRequest::new(
        String::new(),
        "agent.start",
        json!({
            "window_id": window_id,
            "pane_id": pane_id,
            "name": name,
            "kind": kind,
            "resume_session_id": resume_session_id
        }),
    );
    RuntimeCommand::from_request(&request)
}

fn essential_action(result: &Value) -> Value {
    let source = result.get("action").or_else(|| result.get("run")).unwrap_or(result);
    let Some(agent) = source.get("agent") else { return source.clone() };
    json!({
        "window_id": source.get("window_id"),
        "pane_id": source.get("pane_id"),
        "agent_id": agent.get("agent_id"),
        "generation": agent.get("generation"),
        "name": agent.get("name"),
        "kind": agent.get("kind"),
        "session_id": agent.get("session_id")
    })
}

fn finalize_agents(
    pending: Vec<PendingAgentLaunch>,
    sink: &EventSink,
    hub: &RuntimeHub,
) -> Vec<AgentFinalization> {
    std::thread::scope(|scope| {
        let handles: Vec<_> = pending
            .into_iter()
            .map(|pending| {
                let receipt_index = pending.receipt_index;
                let step_id = pending.step_id.clone();
                let sink = sink.clone();
                let hub = hub.clone();
                let handle = scope.spawn(move || finalize_agent(pending, &sink, &hub));
                (receipt_index, step_id, handle)
            })
            .collect();
        handles
            .into_iter()
            .map(|(receipt_index, step_id, handle)| match handle.join() {
                Ok(finalization) => finalization,
                Err(_) => AgentFinalization {
                    receipt_index,
                    step_id,
                    duration_ms: 0,
                    outcome: Err(ApiError::new(
                        "orchestration_failed",
                        "agent ready worker panicked",
                    )),
                },
            })
            .collect()
    })
}

fn finalize_agent(
    pending: PendingAgentLaunch,
    sink: &EventSink,
    hub: &RuntimeHub,
) -> AgentFinalization {
    let outcome = wait_agent_ready(hub, &pending.agent_id, pending.generation, pending.deadline)
        .and_then(|(agent, ready_state)| {
            dispatch_runtime_command(
                RuntimeCommand::AgentPrompt {
                    agent: agent.agent_id,
                    generation: Some(agent.generation),
                    text: pending.initial_prompt.clone(),
                    submit: true,
                },
                sink,
                hub,
            )?;
            let mut action = pending.action.as_object().cloned().unwrap_or_default();
            action.insert(
                "ready_state".to_owned(),
                serde_json::to_value(ready_state).unwrap_or(Value::Null),
            );
            action.insert("submitted".to_owned(), Value::Bool(true));
            Ok(Value::Object(action))
        });
    AgentFinalization {
        receipt_index: pending.receipt_index,
        step_id: pending.step_id,
        duration_ms: elapsed_ms(pending.started_at),
        outcome,
    }
}

pub(super) fn wait_agent_ready(
    hub: &RuntimeHub,
    agent_id: &str,
    generation: u64,
    deadline: Instant,
) -> Result<(RuntimeManagedAgent, RuntimeTaskState), ApiError> {
    let (_, current, receiver) = hub.subscribe();
    let mut snapshot = current;
    let mut observed_agent = false;
    let mut observed_state = None;
    loop {
        let agent = hub.active_agent(agent_id, Some(generation))?;
        observed_agent |= agent.observed;
        if let Some(current) = snapshot.take()
            && let Ok(pane) = current.pane(Some(agent.window_id), agent.pane_id)
        {
            observed_state = Some(pane.task_state);
            if agent.observed && pane.task_state != RuntimeTaskState::Running {
                return Ok((agent, pane.task_state));
            }
        }

        let remaining = deadline.saturating_duration_since(Instant::now());
        if remaining.is_zero() {
            return Err(ApiError::new(
                "agent_ready_timeout",
                "the launched agent was not ready for its initial prompt before timeout",
            )
            .details(json!({
                "agent_id": agent_id,
                "generation": generation,
                "observed": observed_agent,
                "task_state": observed_state
            })));
        }
        snapshot = match receiver.recv_timeout(remaining) {
            Ok(snapshot) => Some(snapshot),
            Err(RecvTimeoutError::Timeout) => None,
            Err(RecvTimeoutError::Disconnected) => {
                return Err(ApiError::new(
                    "runtime_unavailable",
                    "runtime subscription disconnected while waiting for agent readiness",
                ));
            },
        };
    }
}

fn invalid_reference(step: &str, referenced: &str, message: &str) -> ApiError {
    ApiError::new("invalid_reference", message)
        .details(json!({ "step": step, "referenced_step": referenced }))
}

fn invalid_runtime_response(message: &str) -> ApiError {
    ApiError::new("invalid_runtime_response", message)
}

fn default_ready_timeout_ms() -> u64 {
    DEFAULT_READY_TIMEOUT_MS
}

fn default_command_timeout_ms() -> u64 {
    COMMAND_TIMEOUT.as_millis() as u64
}

fn elapsed_ms(started_at: Instant) -> u64 {
    u64::try_from(started_at.elapsed().as_millis()).unwrap_or(u64::MAX)
}
