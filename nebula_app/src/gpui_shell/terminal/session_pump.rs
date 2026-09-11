//! Fair delivery of terminal and connection events to their owning view.
use super::view::TerminalView;
use futures::StreamExt as _;
use gpui::Context;
use nebula_terminal::event::Event as TermEvent;

pub(super) fn attach(
    mut rx: super::event_mailbox::EventReceiver,
    mut stage_rx: futures::channel::mpsc::UnboundedReceiver<crate::ssh_session::SshStage>,
    is_ssh: bool,
    cx: &mut Context<TerminalView>,
) {
    cx.spawn(async move |this, cx| {
        while let Some(event) = rx.next().await {
            // 合并同一批到达的事件，避免每个 Wakeup 都独立触发一帧。
            let mut batch = vec![event];
            while batch.len() < 128 {
                match rx.try_recv() {
                    Ok(event) => batch.push(event),
                    _ => break,
                }
            }
            let full_batch = batch.len() == 128;
            let done = batch.iter().any(|e| matches!(e, TermEvent::Exit));
            if this
                .update(cx, |view: &mut TerminalView, cx| {
                    for event in batch {
                        view.process_event(event, cx);
                    }
                })
                .is_err()
                || done
            {
                break;
            }
            if full_batch {
                cx.background_executor().timer(std::time::Duration::from_millis(1)).await;
            }
        }
    })
    .detach();
    if is_ssh {
        // SSH 连接阶段泵：横幅数据源（与旧壳连接卡片同一
        // 上报流，350ms 门槛之类的视觉策略交给渲染端）。
        cx.spawn(async move |this, cx| {
            while let Some(stage) = stage_rx.next().await {
                if this
                    .update(cx, |view: &mut TerminalView, cx| {
                        view.apply_ssh_stage(stage, cx);
                    })
                    .is_err()
                {
                    break;
                }
            }
        })
        .detach();
    }
}
