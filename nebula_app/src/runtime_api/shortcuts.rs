//! 资源 + 动词形式的控制命令。
//!
//! [`super::cli`] 暴露的是完整协议——每个方法、每个旋钮都在，适合脚本和长期
//! 集成。但一个模型在 pane 里临时要派个活儿时，`nebula ctl agent-prompt --agent
//! codex --text "…"` 这种长度本身就是失败源：拼错一个 flag 就退化成"我试过了但
//! 没成功"。所以这里给同一批能力配一套按资源分组的入口：
//!
//! ```text
//! nebula env                        我在哪、控制面在哪、有哪些命令
//! nebula pane list                  所有 pane
//! nebula pane read <id>             读某个 pane 的输出
//! nebula pane send <id> <文本>      写一行进某个 pane
//! nebula pane wait <id>             等某个 pane
//! nebula agent list                 只有 AI CLI 的 pane
//! nebula agent send <名字> <任务>   派任务并提交
//! nebula agent read <名字>          读它最近打印了什么
//! nebula agent wait <名字>          等它这一轮结束
//! ```
//!
//! 命名刻意选了 CLI 界的通用惯例（资源在前、动词在后，同 `kubectl` / `docker`
//! / `gh`），而不是自造词：别人看一眼就知道在做什么，比"短"更重要。
//!
//! `pane` 与 `agent` 分成两个资源不只是为了好读——它们走的是**不同**的协议方法。
//! Agent 路径带 generation 绑定（Codex 退出重开后不会把任务投给新会话），pane
//! 路径没有这层保护。用两个资源名把这条边界摆在命令行上，比塞进一个参数再靠
//! 前缀区分要难错得多。
//!
//! 这些都是 [`super::cli`] 的**薄别名**，不新增协议方法，也不绕过任何校验：
//! 同一个 `request_once`、同一套 `ApiResponse` 信封、同样的 generation /
//! `after_seq` 竞态保护。短的只是命令行，不是语义。

use super::cli::{CliError, PrintedCliError, print_response, request_once, wait_state_name};
use super::*;
use crate::cli::{
    AgentCommand, AgentOptions, AgentPasteOptions, AgentReadOptions, AgentSendOptions,
    AgentWaitOptions, EnvOptions, ListOptions, PaneCloseOptions, PaneCommand, PaneExecOptions,
    PaneOptions, PanePasteOptions, PaneReadOptions, PaneResizeOptions, PaneSendOptions,
    PaneWaitOptions, PaneZoomOptions, PasteSourceOptions, TabCloseOptions, TabCommand,
    TabMoveOptions, TabRenameOptions, TabResourceOptions, WindowCloseOptions, WindowCommand,
    WindowResourceOptions,
};
use std::fs::File;
use std::io::Read as _;

/// 与 `ctl` 一致的错误出口：失败也回一个 JSON 信封，而不是裸文本。
///
/// 调用方解析 `error.code` 的路径因此不随命令长短改变——这些命令是别名，不是
/// 另一套约定。`PrintedCliError` 表示响应已经打印过（那是 runtime 自己的错误
/// 信封），不再包一层。
fn finish(pretty: bool, outcome: Result<(), Box<dyn Error>>) -> Result<(), Box<dyn Error>> {
    match outcome {
        Ok(()) => Ok(()),
        Err(error) if error.downcast_ref::<PrintedCliError>().is_some() => Err(error),
        Err(error) => {
            let code =
                error.downcast_ref::<CliError>().map_or("cli_transport_error", CliError::code);
            let response = ApiResponse::failure("cli", ApiError::new(code, error.to_string()));
            print_response(&response, pretty)
        },
    }
}

pub fn window(options: WindowResourceOptions) -> Result<(), Box<dyn Error>> {
    match options.command {
        WindowCommand::Close(options) => {
            let pretty = options.output.pretty;
            finish(pretty, window_close(options))
        },
    }
}

pub fn tab(options: TabResourceOptions) -> Result<(), Box<dyn Error>> {
    match options.command {
        TabCommand::Close(options) => {
            let pretty = options.output.pretty;
            finish(pretty, tab_close(options))
        },
        TabCommand::Rename(options) => {
            let pretty = options.output.pretty;
            finish(pretty, tab_rename(options))
        },
        TabCommand::Move(options) => {
            let pretty = options.output.pretty;
            finish(pretty, tab_move(options))
        },
    }
}

