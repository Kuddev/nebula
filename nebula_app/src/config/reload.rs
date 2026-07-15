use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc::{self, Receiver, Sender, TryRecvError};
use std::thread::{self, JoinHandle};

use super::source::ConfigSource;
use super::{LoadedConfig, Result, load_source};

enum WorkerMessage {
    Load(ReloadRequest),
    Shutdown,
}

#[derive(Debug, Clone)]
pub struct ReloadRequest {
    pub request_id: u64,
    pub source: ConfigSource,
}

pub struct ReloadResult {
    pub request_id: u64,
    pub loaded: Result<LoadedConfig>,
}

pub struct ReloadWorker {
    request_tx: Sender<WorkerMessage>,
    result_rx: Receiver<ReloadResult>,
    latest_requested: Arc<AtomicU64>,
    thread: Option<JoinHandle<()>>,
}

impl ReloadWorker {
    pub fn new(notify_ready: impl Fn() + Send + 'static) -> Self {
        Self::with_loader(load_source, notify_ready)
    }

    fn with_loader(
        mut loader: impl FnMut(&ConfigSource) -> Result<LoadedConfig> + Send + 'static,
        notify_ready: impl Fn() + Send + 'static,
    ) -> Self {
        let (request_tx, request_rx) = mpsc::channel();
        let (result_tx, result_rx) = mpsc::channel();
        let latest_requested = Arc::new(AtomicU64::new(0));
        let worker_latest = latest_requested.clone();
        let thread = thread::Builder::new()
            .name("config reload".to_owned())
            .spawn(move || {
                while let Ok(message) = request_rx.recv() {
                    let WorkerMessage::Load(mut request) = message else {
                        break;
                    };

                    // 保存风暴期间只执行队列中最新一次请求，避免旧代次反复覆盖 UI。
                    loop {
                        match request_rx.try_recv() {
                            Ok(WorkerMessage::Load(newer)) => request = newer,
                            Ok(WorkerMessage::Shutdown) => return,
                            Err(TryRecvError::Empty) => break,
                            Err(TryRecvError::Disconnected) => return,
                        }
                    }

                    let loaded = loader(&request.source);
                    if request.request_id != worker_latest.load(Ordering::Acquire) {
                        continue;
                    }
                    if result_tx
                        .send(ReloadResult { request_id: request.request_id, loaded })
                        .is_err()
                    {
                        return;
                    }
                    notify_ready();
                }
            })
            .expect("failed to spawn config reload worker");

        Self { request_tx, result_rx, latest_requested, thread: Some(thread) }
    }

    pub fn request(&self, source: ConfigSource) -> u64 {
        let request_id = self.latest_requested.fetch_add(1, Ordering::AcqRel) + 1;
        let _ = self.request_tx.send(WorkerMessage::Load(ReloadRequest { request_id, source }));
        request_id
    }

    pub fn take_latest(&self) -> Option<ReloadResult> {
        let latest = self.latest_requested.load(Ordering::Acquire);
        let mut selected = None;
        while let Ok(result) = self.result_rx.try_recv() {
            if result.request_id == latest {
                selected = Some(result);
            }
        }
        selected
    }
}

impl Drop for ReloadWorker {
    fn drop(&mut self) {
        let _ = self.request_tx.send(WorkerMessage::Shutdown);
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;
    use std::time::Duration;

    use serde::Deserialize;

    use super::*;
    use crate::config::UiConfig;
    use crate::config::source::ConfigFormat;

    fn loaded(history: usize) -> LoadedConfig {
        let value: toml::Value =
            toml::from_str(&format!("[scrolling]\nhistory={history}")).unwrap();
        LoadedConfig {
            config: UiConfig::deserialize(value).unwrap(),
            source: None,
            lua_generation: None,
        }
    }

    fn source() -> ConfigSource {
        ConfigSource {
            primary_path: PathBuf::from("nebula.lua"),
            format: ConfigFormat::Lua,
            explicit: true,
        }
    }

    #[test]
    fn worker_discards_obsolete_generations() {
        let (ready_tx, ready_rx) = mpsc::channel();
        let worker = ReloadWorker::with_loader(
            |_| {
                std::thread::sleep(Duration::from_millis(20));
                Ok(loaded(777))
            },
            move || {
                let _ = ready_tx.send(());
            },
        );
        worker.request(source());
        worker.request(source());
        let latest = worker.request(source());
        ready_rx.recv_timeout(Duration::from_secs(2)).unwrap();
        let result = worker.take_latest().unwrap();
        assert_eq!(result.request_id, latest);
        assert_eq!(result.loaded.unwrap().config.scrolling.history(), 777);
    }
}
