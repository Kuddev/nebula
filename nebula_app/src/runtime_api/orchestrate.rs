//! 强类型单请求编排：模型只提交一次意图，Runtime 在本地完成确定性控制步骤。

use std::collections::{HashMap, HashSet};

use super::*;

const MAX_ORCHESTRATE_STEPS: usize = 32;
const MAX_ORCHESTRATE_BYTES: usize = 64 * 1024;
const DEFAULT_READY_TIMEOUT_MS: u64 = 10_000;
/// 步骤未能按预期结束时自动附带的屏幕行数。阻拦类型是开放集，Runtime 不猜
/// "是什么挡住了"，只保证把现场交给能读懂任意画面的一方。
const EVIDENCE_TAIL_LINES: usize = 40;
/// 整份回执里所有动态屏幕内容共享的 UTF-8 字节预算。`tail_lines` 只决定观察
/// 窗口，真正的上下文上限在这里——否则 32 步各带几百行就能吃光调用方的窗口。
const MAX_RECEIPT_TAIL_BYTES: usize = 4 * 1024;

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
        /// 命令结束后顺手带回的屏幕尾部行数。省掉"跑完再单独读一轮"的往返；
        /// 它只决定观察窗口，真正的上限是整份回执共享的字节预算。
        #[serde(default)]
        tail_lines: Option<usize>,
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
    /// 等目标 pane 到达某个语义状态。基线(after_seq)自动取自被引用步骤的回执，
    /// 因此"发 prompt 再等 settled"不会命中提交之前那个旧的空闲态。
    Wait {
        id: String,
        target: PaneTarget,
        state: RuntimeWaitState,
        #[serde(default = "default_command_timeout_ms")]
        timeout_ms: u64,
        #[serde(default)]
        tail_lines: Option<usize>,
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
            | Self::AgentLaunch { id, .. }
            | Self::Wait { id, .. } => id,
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
            Self::Wait { .. } => "wait",
        }
    }

    fn target(&self) -> Option<&PaneTarget> {
        match self {
            Self::Focus { target, .. }
            | Self::Prompt { target, .. }
            | Self::Run { target, .. }
            | Self::AgentLaunch { target, .. }
            | Self::Wait { target, .. } => Some(target),
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

#[cfg(test)]
pub(super) fn truncate_tail_for_test(tail: &mut Value, budget: usize) {
    truncate_tail(tail, budget);
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
        OrchestrateStep::Run { command, timeout_ms, tail_lines, wait, .. } => {
            validate_command_line(command)?;
            validate_timeout(*timeout_ms, "run timeout_ms")?;
            match tail_lines {
                // 不等待就没有"命令结束"这个时刻，此时读到的尾部只是提交瞬间的
                // 画面，会被误当成命令输出。拒掉比返回误导性内容好。
                Some(_) if !*wait => Err(ApiError::invalid_params(
                    "tail_lines requires wait: true, because there is no completion moment to read at",
                )),
                _ => validate_tail_lines(*tail_lines),
            }
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
        OrchestrateStep::Wait { timeout_ms, tail_lines, .. } => {
            validate_timeout(*timeout_ms, "wait timeout_ms")?;
            validate_tail_lines(*tail_lines)
        },
        OrchestrateStep::NewTab { .. }
        | OrchestrateStep::Focus { .. }
        | OrchestrateStep::Split { .. } => Ok(()),
    }
}

/// tail_lines 只决定观察窗口大小，字节量由回执预算兜住；这里只拦明显无意义的值。
fn validate_tail_lines(tail_lines: Option<usize>) -> Result<(), ApiError> {
    match tail_lines {
        Some(0) => Err(ApiError::invalid_params("tail_lines must be at least 1")),
        Some(lines) if lines > MAX_READ_LINES => {
            Err(ApiError::invalid_params(format!("tail_lines must not exceed {MAX_READ_LINES}")))
        },
        _ => Ok(()),
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
    enforce_tail_budget(&mut receipts, MAX_RECEIPT_TAIL_BYTES);
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

/// 把整份回执的屏幕内容收进一个共享字节预算。总量没超就一字不动——常见情况零
/// 代价；超了才按份均分，每份从末尾保留：命令的结论和报错都在输出末尾，掐头
/// 比去尾安全。截断一律留下 `truncated` 与原始字节数，不做无声删减。
fn enforce_tail_budget(receipts: &mut [StepReceipt], budget: usize) {
    let mut slots = Vec::new();
    for (index, receipt) in receipts.iter().enumerate() {
        if let Some(len) = tail_slot(receipt).and_then(|tail| tail_text_len(tail)) {
            slots.push((index, len));
        }
    }
    if slots.is_empty() || slots.iter().map(|(_, len)| *len).sum::<usize>() <= budget {
        return;
    }
    let per_slot = (budget / slots.len()).max(1);
    for (index, _) in slots {
        if let Some(tail) = receipts.get_mut(index).and_then(tail_slot_mut) {
            truncate_tail(tail, per_slot);
        }
    }
}

fn tail_slot(receipt: &StepReceipt) -> Option<&Value> {
    let from_action = receipt.action.as_ref().and_then(|action| action.get("tail"));
    from_action.or_else(|| {
        receipt.error.as_ref()?.details.as_ref().and_then(|details| details.get("tail"))
    })
}

fn tail_slot_mut(receipt: &mut StepReceipt) -> Option<&mut Value> {
    if let Some(action) = receipt.action.as_mut()
        && let Some(tail) = action.get_mut("tail")
    {
        return Some(tail);
    }
    receipt.error.as_mut()?.details.as_mut()?.get_mut("tail")
}

/// tail 有两种形态：编排步骤写入的 `{text, ...}` 对象，以及 run 等待直接塞进
/// 错误 details 的裸字符串。两者都要计入同一个预算。
fn tail_text_len(tail: &Value) -> Option<usize> {
    match tail {
        Value::String(text) => Some(text.len()),
        Value::Object(object) => object.get("text").and_then(Value::as_str).map(str::len),
        _ => None,
    }
}

fn truncate_tail(tail: &mut Value, budget: usize) {
    let original = match tail {
        Value::String(text) => std::mem::take(text),
        Value::Object(object) => match object.get_mut("text") {
            Some(Value::String(text)) => std::mem::take(text),
            _ => return,
        },
        _ => return,
    };
    let kept = keep_trailing_bytes(&original, budget).to_owned();
    let truncated = kept.len() < original.len();
    match tail {
        Value::String(slot) => {
            if truncated {
                // 裸字符串没有地方挂元数据，就地说明比静默丢内容好。
                *slot = format!(
                    "[truncated to last {} of {} bytes]\n{kept}",
                    kept.len(),
                    original.len()
                );
            } else {
                *slot = kept;
            }
        },
        Value::Object(object) => {
            object.insert("text".to_owned(), Value::String(kept.clone()));
            if truncated {
                object.insert("truncated".to_owned(), Value::Bool(true));
                object.insert("original_bytes".to_owned(), Value::Number(original.len().into()));
                object.insert("returned_bytes".to_owned(), Value::Number(kept.len().into()));
            }
        },
        _ => {},
    }
}

/// 从末尾保留至多 `budget` 字节，并把起点推到最近的字符边界，避免切出半个字符。
fn keep_trailing_bytes(text: &str, budget: usize) -> &str {
    if text.len() <= budget {
        return text;
    }
    let mut start = text.len() - budget;
    while start < text.len() && !text.is_char_boundary(start) {
        start += 1;
    }
    &text[start..]
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

    if let OrchestrateStep::Wait { id, target, state, timeout_ms, tail_lines } = step {
        let (window_id, pane_id) = resolve_target(id, target, actions)?;
        // 基线取自被引用步骤的回执：那一步提交时 pane 的状态序号。只有序号真的往前
        // 走过，才算"这次等待等到了新变化"，而不是撞上提交之前的旧状态。
        let after_seq = target_baseline_seq(target, actions);
        let outcome = wait_for_pane_state(
            hub,
            window_id,
            pane_id,
            *state,
            after_seq,
            Duration::from_millis(*timeout_ms),
        );
        let lines = tail_lines.unwrap_or(0);
        return match outcome {
            Ok(mut action) => {
                if lines > 0
                    && let Some(tail) = read_pane_tail(sink, hub, window_id, pane_id, lines)
                    && let Some(object) = action.as_object_mut()
                {
                    object.insert("tail".to_owned(), tail);
                }
                Ok(StepOutcome::Complete(action))
            },
            // 等不到就把现场带回来：为什么没到达目标状态，只有屏幕说得清。
            Err(error) => {
                Err(attach_failure_evidence(error, sink, hub, lines.max(EVIDENCE_TAIL_LINES)))
            },
        };
    }

    let mut tail_request = None;
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
        OrchestrateStep::Run { id, target, command, wait, timeout_ms, tail_lines } => {
            let (window_id, pane_id) = resolve_target(id, target, actions)?;
            if let Some(lines) = *tail_lines {
                tail_request = Some((window_id, pane_id, lines));
            }
            RuntimeCommand::Run {
                window_id,
                pane_id,
                command: command.clone(),
                wait: *wait,
                timeout_ms: *timeout_ms,
            }
        },
        OrchestrateStep::AgentLaunch { .. } => unreachable!("handled above"),
        OrchestrateStep::Wait { .. } => unreachable!("handled above"),
    };
    let result = dispatch_runtime_command(command, sink, hub).map_err(|error| {
        // 命令没能正常结束时，请求过 tail 的调用方同样需要现场——失败比成功更需要。
        match tail_request {
            Some((_, _, lines)) => attach_failure_evidence(error, sink, hub, lines),
            None => error,
        }
    })?;
    let mut action = essential_action(&result);
    // 把提交这一刻的状态序号留在回执里。后面的 wait 步以它为基线，否则"发完 prompt
    // 等 settled"会立刻命中提交之前那个还没变的空闲态。
    if let Some(seq) = submitted_state_seq(&result, &action)
        && let Some(object) = action.as_object_mut()
    {
        object.insert("state_change_seq".to_owned(), Value::Number(seq.into()));
    }
    if let Some((window_id, pane_id, lines)) = tail_request
        && let Some(tail) = read_pane_tail(sink, hub, window_id, pane_id, lines)
        && let Some(object) = action.as_object_mut()
    {
        object.insert("tail".to_owned(), tail);
    }
    Ok(StepOutcome::Complete(action))
}

/// 从这一步返回的内嵌快照里取出目标 pane 的状态序号。取不到就不写——基线缺失只是
/// 让后续 wait 退回"任意匹配"，写一个猜的值才会真正骗到调用方。
fn submitted_state_seq(result: &Value, action: &Value) -> Option<u64> {
    let pane_id = action.get("pane_id").and_then(Value::as_u64)?;
    let window_id = action.get("window_id").and_then(Value::as_u64);
    let snapshot = result.get("snapshot")?;
    let snapshot: RuntimeSnapshot = serde_json::from_value(snapshot.clone()).ok()?;
    snapshot.pane(window_id, pane_id).ok().map(|pane| pane.state_change_seq)
}

/// 取被引用步骤回执里的状态序号做等待基线。直接指定 pane 的目标没有基线可言，
/// 那种情况下等待退回"匹配即返回"。
fn target_baseline_seq(target: &PaneTarget, actions: &HashMap<String, Value>) -> Option<u64> {
    let PaneTarget::Reference(reference) = target else { return None };
    actions.get(&reference.step)?.get("state_change_seq").and_then(Value::as_u64)
}

/// 等 pane 到达目标状态。循环判据与 `pane.wait` 完全一致——同一个谓词、同一个
/// 生命周期错误检查，区别只是结果作为编排步骤的 action 返回而不是写回连接。
fn wait_for_pane_state(
    hub: &RuntimeHub,
    window_id: Option<u64>,
    pane_id: u64,
    state: RuntimeWaitState,
    after_seq: Option<u64>,
    timeout: Duration,
) -> Result<Value, ApiError> {
    let (_, current, receiver) = hub.subscribe();
    let mut target_window = window_id;
    let mut observed = None;
    let deadline = Instant::now() + timeout;
    let mut snapshot = current;
    loop {
        if let Some(error) = hub.pane_lifecycle_error(target_window, pane_id) {
            return Err(error);
        }
        if let Some(current) = snapshot.take() {
            match current.pane_target(target_window, pane_id) {
                Ok((resolved, pane)) => {
                    target_window = Some(resolved);
                    if command::wait_matches(pane, state, after_seq) {
                        return Ok(json!({
                            "window_id": resolved,
                            "pane_id": pane_id,
                            "task_state": pane.task_state,
                            "state_change_seq": pane.state_change_seq
                        }));
                    }
                    observed = Some((pane.task_state, pane.state_change_seq));
                },
                Err(error) if error.code == "target_not_found" => {},
                Err(error) => return Err(error),
            }
        }
        let remaining = deadline.saturating_duration_since(Instant::now());
        if remaining.is_zero() {
            break;
        }
        snapshot = match receiver.recv_timeout(remaining) {
            Ok(snapshot) => Some(snapshot),
            Err(RecvTimeoutError::Timeout | RecvTimeoutError::Disconnected) => break,
        };
    }
    if let Some(error) = hub.pane_lifecycle_error(target_window, pane_id) {
        return Err(error);
    }
    Err(ApiError::new(
        "timeout",
        format!("pane {pane_id} did not reach the requested state before timeout"),
    )
    .details(json!({
        "window_id": target_window,
        "pane_id": pane_id,
        "expected_state": state,
        "after_seq": after_seq,
        "observed_state": observed.map(|(state, _)| state),
        "observed_state_change_seq": observed.map(|(_, seq)| seq)
    })))
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

/// 读一次 pane 尾部作为失败现场。读取本身失败不能覆盖原始错误，因此返回
/// Option 而不是 Result：拿不到现场就只是少一份证据，原因仍照实上报。
fn read_pane_tail(
    sink: &EventSink,
    hub: &RuntimeHub,
    window_id: Option<u64>,
    pane_id: u64,
    lines: usize,
) -> Option<Value> {
    let (text, scanned) = read_pane_tail_text(hub, sink, window_id, pane_id, lines)?;
    let returned_lines = if text.is_empty() { 0 } else { text.lines().count() };
    Some(json!({
        "text": text,
        "returned_lines": returned_lines,
        "requested_lines": lines,
        "scanned_lines": scanned
    }))
}

/// 把现场并进错误 details。目标坐标取错误自带的 window/pane，这样就绪超时与
/// 驻留态阻断共用同一条补证据的路径，调用方不必按错误码分别处理。
fn attach_failure_evidence(
    mut error: ApiError,
    sink: &EventSink,
    hub: &RuntimeHub,
    lines: usize,
) -> ApiError {
    let Some(details) = error.details.as_ref().and_then(Value::as_object) else { return error };
    if details.contains_key("tail") {
        return error;
    }
    let Some(pane_id) = details.get("pane_id").and_then(Value::as_u64) else { return error };
    let window_id = details.get("window_id").and_then(Value::as_u64);
    let Some(tail) = read_pane_tail(sink, hub, window_id, pane_id, lines) else { return error };
    let mut details = details.clone();
    details.insert("tail".to_owned(), tail);
    error.details = Some(Value::Object(details));
    error
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
        })
        // 没能提交 prompt 时把屏幕尾部并进错误：调用方要判断"是登录页还是网络挂住"
        // 只能靠现场，而这一步的现场在返回后就被下一帧覆盖了。
        .map_err(|error| attach_failure_evidence(error, sink, hub, EVIDENCE_TAIL_LINES));
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
            if agent.observed {
                // 只有确认空闲才算就绪。此前判据是 `!= Running`，于是停在登录、
                // 更新确认或任何交互提问上的 Agent 都被当成就绪，initial_prompt
                // 直接打进那个弹窗。非空闲的驻留态一律带现场返回，不提交 prompt。
                match pane.task_state {
                    RuntimeTaskState::Idle => return Ok((agent, pane.task_state)),
                    RuntimeTaskState::Running => {},
                    blocked => {
                        return Err(agent_not_ready(&agent, blocked, observed_agent));
                    },
                }
            }
        }

        let remaining = deadline.saturating_duration_since(Instant::now());
        if remaining.is_zero() {
            let hint = agent_pane_hint(hub, agent_id, generation);
            return Err(ApiError::new(
                "agent_ready_timeout",
                "the launched agent was not ready for its initial prompt before timeout",
            )
            .details(json!({
                "agent_id": agent_id,
                "generation": generation,
                "observed": observed_agent,
                "task_state": observed_state,
                "submitted": false,
                "window_id": hint.map(|(window, _)| window),
                "pane_id": hint.map(|(_, pane)| pane)
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

/// Agent 进程活着但没停在可接受 prompt 的位置。错误码按驻留态区分，让调用方
/// 一眼看出"被挡住了"和"启动就退出了"是两回事，两者的续作动作完全不同。
fn agent_not_ready(
    agent: &RuntimeManagedAgent,
    state: RuntimeTaskState,
    observed: bool,
) -> ApiError {
    let (code, message) = match state {
        RuntimeTaskState::Attention | RuntimeTaskState::WaitingInput => (
            "agent_not_ready",
            "the agent stopped on a screen that needs an answer, so its initial prompt was not \
             submitted",
        ),
        RuntimeTaskState::Finished => {
            ("agent_exited_before_ready", "the agent process exited before accepting a prompt")
        },
        RuntimeTaskState::Failed => {
            ("agent_failed_before_ready", "the agent process failed before accepting a prompt")
        },
        RuntimeTaskState::Idle | RuntimeTaskState::Running => {
            ("agent_not_ready", "the agent was not idle when its initial prompt was due")
        },
    };
    ApiError::new(code, message).details(json!({
        "agent_id": agent.agent_id,
        "generation": agent.generation,
        "observed": observed,
        "task_state": state,
        "window_id": agent.window_id,
        "pane_id": agent.pane_id,
        "submitted": false
    }))
}

fn agent_pane_hint(hub: &RuntimeHub, agent_id: &str, generation: u64) -> Option<(u64, u64)> {
    hub.active_agent(agent_id, Some(generation)).ok().map(|agent| (agent.window_id, agent.pane_id))
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