pub fn pane(options: PaneOptions) -> Result<(), Box<dyn Error>> {
    match options.command {
        PaneCommand::List(options) => {
            let pretty = options.output.pretty;
            finish(pretty, pane_list(options))
        },
        PaneCommand::Read(options) => {
            let pretty = options.output.pretty;
            finish(pretty, pane_read(options))
        },
        PaneCommand::Send(options) => {
            let pretty = options.output.pretty;
            finish(pretty, pane_send(options))
        },
        PaneCommand::Paste(options) => {
            let pretty = options.output.pretty;
            finish(pretty, pane_paste(options))
        },
        PaneCommand::Wait(options) => {
            let pretty = options.output.pretty;
            finish(pretty, pane_wait(options))
        },
        PaneCommand::Exec(options) => {
            let pretty = options.output.pretty;
            finish(pretty, pane_exec(options))
        },
        PaneCommand::Close(options) => {
            let pretty = options.output.pretty;
            finish(pretty, pane_close(options))
        },
        PaneCommand::Zoom(options) => {
            let pretty = options.output.pretty;
            finish(pretty, pane_zoom(options))
        },
        PaneCommand::Resize(options) => {
            let pretty = options.output.pretty;
            finish(pretty, pane_resize(options))
        },
    }
}

pub fn agent(options: AgentOptions) -> Result<(), Box<dyn Error>> {
    match options.command {
        AgentCommand::List(options) => {
            let pretty = options.output.pretty;
            finish(pretty, agent_list(options))
        },
        AgentCommand::Send(options) => {
            let pretty = options.output.pretty;
            finish(pretty, agent_send(options))
        },
        AgentCommand::Paste(options) => {
            let pretty = options.output.pretty;
            finish(pretty, agent_paste(options))
        },
        AgentCommand::Read(options) => {
            let pretty = options.output.pretty;
            finish(pretty, agent_read(options))
        },
        AgentCommand::Wait(options) => {
            let pretty = options.output.pretty;
            finish(pretty, agent_wait(options))
        },
    }
}

fn window_close(options: WindowCloseOptions) -> Result<(), Box<dyn Error>> {
    let timeout = validated_timeout(options.timeout_ms)?;
    let response = request_once("window.close", json!({ "window_id": options.window }), timeout)?;
    print_response(&response, options.output.pretty)
}

fn tab_close(options: TabCloseOptions) -> Result<(), Box<dyn Error>> {
    let timeout = validated_timeout(options.timeout_ms)?;
    let response = request_once(
        "tab.close",
        json!({ "window_id": options.window, "tab_index": options.tab }),
        timeout,
    )?;
    print_response(&response, options.output.pretty)
}

fn tab_rename(options: TabRenameOptions) -> Result<(), Box<dyn Error>> {
    let timeout = validated_timeout(options.timeout_ms)?;
    let response = request_once(
        "tab.rename",
        json!({
            "window_id": options.window,
            "tab_index": options.tab,
            "name": options.name
        }),
        timeout,
    )?;
    print_response(&response, options.output.pretty)
}

fn tab_move(options: TabMoveOptions) -> Result<(), Box<dyn Error>> {
    let timeout = validated_timeout(options.timeout_ms)?;
    let response = request_once(
        "tab.move",
        json!({
            "window_id": options.window,
            "tab_index": options.tab,
            "to_index": options.to
        }),
        timeout,
    )?;
    print_response(&response, options.output.pretty)
}

fn pane_close(options: PaneCloseOptions) -> Result<(), Box<dyn Error>> {
    let timeout = validated_timeout(options.timeout_ms)?;
    let response = request_once(
        "pane.close",
        json!({ "window_id": options.window, "pane_id": options.pane }),
        timeout,
    )?;
    print_response(&response, options.output.pretty)
}

fn pane_exec(options: PaneExecOptions) -> Result<(), Box<dyn Error>> {
    let timeout = validated_timeout(options.timeout_ms)?;
    let response = request_once(
        "pane.exec",
        json!({
            "window_id": options.window,
            "pane_id": options.pane,
            "argv": options.argv,
            "timeout_ms": options.timeout_ms,
            "max_output_bytes": options.max_output_bytes
        }),
        timeout.saturating_add(Duration::from_secs(2)),
    )?;
    print_response(&response, options.output.pretty)
}

fn pane_zoom(options: PaneZoomOptions) -> Result<(), Box<dyn Error>> {
    let timeout = validated_timeout(options.timeout_ms)?;
    let response = request_once(
        "pane.zoom",
        json!({
            "window_id": options.window,
            "pane_id": options.pane,
            "zoomed": options.zoomed
        }),
        timeout,
    )?;
    print_response(&response, options.output.pretty)
}

fn pane_resize(options: PaneResizeOptions) -> Result<(), Box<dyn Error>> {
    let timeout = validated_timeout(options.timeout_ms)?;
    let response = request_once(
        "pane.resize",
        json!({
            "window_id": options.window,
            "pane_id": options.pane,
            "ratio": options.ratio
        }),
        timeout,
    )?;
    print_response(&response, options.output.pretty)
}

/// `nebula env` —— 发现层入口。
///
/// 环境那半段**不依赖 runtime**：即使控制面没起来、端口文件过期、或这根本不是
/// Nebula 的 pane，命令仍然成功返回并如实说明缺什么。这是刻意的——一个探测命令
/// 若在"没连上"时整体失败，调用方唯一能学到的就是"不知道"，只好去猜。
pub fn env(options: EnvOptions) -> Result<(), Box<dyn Error>> {
    let pretty = options.output.pretty;
    finish(pretty, env_inner(options))
}

