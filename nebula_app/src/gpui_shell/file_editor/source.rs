//! Local and remote adapters share the editor's buffer and save lifecycle.

use super::document::Document;
use crate::ssh_sftp::document::{DocumentOperation, RemoteDocument, RemoteLocation};
use crate::text_document::{SaveError, TextSnapshot};
use std::io;
use std::path::{Path, PathBuf};

#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) enum DocumentSource {
    Local(PathBuf),
    Remote(RemoteLocation),
}

impl DocumentSource {
    pub(super) fn path(&self) -> &Path {
        match self {
            Self::Local(path) => path,
            Self::Remote(location) => Path::new(&location.path),
        }
    }

    pub(super) fn display(&self) -> String {
        match self {
            Self::Local(path) => path.display().to_string(),
            Self::Remote(location) => location.display(),
        }
    }

    pub(super) fn is_remote(&self) -> bool {
        matches!(self, Self::Remote(_))
    }

    pub(super) async fn load(self, operation: DocumentOperation) -> io::Result<LoadedDocument> {
        match self {
            Self::Local(path) => Document::load(&path).map(LoadedDocument::Local),
            Self::Remote(location) => on_network(async move {
                RemoteDocument::load(location, operation)
                    .await
                    .map(LoadedDocument::Remote)
                    .map_err(SaveError::Io)
            })
            .await
            .map_err(|error| match error {
                SaveError::Io(error) => error,
                error => io::Error::other(error),
            }),
        }
    }
}

#[derive(Clone)]
pub(super) enum LoadedDocument {
    Local(Document),
    Remote(RemoteDocument),
}

impl std::ops::Deref for LoadedDocument {
    type Target = TextSnapshot;
    fn deref(&self) -> &Self::Target {
        match self {
            Self::Local(document) => document,
            Self::Remote(document) => &document.snapshot,
        }
    }
}

impl LoadedDocument {
    pub(super) fn modified(&self) -> Option<std::time::SystemTime> {
        match self {
            Self::Local(document) => document.modified,
            Self::Remote(document) => document.modified,
        }
    }

    pub(super) fn size(&self) -> u64 {
        match self {
            Self::Local(document) => document.total_bytes,
            Self::Remote(document) => document.size(),
        }
    }

    pub(super) async fn save(
        self,
        source: DocumentSource,
        text: String,
        operation: DocumentOperation,
    ) -> Result<Self, SaveError> {
        match (self, source) {
            (Self::Local(document), DocumentSource::Local(path)) => {
                document.save(&path, text).map(Self::Local)
            },
            (Self::Remote(document), DocumentSource::Remote(location))
                if document.location == location =>
            {
                on_network(async move { document.save(text, operation).await.map(Self::Remote) })
                    .await
            },
            _ => Err(SaveError::Changed),
        }
    }
}

async fn on_network<T: Send + 'static>(
    future: impl std::future::Future<Output = Result<T, SaveError>> + Send + 'static,
) -> Result<T, SaveError> {
    let (send, receive) = tokio::sync::oneshot::channel();
    crate::ssh_session::runtime()?.spawn(async move {
        let _ = send.send(future.await);
    });
    receive.await.map_err(|_| io::Error::other("Remote document operation ended"))?
}
