//! An in-memory filesystem behind the real SFTP codec, with deterministic I/O faults.

use crate::ssh_sftp::document::DocumentOperation;
use russh_sftp::protocol::{
    Attrs, Data, File, FileAttributes, Handle, Name, OpenFlags, Status, StatusCode,
};
use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};

#[derive(Clone)]
pub(super) struct Node {
    pub data: Vec<u8>,
    pub permissions: u32,
    pub uid: u32,
    pub gid: u32,
    pub modified: u32,
}

impl Node {
    pub(super) fn new(data: &[u8]) -> Self {
        Self { data: data.to_vec(), permissions: 0o100640, uid: 1001, gid: 1002, modified: 10 }
    }

    fn attributes(&self) -> FileAttributes {
        FileAttributes {
            size: Some(self.data.len() as u64),
            permissions: Some(self.permissions),
            uid: Some(self.uid),
            gid: Some(self.gid),
            mtime: Some(self.modified),
            ..FileAttributes::empty()
        }
    }
}

#[derive(Default)]
pub(super) struct Store {
    pub files: BTreeMap<String, Node>,
    pub aliases: BTreeMap<String, String>,
    pub fail_writes: bool,
    pub fail_publish_once: bool,
    pub edit_before_backup: Option<Vec<u8>>,
    pub create_after_backup: Option<Vec<u8>>,
    pub cancel_after_backup: Option<DocumentOperation>,
}

struct Peer {
    store: Arc<Mutex<Store>>,
    handles: BTreeMap<String, String>,
}

fn success(id: u32) -> Status {
    Status {
        id,
        status_code: StatusCode::Ok,
        error_message: String::new(),
        language_tag: String::new(),
    }
}

impl Peer {
    fn attributes(&self, path: &str) -> Result<FileAttributes, StatusCode> {
        let store = self.store.lock().unwrap();
        let path = store.aliases.get(path).map(String::as_str).unwrap_or(path);
        store.files.get(path).map(Node::attributes).ok_or(StatusCode::NoSuchFile)
    }
}

impl russh_sftp::server::Handler for Peer {
    type Error = StatusCode;
    fn unimplemented(&self) -> Self::Error {
        StatusCode::OpUnsupported
    }

    async fn realpath(&mut self, id: u32, path: String) -> Result<Name, StatusCode> {
        let path = self.store.lock().unwrap().aliases.get(&path).cloned().unwrap_or(path);
        Ok(Name { id, files: vec![File::dummy(path)] })
    }

    async fn stat(&mut self, id: u32, path: String) -> Result<Attrs, StatusCode> {
        Ok(Attrs { id, attrs: self.attributes(&path)? })
    }

    async fn lstat(&mut self, id: u32, path: String) -> Result<Attrs, StatusCode> {
        self.stat(id, path).await
    }

    async fn fstat(&mut self, id: u32, handle: String) -> Result<Attrs, StatusCode> {
        let path = self.handles.get(&handle).ok_or(StatusCode::NoSuchFile)?.clone();
        self.stat(id, path).await
    }

    async fn open(
        &mut self,
        id: u32,
        filename: String,
        flags: OpenFlags,
        _: FileAttributes,
    ) -> Result<Handle, StatusCode> {
        let mut store = self.store.lock().unwrap();
        if flags.contains(OpenFlags::CREATE) {
            if flags.contains(OpenFlags::EXCLUDE) && store.files.contains_key(&filename) {
                return Err(StatusCode::Failure);
            }
            store.files.entry(filename.clone()).or_insert_with(|| Node {
                uid: 0,
                gid: 0,
                ..Node::new(&[])
            });
        }
        if !store.files.contains_key(&filename) {
            return Err(StatusCode::NoSuchFile);
        }
        let handle = format!("handle-{id}");
        self.handles.insert(handle.clone(), filename);
        Ok(Handle { id, handle })
    }