fn env_inner(options: EnvOptions) -> Result<(), Box<dyn Error>> {
    let timeout = validated_timeout(options.timeout_ms)?;
    let pane_id = std::env::var(crate::agent_env::PANE_ENV).ok().and_then(|id| id.parse().ok());
    let runtime = probe_runtime(timeout);
    let reachable = runtime.get("reachable").and_then(Value::as_bool).unwrap_or(false);
    let response = ApiResponse::success(
        "env",
        json!({
            "inside_nebula": std::env::var("TERM_PROGRAM").as_deref()
                == Ok(crate::agent_env::TERM_PROGRAM),
            "term_program": std::env::var("TERM_PROGRAM").ok(),
            "term_program_version": std::env::var("TERM_PROGRAM_VERSION").ok(),
            "pane_id": pane_id,
            "cli": std::env::var(crate::agent_env::CLI_ENV).ok(),
            "bin_dir": std::env::var(crate::agent_env::BIN_DIR_ENV).ok(),
            // 远端 pane 不得取用本地上下文，控制面对它也不适用。明说比让调用方
            // 从一堆 `null` 里反推更省事。
            "remote": std::env::var("NEBULA_PANE_REMOTE").is_ok(),
            "runtime": runtime,
            // 控制面没通时不要再发第二次请求——那只会让一条探测命令白等两个
            // 超时，而答案早就写在 `runtime` 段里了。
            "self": pane_id
                .filter(|_| reachable)
                .and_then(|pane_id| describe_self(pane_id, timeout)),
            "commands": command_catalog(),
        }),
    );
    print_response(&response, options.output.pretty)
}

/// 探测控制面。三种结果都要能区分：通、没通、通了但拒绝。
fn probe_runtime(timeout: Duration) -> Value {
    match request_once("runtime.describe", json!({}), timeout) {
        Ok(response) if response.ok => {
            json!({ "reachable": true, "describe": response.result })
        },
        Ok(response) => json!({ "reachable": true, "error": response.error }),
        Err(error) => json!({
            "reachable": false,
            "error": { "code": "runtime_unavailable", "message": error.to_string() },
        }),
    }
}

/// 从一个响应的 `result` 里取出快照。
///
/// 两种形状都要认：`runtime.snapshot` 的 `result` **就是**快照本体，而
/// `pane.prompt` / `agent.prompt` 一类的 `result` 是
/// `{"action": …, "snapshot": …}`。混淆这两者的后果是安静的——压平会得到空列表、
/// `self` 会永远是 `null`——所以这里集中处理一次，而不是在每个调用点各写一遍。
fn snapshot_of(result: &Value) -> Option<RuntimeSnapshot> {
    let value = result.get("snapshot").unwrap_or(result);
    serde_json::from_value(value.clone()).ok()
}

/// 从快照里摘出调用者自己那个 pane。
///
/// 失败一律降级为 `None` 而不是让 `env` 失败：这只是锦上添花的一段，缺了不
/// 影响调用方拿到身份和命令清单。
fn describe_self(pane_id: u64, timeout: Duration) -> Option<Value> {
    let response = request_once("runtime.snapshot", json!({}), timeout).ok()?;
    if !response.ok {
        return None;
    }
    let snapshot = snapshot_of(response.result.as_ref()?)?;
    serde_json::to_value(snapshot.pane(None, pane_id).ok()?).ok()
}

