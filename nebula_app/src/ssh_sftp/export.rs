//! A drag owns its download plan and temporary directory until every consumer releases it.

use super::*;
use std::sync::OnceLock;

#[derive(Clone, Debug)]
pub(crate) struct ExportFile {
    pub relative_path: PathBuf,
    pub directory: bool,
    pub size: u64,
}

pub(crate) struct PreparedExport {
    pub files: Vec<ExportFile>,
    directory: Arc<tempfile::TempDir>,
    plan: Mutex<Option<DownloadPlan>>,
    destination: String,
    context: TaskContext,
    materialized: OnceLock<Result<(), String>>,
    completion: Mutex<Option<tokio::sync::oneshot::Sender<Result<(), String>>>>,
}

impl PreparedExport {
    /// Only the native worker calls this blocking bridge. Directory entries are
    /// downloaded once, even if Explorer requests or clones several streams.
    pub(crate) fn materialize(
        &self,
        index: usize,
    ) -> io::Result<(PathBuf, Arc<tempfile::TempDir>)> {
        let file = self.files.get(index).filter(|f| !f.directory).ok_or_else(|| {
            io::Error::new(io::ErrorKind::InvalidInput, "Invalid drag file index")
        })?;
        let result = self.materialized.get_or_init(|| {
            let runtime = crate::ssh_session::runtime().map_err(|e| e.to_string())?;
            let plan =
                lock(&self.plan).take().ok_or("The drag download plan was already consumed")?;
            runtime
                .block_on(async {
                    self.context.check_cancelled()?;
                    let sftp = crate::ssh_session::open_sftp(&self.destination).await?;
                    for file in &plan.files {
                        self.context.check_cancelled()?;
                        let metadata = sftp.metadata(file.remote.clone()).await?;
                        if !metadata.is_regular()
                            || metadata.len() != file.size
                            || (file.modified != 0
                                && u64::from(metadata.mtime.unwrap_or(0)) != file.modified)
                        {
                            return Err(
                                io::Error::other("Remote file changed; start a new drag").into()
                            );
                        }
                    }
                    execute_download_plan(&sftp, plan, false, &self.context).await
                })
                .map_err(|e: SftpError| e.to_string())
        });
        result.as_ref().map_err(|e| io::Error::other(e.clone()))?;
        self.context.check_cancelled().map_err(|e| io::Error::other(e.to_string()))?;
        Ok((self.directory.path().join(&file.relative_path), self.directory.clone()))
    }

    pub(crate) fn finish(&self, result: Result<(), String>) {
        if let Some(done) = lock(&self.completion).take() {
            let _ = done.send(result);
        }
    }
}

impl Drop for PreparedExport {
    fn drop(&mut self) {
        self.finish(Err("File drag cancelled".to_owned()));
    }
}

impl SftpController {
    pub(crate) fn prepare_export(
        &self,
        entry: SftpEntry,
        source_entries: Vec<SftpEntry>,
    ) -> tokio::sync::oneshot::Receiver<Result<Arc<PreparedExport>, String>> {
        let snapshot = self.snapshot();
        let (ready, receive) = tokio::sync::oneshot::channel();
        self.start_job(
            SftpPhase::Working,
            Some(TransferProgress::new(entry.name.clone(), 0)),
            move |context| async move {
                let prepare = async {
                    context.check_cancelled()?;
                    let directory =
                        Arc::new(tempfile::Builder::new().prefix("pebrel-drag-").tempdir()?);
                    let sftp = crate::ssh_session::open_sftp(&snapshot.destination).await?;
                    let plan =
                        build_download_plan(&sftp, entry, directory.path().to_owned(), &context)
                            .await?;
                    context.set_total(plan.total);
                    let files = export_manifest(&plan, directory.path())?;
                    let (done, wait) = tokio::sync::oneshot::channel();
                    let prepared = Arc::new(PreparedExport {
                        files,
                        directory,
                        plan: Mutex::new(Some(plan)),
                        destination: snapshot.destination.clone(),
                        context: context.clone(),
                        materialized: OnceLock::new(),
                        completion: Mutex::new(Some(done)),
                    });
                    Ok::<_, SftpError>((prepared, wait, sftp))
                }
                .await;
                let (prepared, done, sftp) = match prepare {
                    Ok(value) => value,
                    Err(error) => {
                        let message = error.to_string();
                        let _ = ready.send(Err(message.clone()));
                        return Err(io::Error::other(message).into());
                    },
                };
                if ready.send(Ok(prepared)).is_err() {
                    return Err(io::Error::other("File drag cancelled").into());
                }
                done.await
                    .map_err(|_| io::Error::other("File drag ended"))?
                    .map_err(io::Error::other)?;
                // A completed download does not depend on another network round-trip.
                // Preserve the captured source listing when the source is still visible.
                drop(sftp);
                Ok((snapshot.path, source_entries))
            },
        );
        receive
    }
}

fn export_manifest(plan: &DownloadPlan, root: &Path) -> io::Result<Vec<ExportFile>> {
    let relative = |path: &Path| -> io::Result<PathBuf> {
        path.strip_prefix(root).map(Path::to_owned).map_err(|_| {
            io::Error::new(io::ErrorKind::InvalidData, "Drag path escaped its temporary directory")
        })
    };
    let mut files = Vec::with_capacity(plan.directories.len() + plan.files.len());
    for directory in &plan.directories {
        files.push(ExportFile { relative_path: relative(directory)?, directory: true, size: 0 });
    }
    for file in &plan.files {
        files.push(ExportFile {
            relative_path: relative(&file.local)?,
            directory: false,
            size: file.size,
        });
    }
    Ok(files)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn export_keeps_empty_directories_and_relative_tree_identity() {
        let root = PathBuf::from("staging");
        let plan = DownloadPlan {
            directories: vec![root.join("site"), root.join("site/empty")],
            files: vec![DownloadFile {
                remote: "/site/a.txt".into(),
                local: root.join("site/a.txt"),
                size: 7,
                modified: 0,
            }],
            total: 7,
        };
        let files = export_manifest(&plan, &root).unwrap();
        assert_eq!(files.len(), 3);
        assert!(files[1].directory);
        assert_eq!(files[1].relative_path, PathBuf::from("site/empty"));
        assert_eq!(files[2].size, 7);
    }

    #[test]
    fn export_rejects_files_outside_its_owned_directory() {
        let plan =
            DownloadPlan { directories: vec![PathBuf::from("elsewhere")], files: vec![], total: 0 };
        assert!(export_manifest(&plan, Path::new("staging")).is_err());
    }
}
