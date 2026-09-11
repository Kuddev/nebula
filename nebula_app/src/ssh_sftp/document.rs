//! Remote documents pin an authenticated SFTP channel and keep bounded text snapshots.

use super::*;
use crate::text_document::{MAX_BYTES, SaveError, TextSnapshot};
use tokio::io::AsyncReadExt as _;

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct RemoteLocation {
    pub destination: String,
    pub path: String,
}

impl RemoteLocation {
    pub(crate) fn display(&self) -> String {
        format!("ssh://{}{}", self.destination, self.path)
    }
}

#[derive(Clone, Default)]
pub(crate) struct DocumentOperation {
    control: TaskControl,
    wake: Arc<tokio::sync::Notify>,
}

impl DocumentOperation {
    pub(crate) fn cancel(&self) {
        self.control.fetch_or(CANCEL_REQUESTED, Ordering::AcqRel);
        self.wake.notify_one();
    }

    async fn cancelled(&self) {
        loop {
            if self.control.load(Ordering::Acquire) & CANCEL_REQUESTED != 0 {
                return;
            }
            self.wake.notified().await;
        }
    }

    fn context(&self, location: &RemoteLocation) -> TaskContext {
        TaskContext {
            state: Arc::new(Mutex::new(SftpSnapshot {
                destination: location.destination.clone(),
                path: location.path.clone(),
                entries: vec![],
                phase: SftpPhase::Working,
                error: None,
                progress: None,
            })),
            task_control: self.control.clone(),
            generation: Arc::new(AtomicU64::new(1)),
            task_generation: 1,
            wake: Arc::new(|| {}),
            last_wake: Arc::new(Mutex::new(Instant::now())),
        }
    }
}

#[derive(Clone)]
pub(crate) struct RemoteDocument {
    pub location: RemoteLocation,
    pub snapshot: TextSnapshot,
    pub modified: Option<std::time::SystemTime>,
    target: String,
    metadata: russh_sftp::protocol::FileAttributes,
    // Reconnecting through a newly edited SSH alias must never redirect a draft.
    session: Arc<SftpSession>,
}

impl RemoteDocument {
    pub(crate) fn size(&self) -> u64 {
        self.metadata.len()
    }

    pub(crate) async fn load(
        location: RemoteLocation,
        operation: DocumentOperation,
    ) -> io::Result<Self> {
        let load = async {
            let sftp = Arc::new(
                crate::ssh_session::open_sftp(&location.destination)
                    .await
                    .map_err(io::Error::other)?,
            );
            Self::load_from_session(location, sftp, &operation).await
        };
        tokio::select! {
            result = tokio::time::timeout(Duration::from_secs(45), load) => result.map_err(|_| io::Error::new(io::ErrorKind::TimedOut, "Remote document read timed out"))?,
            _ = operation.cancelled() => Err(io::Error::new(io::ErrorKind::Interrupted, "Remote document read cancelled")),
        }
    }

    async fn load_from_session(
        location: RemoteLocation,
        session: Arc<SftpSession>,
        operation: &DocumentOperation,
    ) -> io::Result<Self> {
        let context = operation.context(&location);
        context.check_cancelled().map_err(io::Error::other)?;
        let target = session.canonicalize(location.path.clone()).await.map_err(io::Error::other)?;
        let metadata = session.metadata(target.clone()).await.map_err(io::Error::other)?;
        if !metadata.is_regular() {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "The remote item is not a regular file",
            ));
        }
        let bytes =
            read_bounded(&session, &target, Some(&context)).await.map_err(io::Error::other)?;
        let after = session.metadata(target.clone()).await.map_err(io::Error::other)?;
        if metadata.size != after.size
            || metadata.mtime != after.mtime
            || metadata.permissions != after.permissions
        {
            return Err(io::Error::other(SaveError::Changed));
        }
        let modified = metadata
            .mtime
            .map(|value| std::time::UNIX_EPOCH + Duration::from_secs(u64::from(value)));
        Ok(Self {
            location,
            snapshot: TextSnapshot::decode(bytes, false),
            modified,
            target,
            metadata,
            session,
        })
    }

    pub(crate) async fn save(
        &self,
        text: String,
        operation: DocumentOperation,
    ) -> Result<Self, SaveError> {
        let bytes = self.snapshot.encode(&text)?;
        let context = operation.context(&self.location);
        context.check_cancelled().map_err(map_error)?;
        let expected = transaction::RemotePrecondition {
            requested: self.location.path.clone(),
            canonical: self.target.clone(),
            bytes: self.snapshot.bytes.clone(),
            metadata: self.metadata.clone(),
        };
        if text == self.snapshot.text {
            transaction::verify_remote_precondition(&self.session, &expected, &context)
                .await
                .map_err(map_error)?;
            return Ok(self.clone());
        }
        let staged = tempfile::NamedTempFile::new()?;
        tokio::fs::write(staged.path(), &bytes).await?;
        let stamp = transaction::FileStamp {
            len: bytes.len() as u64,
            modified: None,
            permissions: self.metadata.permissions,
        };
        // Do not abort the future during publication. The shared transaction
        // observes cancellation before publication and finishes/rolls back after it.
        transaction::upload_file_checked(
            &self.session,
            staged.path(),
            &self.target,
            stamp,
            false,
            &context,
            Some(&expected),
        )
        .await
        .map_err(map_error)?;
        let metadata = self.session.metadata(self.target.clone()).await.unwrap_or_else(|_| {
            let mut metadata = self.metadata.clone();
            metadata.size = Some(bytes.len() as u64);
            metadata.mtime = None;
            metadata
        });
        let modified = metadata
            .mtime
            .map(|value| std::time::UNIX_EPOCH + Duration::from_secs(u64::from(value)));
        Ok(Self {
            snapshot: TextSnapshot::decode(bytes, false),
            metadata,
            modified,
            ..self.clone()
        })
    }
}

fn map_error(error: SftpError) -> SaveError {
    match error.downcast::<SaveError>() {
        Ok(error) => *error,
        Err(error) => match error.downcast::<io::Error>() {
            Ok(error) => SaveError::Io(*error),
            Err(error) => SaveError::Io(io::Error::other(error)),
        },
    }
}

pub(super) async fn read_bounded(
    sftp: &SftpSession,
    path: &str,
    context: Option<&TaskContext>,
) -> SftpResult<Vec<u8>> {
    let mut file = sftp.open(path.to_owned()).await?;
    let mut bytes = Vec::new();
    let mut buffer = [0u8; 65536];
    while bytes.len() <= MAX_BYTES {
        if let Some(context) = context {
            context.check_cancelled()?;
        }
        let limit = buffer.len().min(MAX_BYTES + 1 - bytes.len());
        let count = file.read(&mut buffer[..limit]).await?;
        if count == 0 {
            break;
        }
        bytes.extend_from_slice(&buffer[..count]);
    }
    Ok(bytes)
}

#[cfg(test)]
#[path = "document/tests.rs"]
mod tests;