/// 命令清单，随 `nebula env` 一起返回。
///
/// 给的是**可直接复制执行**的样例而不是抽象签名：模型照抄一条完整命令的成功率
/// 远高于自己按参数表拼装。这一段就是"不靠 Skill 也能被发现"的实体——Skill 负责
/// 教它*什么时候*该调，这份清单负责保证它*调得对*。
fn command_catalog() -> Value {
    json!([
        {
            "command": "nebula window close <window>",
            "purpose": "Close an idle window. Busy panes return confirmation_required.",
            "example": "nebula window close 3",
        },
        {
            "command": "nebula tab close <tab> --window <window>",
            "purpose": "Close an idle tab by its zero-based index within one window.",
            "example": "nebula tab close 2 --window 3",
        },
        {
            "command": "nebula tab rename <tab> <name> --window <window>",
            "purpose": "Set a tab's custom name; an empty name restores its generated title.",
            "example": "nebula tab rename 2 tests --window 3",
        },
        {
            "command": "nebula tab move <tab> <to> --window <window>",
            "purpose": "Move a tab to another index in the same window.",
            "example": "nebula tab move 2 0 --window 3",
        },
        {
            "command": "nebula pane list",
            "purpose": "List every pane with id, task state, cwd, and Git branch.",
            "example": "nebula pane list",
        },
        {
            "command": "nebula pane read <pane>",
            "purpose": "Read the tail of a pane's real terminal buffer.",
            "example": "nebula pane read 17 --lines 80",
        },
        {
            "command": "nebula pane send <pane> <text>",
            "purpose": "Write one line into a pane and press Enter.",
            "example": "nebula pane send 17 \"cargo test\" --wait",
        },
        {
            "command": "nebula pane paste <pane> [text|--stdin|--from-file]",
            "purpose": "Paste bounded UTF-8 as one bracketed block. Local stdin/file content is \
                        never forwarded to SSH panes.",
            "example": "nebula pane paste 17 --from-file task.txt --wait",
        },
        {
            "command": "nebula pane wait <pane>",
            "purpose": "Block until a pane settles.",
            "example": "nebula pane wait 17 --after-seq 41",
        },
        {
            "command": "nebula pane exec <pane> -- <program> [args]",
            "purpose": "Run an independent non-TTY child in the pane's local cwd and capture stdout/stderr separately.",
            "example": "nebula pane exec 17 -- cargo test",
        },
        {
            "command": "nebula pane close <pane>",
            "purpose": "Close an idle pane. Busy panes return confirmation_required.",
            "example": "nebula pane close 17",
        },
        {
            "command": "nebula pane zoom <pane> --zoomed <true|false>",
            "purpose": "Idempotently enable or disable focused-pane zoom for the pane's tab.",
            "example": "nebula pane zoom 17 --zoomed true",
        },
        {
            "command": "nebula pane resize <pane> <ratio>",
            "purpose": "Set the pane's share of its direct parent split, from 0.05 through 0.95.",
            "example": "nebula pane resize 17 0.60",
        },
        {
            "command": "nebula agent list",
            "purpose": "List the panes running an AI CLI, with session identity and generation. \
                        Resolve a delegation target here first — never send to whichever pane \
                        happens to be focused.",
            "example": "nebula agent list",
        },
        {
            "command": "nebula agent send <agent> <task>",
            "purpose": "Hand one task to an agent and submit it. Bound to the agent's current \
                        generation, so a session that restarted in the meantime cannot inherit \
                        work aimed at the one it replaced.",
            "example": "nebula agent send codex \"fix the login regression in auth/\" --wait",
        },
        {
            "command": "nebula agent paste <agent> [text|--stdin|--from-file]",
            "purpose": "Paste a bounded multi-line task into the same managed-agent generation.",
            "example": "Get-Content task.md | nebula agent paste codex --stdin --wait",
        },
        {
            "command": "nebula agent read <agent>",
            "purpose": "Read the tail of what the agent printed, straight from its terminal grid.",
            "example": "nebula agent read codex --lines 80",
        },
        {
            "command": "nebula agent wait <agent>",
            "purpose": "Block until the agent's turn ends. Pass --after-seq with the \
                        state_change_seq observed before dispatching, so an already-idle agent \
                        cannot satisfy the wait immediately.",
            "example": "nebula agent wait codex --after-seq 41",
        },
        {
            "command": "nebula ctl --help",
            "purpose": "The full protocol: split panes, start or fork agents into isolated Git \
                        worktrees, run commands for a real exit code, subscribe to state events, \
                        or drive a whole multi-agent layout in one orchestrate request.",
            "example": "nebula ctl describe --pretty",
        },
    ])
}

fn pane_list(options: ListOptions) -> Result<(), Box<dyn Error>> {
    let timeout = validated_timeout(options.timeout_ms)?;
    let response = request_once("runtime.snapshot", json!({}), timeout)?;
    // 快照是整棵窗口树。这条命令的用途是"给我一张可选目标表"，所以压平成一维
    // 行，并且只保留派活儿时真正需要的字段。
    let Some(flattened) = flatten_panes(&response, options.window) else {
        return print_response(&response, options.output.pretty);
    };
    print_response(&ApiResponse::success("pane.list", flattened), options.output.pretty)
}

/// 把窗口树压平成 pane 行。返回 `None` 表示响应不是预期形状（例如 runtime 报
/// 错），调用方应原样透传，不要伪造一个空列表——空列表会被读成"确实没有 pane"。
fn flatten_panes(response: &ApiResponse, window: Option<u64>) -> Option<Value> {
    let snapshot = snapshot_of(response.result.as_ref()?)?;
    let mut panes = Vec::new();
    for tree in snapshot.windows.iter().filter(|tree| window.is_none_or(|id| tree.id == id)) {
        for tab in &tree.tabs {
            for pane in &tab.panes {
                panes.push(json!({
                    "window_id": tree.id,
                    "tab_index": tab.index,
                    "tab_active": tab.active,
                    "pane_id": pane.id,
                    "pane_active": pane.active,
                    "title": pane.title,
                    "cwd": pane.cwd,
                    "branch": pane.branch,
                    "ssh_destination": pane.ssh_destination,
                    "agent": pane.agent,
                    "task_state": pane.task_state,
                    "state_change_seq": pane.state_change_seq,
                }));
            }
        }
    }
    Some(json!({ "revision": snapshot.revision, "panes": panes }))
}

