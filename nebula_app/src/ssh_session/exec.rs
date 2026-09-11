//! Short-lived remote probes share the shell channel's close-on-drop ownership.
use super::{SessionError, lifecycle};
use russh::{Channel, ChannelMsg, client};
use std::time::Duration;

pub(super) async fn capture(
    channel: Channel<client::Msg>,
    command: &str,
    script: &[u8],
    budget: Duration,
    raw_destination: &str,
) -> Result<String, SessionError> {
    let mut channel = lifecycle::own_channel(channel);
    channel.exec(true, command).await?;
    if !script.is_empty() {
        channel.data_bytes(script.to_vec()).await?;
        // 不发 EOF 的话远端 `sh` 会一直等更多输入，命令永远不结束。
        channel.eof().await?;
    }

    let collect = async {
        let mut stdout = Vec::new();
        while let Some(message) = channel.wait().await {
            match message {
                ChannelMsg::Data { data } => stdout.extend_from_slice(&data),
                // 标准错误只当诊断线索，不混进结果——远端的 `ps: not found`
                // 之类抱怨不该被当成路径。
                ChannelMsg::ExtendedData { data, .. } => {
                    if let Ok(text) = std::str::from_utf8(&data) {
                        let text = text.trim();
                        if !text.is_empty() {
                            log::debug!("远端命令 stderr（{raw_destination}）: {text}");
                        }
                    }
                },
                ChannelMsg::Eof | ChannelMsg::Close => break,
                _ => {},
            }
        }
        stdout
    };

    let result = tokio::time::timeout(budget, collect).await;
    let closed = channel.finish().await;
    match result {
        // 远端文件名和路径未必是合法 UTF-8。有损转换让"大部分能读"胜过
        // "整次探测失败"；真正需要字节精度的路径操作走 SFTP，不走这里。
        Ok(stdout) => {
            closed?;
            Ok(String::from_utf8_lossy(&stdout).into_owned())
        },
        Err(_) => Err(format!("远端命令超过 {} 秒未返回", budget.as_secs()).into()),
    }
}
