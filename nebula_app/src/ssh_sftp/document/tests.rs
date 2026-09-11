use super::*;
#[path = "tests/peer.rs"]
mod peer;

fn run(future: impl std::future::Future<Output = ()>) {
    tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .enable_all()
        .build()
        .unwrap()
        .block_on(future);
}

async fn open(session: Arc<SftpSession>) -> RemoteDocument {
    RemoteDocument::load_from_session(
        RemoteLocation { destination: "test-host".into(), path: "/config".into() },
        session,
        &DocumentOperation::default(),
    )
    .await
    .unwrap()
}

#[test]
fn remote_save_preserves_bom_crlf_permissions_and_ownership() {
    run(async {
        let (store, session) = peer::connect("\u{feff}原文\r\n".as_bytes()).await;
        let document = open(session).await;
        let saved = document.save("修改\n".into(), DocumentOperation::default()).await.unwrap();
        assert_eq!(saved.snapshot.text, "修改\n");
        let store = store.lock().unwrap();
        assert_eq!(store.files["/config"].data, "\u{feff}修改\r\n".as_bytes());
        assert_eq!(store.files["/config"].permissions, 0o100640);
        assert_eq!((store.files["/config"].uid, store.files["/config"].gid), (1001, 1002));
        assert_eq!(store.files.len(), 1);
    });
}

#[test]
fn same_size_same_timestamp_external_change_is_never_overwritten() {
    run(async {
        let (store, session) = peer::connect(b"first").await;
        let document = open(session).await;
        store.lock().unwrap().files.get_mut("/config").unwrap().data = b"other".to_vec();
        assert!(matches!(
            document.save("draft".into(), DocumentOperation::default()).await,
            Err(SaveError::Changed)
        ));
        assert_eq!(store.lock().unwrap().files["/config"].data, b"other");
    });
}

#[test]
fn a_change_in_the_rename_gap_is_restored_without_losing_external_data() {
    run(async {
        let (store, session) = peer::connect(b"first").await;
        let document = open(session).await;
        store.lock().unwrap().edit_before_backup = Some(b"other".to_vec());
        assert!(matches!(
            document.save("draft".into(), DocumentOperation::default()).await,
            Err(SaveError::Changed)
        ));
        let store = store.lock().unwrap();
        assert_eq!(store.files["/config"].data, b"other");
        assert_eq!(store.files.len(), 1);
    });
}

#[test]
fn failed_publication_restores_the_original_file() {
    run(async {
        let (store, session) = peer::connect(b"original").await;
        let document = open(session).await;
        store.lock().unwrap().fail_publish_once = true;
        assert!(document.save("draft".into(), DocumentOperation::default()).await.is_err());
        let store = store.lock().unwrap();
        assert_eq!(store.files["/config"].data, b"original");
        assert_eq!(store.files.len(), 1);
    });
}

#[test]
fn rollback_never_overwrites_a_new_destination_created_by_another_writer() {
    run(async {
        let (store, session) = peer::connect(b"original").await;
        let document = open(session).await;
        store.lock().unwrap().create_after_backup = Some(b"external".to_vec());
        let result = document.save("draft".into(), DocumentOperation::default()).await;
        assert!(result.is_err());
        let store = store.lock().unwrap();
        assert_eq!(store.files["/config"].data, b"external");
        assert!(
            store
                .files
                .iter()
                .any(|(path, node)| path.contains("nebula-backup") && node.data == b"original")
        );
        assert!(
            store
                .files
                .iter()
                .any(|(path, node)| path.contains("nebula-upload") && node.data == b"draft")
        );
    });
}

#[test]
fn permission_failure_and_prepublication_cancellation_leave_original_intact() {
    run(async {
        let (store, session) = peer::connect(b"original").await;
        let document = open(session).await;
        let cancelled = DocumentOperation::default();
        cancelled.cancel();
        assert!(document.save("draft".into(), cancelled).await.is_err());
        store.lock().unwrap().fail_writes = true;
        assert!(document.save("draft".into(), DocumentOperation::default()).await.is_err());
        let store = store.lock().unwrap();
        assert_eq!(store.files["/config"].data, b"original");
        assert_eq!(store.files.len(), 1);
    });
}

#[test]
fn cancellation_after_publication_starts_finishes_the_file_transaction() {
    run(async {
        let (store, session) = peer::connect(b"original").await;
        let document = open(session).await;
        let operation = DocumentOperation::default();
        store.lock().unwrap().cancel_after_backup = Some(operation.clone());
        assert!(document.save("draft".into(), operation).await.is_ok());
        let store = store.lock().unwrap();
        assert_eq!(store.files["/config"].data, b"draft");
        assert_eq!(store.files.len(), 1);
    });
}

#[test]
fn retargeted_symlinks_cannot_redirect_an_existing_document_save() {
    run(async {
        let (store, session) = peer::connect(b"original").await;
        {
            let mut store = store.lock().unwrap();
            store.files.insert("/other".into(), peer::Node::new(b"original"));
            store.aliases.insert("/link".into(), "/config".into());
        }
        let location = RemoteLocation { destination: "test-host".into(), path: "/link".into() };
        let document =
            RemoteDocument::load_from_session(location, session, &DocumentOperation::default())
                .await
                .unwrap();
        store.lock().unwrap().aliases.insert("/link".into(), "/other".into());
        assert!(matches!(
            document.save("draft".into(), DocumentOperation::default()).await,
            Err(SaveError::Changed)
        ));
        let store = store.lock().unwrap();
        assert_eq!(store.files["/config"].data, b"original");
        assert_eq!(store.files["/other"].data, b"original");
    });
}