fn pane_read(options: PaneReadOptions) -> Result<(), Box<dyn Error>> {
    let timeout = validated_timeout(options.timeout_ms)?;
    let response = request_once(
        "pane.read",
        json!({ "window_id": options.window, "pane_id": options.pane, "lines": options.lines }),
        timeout,
    )?;
    print_response(&response, options.output.pretty)
}

fn pane_send(options: PaneSendOptions) -> Result<(), Box<dyn Error>> {
    let timeout = validated_timeout(options.timeout_ms)?;
    let response = request_once(
        "pane.prompt",
        json!({
            "window_id": options.window,
            "pane_id": options.pane,
            "text": joined(&options.text),
            "submit": !options.no_submit,
        }),
        timeout,
    )?;
    if !response.ok || !options.wait {
        return print_response(&response, options.output.pretty);
    }
    let wait_timeout = validated_timeout(options.wait_timeout_ms)?;
    // 基线取自提交后的那张快照。用它当 `after_seq` 才让随后的等待意味着
    // "又静下来了"，而不是"本来就是静的"——这正是把提交前的 idle 误判成
    // "已完成"的那个经典竞态。
    let Some(baseline) = dispatched_state_change_seq(&response, options.window, Some(options.pane))
    else {
        return Err(CliError::new(
            "runtime_no_response",
            "the line was submitted but the response carried no pane state baseline; retry the \
             wait with the state_change_seq from `nebula pane list`",
        )
        .into());
    };
    let response = request_once(
        "pane.wait",
        json!({
            "window_id": options.window,
            "pane_id": options.pane,
            "state": wait_state_name(crate::cli::ControlWaitState::Settled),
            "timeout_ms": wait_timeout.as_millis() as u64,
            "after_seq": baseline,
        }),
        wait_timeout.saturating_add(Duration::from_secs(1)),
    )?;
    print_response(&response, options.output.pretty)
}

fn pane_paste(options: PanePasteOptions) -> Result<(), Box<dyn Error>> {
    let timeout = validated_timeout(options.timeout_ms)?;
    let text = read_paste_source(options.source)?;
    let response = request_once(
        "pane.paste",
        json!({
            "window_id": options.window,
            "pane_id": options.pane,
            "text": text,
            "submit": !options.no_submit,
        }),
        timeout,
    )?;
    if !response.ok || !options.wait {
        return print_response(&response, options.output.pretty);
    }
    let wait_timeout = validated_timeout(options.wait_timeout_ms)?;
    let Some(baseline) = dispatched_state_change_seq(&response, options.window, Some(options.pane))
    else {
        return Err(CliError::new(
            "runtime_no_response",
            "the paste was submitted but the response carried no pane state baseline; retry the \
             wait with the state_change_seq from `nebula pane list`",
        )
        .into());
    };
    let response = request_once(
        "pane.wait",
        json!({
            "window_id": options.window,
            "pane_id": options.pane,
            "state": wait_state_name(crate::cli::ControlWaitState::Settled),
            "timeout_ms": wait_timeout.as_millis() as u64,
            "after_seq": baseline,
        }),
        wait_timeout.saturating_add(Duration::from_secs(1)),
    )?;
    print_response(&response, options.output.pretty)
}

fn pane_wait(options: PaneWaitOptions) -> Result<(), Box<dyn Error>> {
    let timeout = validated_timeout(options.timeout_ms)?;
    let response = request_once(
        "pane.wait",
        json!({
            "window_id": options.window,
            "pane_id": options.pane,
            "state": wait_state_name(options.state),
            "timeout_ms": timeout.as_millis() as u64,
            "after_seq": options.after_seq,
        }),
        timeout.saturating_add(Duration::from_secs(1)),
    )?;
    print_response(&response, options.output.pretty)
}

fn agent_list(options: ListOptions) -> Result<(), Box<dyn Error>> {
    let timeout = validated_timeout(options.timeout_ms)?;
    let response = request_once("agents.list", json!({ "window_id": options.window }), timeout)?;
    print_response(&response, options.output.pretty)
}