    async fn read(
        &mut self,
        id: u32,
        handle: String,
        offset: u64,
        length: u32,
    ) -> Result<Data, StatusCode> {
        let path = self.handles.get(&handle).ok_or(StatusCode::NoSuchFile)?;
        let store = self.store.lock().unwrap();
        let node = store.files.get(path).ok_or(StatusCode::NoSuchFile)?;
        let offset = usize::try_from(offset).map_err(|_| StatusCode::Failure)?;
        if offset >= node.data.len() {
            return Err(StatusCode::Eof);
        }
        let end = node.data.len().min(offset + length as usize);
        Ok(Data { id, data: node.data[offset..end].to_vec() })
    }

    async fn write(
        &mut self,
        id: u32,
        handle: String,
        offset: u64,
        data: Vec<u8>,
    ) -> Result<Status, StatusCode> {
        let path = self.handles.get(&handle).ok_or(StatusCode::NoSuchFile)?;
        let mut store = self.store.lock().unwrap();
        if store.fail_writes {
            return Err(StatusCode::PermissionDenied);
        }
        let node = store.files.get_mut(path).ok_or(StatusCode::NoSuchFile)?;
        let start = usize::try_from(offset).map_err(|_| StatusCode::Failure)?;
        let end = start
            .checked_add(data.len())
            .filter(|end| *end <= 16 * 1024 * 1024)
            .ok_or(StatusCode::Failure)?;
        if end > node.data.len() {
            node.data.resize(end, 0);
        }
        node.data[start..end].copy_from_slice(&data);
        node.modified = 20;
        Ok(success(id))
    }

    async fn close(&mut self, id: u32, handle: String) -> Result<Status, StatusCode> {
        self.handles.remove(&handle);
        Ok(success(id))
    }

    async fn setstat(
        &mut self,
        id: u32,
        path: String,
        attrs: FileAttributes,
    ) -> Result<Status, StatusCode> {
        let mut store = self.store.lock().unwrap();
        let node = store.files.get_mut(&path).ok_or(StatusCode::NoSuchFile)?;
        if let Some(value) = attrs.permissions {
            node.permissions = value;
        }
        if let Some(value) = attrs.uid {
            node.uid = value;
        }
        if let Some(value) = attrs.gid {
            node.gid = value;
        }
        if let Some(value) = attrs.mtime {
            node.modified = value;
        }
        Ok(success(id))
    }

    async fn rename(&mut self, id: u32, old: String, new: String) -> Result<Status, StatusCode> {
        let mut store = self.store.lock().unwrap();
        if old.contains("nebula-upload") && new == "/config" && store.fail_publish_once {
            store.fail_publish_once = false;
            return Err(StatusCode::Failure);
        }
        if store.files.contains_key(&new) {
            return Err(StatusCode::Failure);
        }
        let backup = old == "/config" && new.contains("nebula-backup");
        if backup && let Some(data) = store.edit_before_backup.take() {
            store.files.get_mut(&old).unwrap().data = data;
        }
        let node = store.files.remove(&old).ok_or(StatusCode::NoSuchFile)?;
        store.files.insert(new, node);
        if backup {
            if let Some(data) = store.create_after_backup.take() {
                store.files.insert(old, Node::new(&data));
            }
            if let Some(operation) = store.cancel_after_backup.take() {
                operation.cancel();
            }
        }
        Ok(success(id))
    }

    async fn remove(&mut self, id: u32, path: String) -> Result<Status, StatusCode> {
        self.store.lock().unwrap().files.remove(&path).ok_or(StatusCode::NoSuchFile)?;
        Ok(success(id))
    }
}

pub(super) async fn connect(
    data: &[u8],
) -> (Arc<Mutex<Store>>, Arc<russh_sftp::client::SftpSession>) {
    let store = Arc::new(Mutex::new(Store::default()));
    store.lock().unwrap().files.insert("/config".into(), Node::new(data));
    let (client, server) = tokio::io::duplex(128 * 1024);
    tokio::spawn(russh_sftp::server::run(
        server,
        Peer { store: store.clone(), handles: BTreeMap::new() },
    ));
    let session = russh_sftp::client::SftpSession::new_with_config(
        client,
        crate::ssh_sftp::limits::session_config(),
    )
    .await
    .unwrap();
    (store, Arc::new(session))
}
