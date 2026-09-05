use std::io;
use std::sync::{Arc, Mutex, OnceLock};
use std::time::Duration;

use tokio::sync::oneshot;
use zeroize::Zeroizing;

pub enum PromptKind {
    HostKey { host: String, port: u16, fingerprint: String },
    Secret { label: String, allow_save: bool },
}

pub enum PromptResponse {
    Trust,
    Secret { value: Zeroizing<Vec<u8>>, save: bool },
    Cancel,
}

pub struct Prompt {
    pub kind: PromptKind,
    reply: Mutex<Option<oneshot::Sender<PromptResponse>>>,
}

impl Prompt {
    fn channel(kind: PromptKind) -> (Arc<Self>, oneshot::Receiver<PromptResponse>) {
        let (sender, receiver) = oneshot::channel();
        (Arc::new(Self { kind, reply: Mutex::new(Some(sender)) }), receiver)
    }

    #[cfg(test)]
    pub(crate) fn for_test(kind: PromptKind) -> (Arc<Self>, oneshot::Receiver<PromptResponse>) {
        Self::channel(kind)
    }

    pub fn respond(&self, response: PromptResponse) -> bool {
        let response = match (&self.kind, response) {
            (PromptKind::HostKey { .. }, PromptResponse::Trust) => PromptResponse::Trust,
            (PromptKind::Secret { allow_save, .. }, PromptResponse::Secret { value, save }) => {
                PromptResponse::Secret { value, save: save && *allow_save }
            },
            _ => PromptResponse::Cancel,
        };
        self.reply
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .take()
            .is_some_and(|sender| sender.send(response).is_ok())
    }

    pub fn is_pending(&self) -> bool {
        self.reply
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .as_ref()
            .is_some_and(|sender| !sender.is_closed())
    }
}

type Dispatcher = Box<dyn Fn(Arc<Prompt>) -> bool + Send + Sync>;
static DISPATCHER: OnceLock<Dispatcher> = OnceLock::new();
static PROMPT_LOCK: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

pub fn install(dispatch: impl Fn(Arc<Prompt>) -> bool + Send + Sync + 'static) {
    let _ = DISPATCHER.set(Box::new(dispatch));
}

pub fn available() -> bool {
    DISPATCHER.get().is_some()
}

async fn request(kind: PromptKind) -> io::Result<PromptResponse> {
    let dispatch = DISPATCHER.get().ok_or_else(|| {
        io::Error::new(io::ErrorKind::NotConnected, "SSH user interface unavailable")
    })?;
    let _serial = PROMPT_LOCK.lock().await;
    request_with(kind, |prompt| dispatch(prompt), Duration::from_secs(300)).await
}

async fn request_with(
    kind: PromptKind,
    dispatch: impl FnOnce(Arc<Prompt>) -> bool,
    timeout: Duration,
) -> io::Result<PromptResponse> {
    let (prompt, receiver) = Prompt::channel(kind);
    if !dispatch(prompt) {
        return Err(io::Error::new(io::ErrorKind::NotConnected, "SSH window is closed"));
    }
    match tokio::time::timeout(timeout, receiver).await {
        Ok(Ok(response)) => Ok(response),
        Ok(Err(_)) => Ok(PromptResponse::Cancel),
        Err(_) => Err(io::Error::new(io::ErrorKind::TimedOut, "SSH confirmation timed out")),
    }
}

pub async fn confirm_host(host: &str, port: u16, fingerprint: String) -> io::Result<bool> {
    let response =
        request(PromptKind::HostKey { host: host.to_owned(), port, fingerprint }).await?;
    Ok(matches!(response, PromptResponse::Trust))
}

pub async fn secret(
    label: String,
    allow_save: bool,
) -> io::Result<Option<(Zeroizing<Vec<u8>>, bool)>> {
    match request(PromptKind::Secret { label, allow_save }).await? {
        PromptResponse::Secret { value, save } => Ok(Some((value, save && allow_save))),
        _ => Ok(None),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn keyboard_interactive_secrets_cannot_request_storage() {
        let (prompt, mut receiver) = Prompt::channel(PromptKind::Secret {
            label: "Verification code".into(),
            allow_save: false,
        });
        prompt.respond(PromptResponse::Secret {
            value: Zeroizing::new(b"123456".to_vec()),
            save: true,
        });
        assert!(matches!(receiver.try_recv(), Ok(PromptResponse::Secret { save: false, .. })));
    }

    #[test]
    fn a_secret_response_cannot_trust_a_host() {
        let (prompt, mut receiver) = Prompt::channel(PromptKind::HostKey {
            host: "example.test".into(),
            port: 22,
            fingerprint: "SHA256:test".into(),
        });
        prompt.respond(PromptResponse::Secret { value: Zeroizing::new(Vec::new()), save: true });
        assert!(matches!(receiver.try_recv(), Ok(PromptResponse::Cancel)));
    }

    #[test]
    fn absent_window_or_expired_prompt_never_succeeds() {
        let runtime = tokio::runtime::Builder::new_current_thread().enable_time().build().unwrap();
        runtime.block_on(async {
            let kind = || PromptKind::Secret { label: "Password".into(), allow_save: false };
            let response = request_with(kind(), |_| false, Duration::from_millis(1)).await;
            assert!(matches!(response, Err(error) if error.kind() == io::ErrorKind::NotConnected));
            let mut pending = None;
            let response = request_with(
                kind(),
                |prompt| {
                    pending = Some(prompt);
                    true
                },
                Duration::from_millis(1),
            )
            .await;
            assert!(matches!(response, Err(error) if error.kind() == io::ErrorKind::TimedOut));
            assert!(!pending.unwrap().is_pending());
        });
    }

    #[test]
    fn confirmation_is_one_shot_and_cancel_is_not_trust() {
        let (sender, mut receiver) = oneshot::channel();
        let prompt = Prompt {
            kind: PromptKind::HostKey {
                host: "example.test".into(),
                port: 22,
                fingerprint: "SHA256:test".into(),
            },
            reply: Mutex::new(Some(sender)),
        };
        assert!(prompt.is_pending());
        assert!(prompt.respond(PromptResponse::Cancel));
        assert!(!prompt.respond(PromptResponse::Trust));
        assert!(matches!(receiver.try_recv(), Ok(PromptResponse::Cancel)));
        assert!(!prompt.is_pending());
    }

    #[test]
    fn expired_request_cannot_accept_credentials() {
        let (sender, receiver) = oneshot::channel();
        let prompt = Prompt {
            kind: PromptKind::Secret { label: "Password".into(), allow_save: false },
            reply: Mutex::new(Some(sender)),
        };
        drop(receiver);
        assert!(!prompt.is_pending());
        assert!(!prompt.respond(PromptResponse::Trust));
    }
}