fn agent_send(options: AgentSendOptions) -> Result<(), Box<dyn Error>> {
    let timeout = validated_timeout(options.timeout_ms)?;
    let response = request_once(
        "agent.prompt",
        json!({
            "agent": options.agent,
            "generation": options.generation,
            "text": joined(&options.text),
            "submit": !options.no_submit,
        }),
        timeout,
    )?;
    if !response.ok || !options.wait {
        return print_response(&response, options.output.pretty);
    }
    let wait_timeout = validated_timeout(options.wait_timeout_ms)?;
    let Some(baseline) = dispatched_state_change_seq(&response, None, None) else {
        return Err(CliError::new(
            "runtime_no_response",
            "the task was delivered but the response carried no agent state baseline; re-resolve \
             the target with `nebula agent list` before waiting",
        )
        .into());
    };
    // `agent.wait` 要求显式代际——这是"别把另一个会话的静默当成这次任务完成"的
    // 那道锁。派发成功的响应正常都带它；真取不到时明确报错，而不是送一个 `null`
    // 让服务端回一句不知所云的 `invalid_params`。
    let Some(generation) = options.generation.or_else(|| dispatched_generation(&response)) else {
        return Err(CliError::new(
            "runtime_no_response",
            "the task was delivered but the response carried no agent generation; \
             re-resolve the target with `nebula agent list` before waiting",
        )
        .into());
    };
    let response = request_once(
        "agent.wait",
        json!({
            "agent": options.agent,
            "generation": generation,
            "state": wait_state_name(crate::cli::ControlWaitState::Settled),
            "timeout_ms": wait_timeout.as_millis() as u64,
            "after_seq": baseline,
        }),
        wait_timeout.saturating_add(Duration::from_secs(1)),
    )?;
    print_response(&response, options.output.pretty)
}

fn agent_paste(options: AgentPasteOptions) -> Result<(), Box<dyn Error>> {
    let timeout = validated_timeout(options.timeout_ms)?;
    let text = read_paste_source(options.source)?;
    let response = request_once(
        "agent.paste",
        json!({
            "agent": options.agent,
            "generation": options.generation,
            "text": text,
            "submit": !options.no_submit,
        }),
        timeout,
    )?;
    if !response.ok || !options.wait {
        return print_response(&response, options.output.pretty);
    }
    let wait_timeout = validated_timeout(options.wait_timeout_ms)?;
    let Some(baseline) = dispatched_state_change_seq(&response, None, None) else {
        return Err(CliError::new(
            "runtime_no_response",
            "the paste was delivered but the response carried no agent state baseline; \
             re-resolve the target with `nebula agent list` before waiting",
        )
        .into());
    };
    let Some(generation) = options.generation.or_else(|| dispatched_generation(&response)) else {
        return Err(CliError::new(
            "runtime_no_response",
            "the paste was delivered but the response carried no agent generation; \
             re-resolve the target with `nebula agent list` before waiting",
        )
        .into());
    };
    let response = request_once(
        "agent.wait",
        json!({
            "agent": options.agent,
            "generation": generation,
            "state": wait_state_name(crate::cli::ControlWaitState::Settled),
            "timeout_ms": wait_timeout.as_millis() as u64,
            "after_seq": baseline,
        }),
        wait_timeout.saturating_add(Duration::from_secs(1)),
    )?;
    print_response(&response, options.output.pretty)
}

fn agent_read(options: AgentReadOptions) -> Result<(), Box<dyn Error>> {
    let timeout = validated_timeout(options.timeout_ms)?;
    let response = request_once(
        "agent.read",
        json!({
            "agent": options.agent,
            "generation": options.generation,
            "lines": options.lines,
        }),
        timeout,
    )?;
    print_response(&response, options.output.pretty)
}

fn agent_wait(options: AgentWaitOptions) -> Result<(), Box<dyn Error>> {
    let timeout = validated_timeout(options.timeout_ms)?;
    // 调用方没给代际时，用当前活跃代际补齐。若期间 Agent 已经换代，`agent.get`
    // 会先报错，而不是让等待静静地盯着一个错的目标。
    let generation = match options.generation {
        Some(generation) => generation,
        None => resolve_generation(&options.agent, timeout)?,
    };
    let response = request_once(
        "agent.wait",
        json!({
            "agent": options.agent,
            "generation": generation,
            "state": wait_state_name(options.state),
            "timeout_ms": timeout.as_millis() as u64,
            "after_seq": options.after_seq,
        }),
        timeout.saturating_add(Duration::from_secs(1)),
    )?;
    print_response(&response, options.output.pretty)
}

/// 当前活跃代际。目标不存在或响应缺字段都算失败——猜一个代际去等，等到的可能是
/// 另一个会话。
fn resolve_generation(agent: &str, timeout: Duration) -> Result<u64, Box<dyn Error>> {
    let response = request_once("agent.get", json!({ "agent": agent }), timeout)?;
    if !response.ok {
        let message = response
            .error
            .as_ref()
            .map_or("agent.get failed", |error| error.message.as_str())
            .to_owned();
        return Err(CliError::new("target_not_found", message).into());
    }
    response
        .result
        .as_ref()
        .and_then(|result| result.get("agent")?.get("generation")?.as_u64())
        .ok_or_else(|| {
            CliError::new(
                "target_not_found",
                format!("no live agent matches {agent:?}; run `nebula agent list`"),
            )
            .into()
        })
}

fn read_paste_source(source: PasteSourceOptions) -> Result<String, Box<dyn Error>> {
    let PasteSourceOptions { text, stdin, from_file } = source;
    let value = if stdin {
        read_paste_utf8(std::io::stdin().lock(), "stdin")?
    } else if let Some(path) = from_file {
        let file = File::open(&path).map_err(|error| {
            CliError::new(
                "input_read_error",
                format!("could not open paste file {}: {error}", path.display()),
            )
        })?;
        read_paste_utf8(file, &path.display().to_string())?
    } else if text.is_empty() {
        return Err(CliError::new(
            "invalid_params",
            "provide paste text, --stdin, or --from-file <path>",
        )
        .into());
    } else {
        joined(&text)
    };
    validate_paste_text(&value).map_err(|error| CliError::new("invalid_params", error.message))?;
    Ok(value)
}

fn read_paste_utf8(reader: impl Read, label: &str) -> Result<String, Box<dyn Error>> {
    let mut bytes = Vec::new();
    reader.take((MAX_PROMPT_BYTES + 1) as u64).read_to_end(&mut bytes).map_err(|error| {
        CliError::new("input_read_error", format!("could not read paste input {label}: {error}"))
    })?;
    if bytes.len() > MAX_PROMPT_BYTES {
        return Err(CliError::new(
            "invalid_params",
            format!("paste text exceeds the {MAX_PROMPT_BYTES}-byte limit"),
        )
        .into());
    }
    String::from_utf8(bytes).map_err(|error| {
        CliError::new("invalid_utf8", format!("paste input {label} is not valid UTF-8: {error}"))
            .into()
    })
}

/// 多个词拼一句：`send codex fix login` 和 `send codex "fix login"` 应该等价，
/// 否则忘记加引号就变成一条截断的任务被真的派出去。
fn joined(text: &[String]) -> String {
    text.join(" ")
}

/// 派发响应里目标 pane 的状态计数器。
///
/// `runtime_result` 把动作元数据放在 `result.action`、把规范快照放在
/// `result.snapshot`。Agent 的窗口与 pane 位于 `action.agent`；普通 pane 的两者
/// 直接位于 `action`。fallback 参数只兼容不回动作元数据的旧服务端。
///
/// 调用方必须把 `None` 当成错误，不能退化为纯状态匹配：否则一个本来就 idle 的
/// pane 会立刻“满足”等待，让模型把尚未开始的任务误报成已经完成。
fn dispatched_state_change_seq(
    response: &ApiResponse,
    fallback_window: Option<u64>,
    fallback_pane: Option<u64>,
) -> Option<u64> {
    let result = response.result.as_ref()?;
    let action = result.get("action").unwrap_or(result);
    let agent = action.get("agent");
    let window_id = agent
        .and_then(|agent| agent.get("window_id"))
        .and_then(Value::as_u64)
        .or_else(|| action.get("window_id").and_then(Value::as_u64))
        .or(fallback_window);
    let pane_id = action
        .get("agent")
        .and_then(|agent| agent.get("pane_id"))
        .and_then(Value::as_u64)
        .or_else(|| action.get("pane_id").and_then(Value::as_u64))
        .or(fallback_pane)?;
    let snapshot = snapshot_of(result)?;
    snapshot.pane(window_id, pane_id).ok().map(|pane| pane.state_change_seq)
}

fn dispatched_generation(response: &ApiResponse) -> Option<u64> {
    let result = response.result.as_ref()?;
    result.get("action").unwrap_or(result).get("agent")?.get("generation")?.as_u64()
}

fn validated_timeout(timeout_ms: u64) -> Result<Duration, Box<dyn Error>> {
    if timeout_ms == 0 || Duration::from_millis(timeout_ms) > MAX_WAIT {
        return Err(CliError::new(
            "invalid_params",
            format!("timeout must be between 1 and {} ms", MAX_WAIT.as_millis()),
        )
        .into());
    }
    Ok(Duration::from_millis(timeout_ms))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn snapshot_with_pane(pane_id: u64, state_change_seq: u64) -> RuntimeSnapshot {
        RuntimeSnapshot::new(
            0,
            vec![RuntimeWindow {
                id: 7,
                focused: true,
                session_exempt: false,
                active_tab: 0,
                focused_pane_id: Some(pane_id),
                tabs: vec![RuntimeTab {
                    index: 0,
                    active: true,
                    label: "test".into(),
                    kind: "shell".into(),
                    bell: false,
                    focused_pane_id: Some(pane_id),
                    zoomed_pane_id: None,
                    layout: Some(RuntimeLayout::Pane { pane_id }),
                    panes: vec![RuntimePane {
                        id: pane_id,
                        active: true,
                        title: "shell".into(),
                        cwd: "D:/work".into(),
                        branch: "main".into(),
                        ssh_destination: None,
                        running_program: None,
                        agent: None,
                        task_state: RuntimeTaskState::Idle,
                        state_change_seq,
                        active_run: None,
                        last_run: None,
                    }],
                }],
            }],
        )
    }

    #[test]
    fn words_join_into_one_line() {
        assert_eq!(joined(&["fix".to_owned(), "login".to_owned()]), "fix login");
        assert_eq!(joined(&["fix login".to_owned()]), "fix login");
    }

    #[test]
    fn paste_reader_is_strict_utf8_and_bounded() {
        let text =
            read_paste_utf8(std::io::Cursor::new("first\nsecond"), "memory").expect("valid UTF-8");
        assert_eq!(text, "first\nsecond");
        assert!(read_paste_utf8(std::io::Cursor::new([0xff]), "memory").is_err());
        assert!(
            read_paste_utf8(std::io::Cursor::new(vec![b'x'; MAX_PROMPT_BYTES + 1]), "memory",)
                .is_err()
        );
    }

    #[test]
    fn timeout_bounds_are_enforced() {
        assert!(validated_timeout(0).is_err());
        assert!(validated_timeout(MAX_WAIT.as_millis() as u64 + 1).is_err());
        assert!(validated_timeout(1).is_ok());
    }

    #[test]
    fn snapshot_extraction_accepts_direct_and_action_results() {
        let snapshot = snapshot_with_pane(17, 41);
        let direct = serde_json::to_value(&snapshot).expect("serialize snapshot");
        assert_eq!(snapshot_of(&direct), Some(snapshot.clone()));

        let wrapped = json!({ "action": { "pane_id": 17 }, "snapshot": snapshot });
        assert_eq!(snapshot_of(&wrapped).map(|snapshot| snapshot.revision), Some(0));
    }

    #[test]
    fn dispatch_metadata_is_read_from_the_action_envelope() {
        let mut duplicate_panes = snapshot_with_pane(17, 41);
        let mut other_window = duplicate_panes.windows[0].clone();
        other_window.id = 8;
        other_window.tabs[0].panes[0].state_change_seq = 99;
        duplicate_panes.windows.push(other_window);

        let agent = ApiResponse::success(
            "agent.prompt",
            json!({
                "action": {
                    "agent": { "window_id": 7, "pane_id": 17, "generation": 9 }
                },
                "snapshot": duplicate_panes,
            }),
        );
        assert_eq!(dispatched_generation(&agent), Some(9));
        assert_eq!(dispatched_state_change_seq(&agent, None, None), Some(41));

        let pane = ApiResponse::success(
            "pane.prompt",
            json!({
                "action": { "window_id": 7, "pane_id": 17 },
                "snapshot": snapshot_with_pane(17, 42),
            }),
        );
        assert_eq!(dispatched_state_change_seq(&pane, None, None), Some(42));
    }

    #[test]
    fn catalog_examples_match_their_command() {
        // 清单是给模型照抄的。样例若和它声明的命令不一致，抄过去就是错的。
        let catalog = command_catalog();
        for entry in catalog.as_array().expect("catalog is an array") {
            let command = entry["command"].as_str().expect("command");
            let example = entry["example"].as_str().expect("example");
            let mut words = command.split(' ');
            let prefix = words.by_ref().take(2).collect::<Vec<_>>().join(" ");
            // stdin 示例可以在命令前带 producer 与管道；关键是声明的资源命令
            // 确实出现在可复制样例里，而不是强制它位于第一个字节。
            assert!(example.contains(&prefix), "{example:?} should contain {prefix:?}");
            // 动词也要对上——除非它本身就是个 flag：`nebula ctl --help` 指向的是
            // 完整协议，样例给的是其中一条具体调用，不该要求字面相同。
            if let Some(verb) = words.next().filter(|verb| !verb.starts_with('-')) {
                assert!(example.contains(verb), "{example:?} should exercise {verb:?}");
            }
            assert!(!entry["purpose"].as_str().unwrap_or_default().is_empty());
        }
    }

    #[test]
    fn catalog_covers_resource_verbs() {
        let catalog = command_catalog();
        let commands: Vec<&str> = catalog
            .as_array()
            .expect("array")
            .iter()
            .map(|entry| entry["command"].as_str().expect("command"))
            .collect();
        // 两个资源的公共动词都要在清单里。漏一个就等于让模型以为它不存在。
        for verb in ["list", "read", "send", "paste", "wait"] {
            assert!(
                commands.iter().any(|command| *command == format!("nebula pane {verb}")
                    || command.starts_with(&format!("nebula pane {verb} "))),
                "pane {verb} missing from the catalog"
            );
            assert!(
                commands.iter().any(|command| *command == format!("nebula agent {verb}")
                    || command.starts_with(&format!("nebula agent {verb} "))),
                "agent {verb} missing from the catalog"
            );
        }

        for command in [
            "nebula window close <window>",
            "nebula tab close <tab> --window <window>",
            "nebula tab rename <tab> <name> --window <window>",
            "nebula tab move <tab> <to> --window <window>",
            "nebula pane close <pane>",
            "nebula pane zoom <pane> --zoomed <true|false>",
            "nebula pane resize <pane> <ratio>",
            "nebula pane exec <pane> -- <program> [args]",
        ] {
            assert!(commands.contains(&command), "{command} missing from the catalog");
        }
    }
}
