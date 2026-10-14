use super::*;
#[tokio::test]
async fn bidirectional_session_revocation_blocks_queued_write() {
    let fixture = super::super::tests::Fixture::new(false);
    let context = &fixture.context;
    let id = fixture.peer.id.clone();
    let mut store = TrustStore::load(&context.peers.trust_dir).unwrap();
    store
        .set_permission(
            &context.peers.trust_dir,
            &id,
            SessionPermission::ShareClipboard,
            true,
        )
        .unwrap();
    assert!(!store.find_id(&id).unwrap().permissions.control_this_node);
    context.clipboard.status.lock().unwrap().supported = Some(true);
    let mut writes = context.clipboard.writes.subscribe();
    let (a, mut b) = tokio::io::duplex(128 * 1024);
    let (stop_tx, stop) = watch::channel(false);
    let running = session(context, &id, a, stop);
    let exercise = async {
        wire::negotiate(&mut b).await.unwrap();
        for (generation, text) in [(1, "world"), (2, "你好 🦀"), (3, "")] {
            let message = ClipboardMessage::Text {
                origin: id.clone(),
                epoch: [7; 16],
                generation: u64::MAX / 2 + generation,
                byte_length: text.len() as u32,
                text: text.into(),
            };
            wire::write(&mut b, &message).await.unwrap();
            tokio::time::timeout(Duration::from_secs(2), writes.changed())
                .await
                .unwrap()
                .unwrap();
            let request = writes.borrow_and_update().clone().unwrap();
            assert_eq!(request.text, text);
            assert!((request.authorize)().is_some());
            assert!(context
                .clipboard
                .engine
                .lock()
                .unwrap()
                .local(text.into())
                .is_none());
        }
        let outbound = context
            .clipboard
            .engine
            .lock()
            .unwrap()
            .local("hello".into())
            .unwrap();
        context.clipboard.latest.send_replace(Some(outbound));
        loop {
            let message = wire::read(&mut b).await.unwrap();
            match message {
                ClipboardMessage::Ping => {
                    wire::write(&mut b, &ClipboardMessage::Pong).await.unwrap()
                }
                ClipboardMessage::Text { text, .. } => {
                    assert_eq!(text, "hello");
                    break;
                }
                _ => panic!("unexpected message"),
            }
        }
        let queued = writes.borrow().clone().unwrap();
        {
            let _edit = ukvm_network::trust::edit_lock(&context.peers.trust_dir).unwrap();
            store
                .set_permission(
                    &context.peers.trust_dir,
                    &id,
                    SessionPermission::ShareClipboard,
                    false,
                )
                .unwrap();
        }
        assert!((queued.authorize)().is_none());
        let payload = context
            .clipboard
            .engine
            .lock()
            .unwrap()
            .local("never sent".into())
            .unwrap();
        assert!(send(context, &id, &mut tokio::io::sink(), &payload)
            .await
            .is_err());
        stop_tx.send_replace(true);
    };
    let (result, ()) = tokio::time::timeout(Duration::from_secs(8), async {
        tokio::join!(running, exercise)
    })
    .await
    .unwrap();
    assert!(result.is_ok() || result.unwrap_err().contains("revoked"));
    assert_eq!(context.clipboard.snapshot().connected_peers, 0);
    let safe = toml::to_string(&context.clipboard.snapshot()).unwrap();
    for secret in ["hello", "world", "never sent", "你好"] {
        assert!(!safe.contains(secret));
    }
}

#[tokio::test]
async fn unauthorized_session_never_negotiates_or_requests_portal() {
    let fixture = super::super::tests::Fixture::new(true);
    fixture.context.clipboard.status.lock().unwrap().supported = Some(true);
    let (stream, _peer) = tokio::io::duplex(1024);
    let error = session(
        &fixture.context,
        &fixture.peer.id,
        stream,
        watch::channel(false).1,
    )
    .await
    .unwrap_err();
    assert!(error.contains("denied"));
    assert!(fixture.context.clipboard.writes.borrow().is_none());
    assert_eq!(
        *fixture.context.receiver_status.lock().unwrap(),
        "consent not requested"
    );
}

#[tokio::test]
async fn reconnect_deduplicates_old_generation_and_accepts_new_text() {
    let fixture = super::super::tests::Fixture::new(false);
    let context = &fixture.context;
    let id = fixture.peer.id.clone();
    let mut store = TrustStore::load(&context.peers.trust_dir).unwrap();
    store
        .set_permission(
            &context.peers.trust_dir,
            &id,
            SessionPermission::ShareClipboard,
            true,
        )
        .unwrap();
    context.clipboard.status.lock().unwrap().supported = Some(true);
    let mut writes = context.clipboard.writes.subscribe();
    for attempt in 0..2 {
        let (a, mut b) = tokio::io::duplex(128 * 1024);
        let (stop_tx, stop) = watch::channel(false);
        let exercise = async {
            wire::negotiate(&mut b).await.unwrap();
            let first = ClipboardMessage::Text {
                origin: id.clone(),
                epoch: [8; 16],
                generation: 1,
                byte_length: 5,
                text: "first".into(),
            };
            wire::write(&mut b, &first).await.unwrap();
            if attempt == 0 {
                writes.changed().await.unwrap();
                writes.borrow_and_update();
            } else {
                assert!(
                    tokio::time::timeout(Duration::from_millis(30), writes.changed())
                        .await
                        .is_err()
                );
                let next = ClipboardMessage::Text {
                    origin: id.clone(),
                    epoch: [8; 16],
                    generation: 2,
                    byte_length: 6,
                    text: "second".into(),
                };
                wire::write(&mut b, &next).await.unwrap();
                writes.changed().await.unwrap();
                assert_eq!(writes.borrow_and_update().as_ref().unwrap().text, "second");
            }
            stop_tx.send_replace(true);
        };
        let (result, ()) = tokio::time::timeout(Duration::from_secs(3), async {
            tokio::join!(session(context, &id, a, stop), exercise)
        })
        .await
        .unwrap();
        result.unwrap();
        assert_eq!(context.clipboard.snapshot().connected_peers, 0);
    }
}
#[tokio::test]
async fn disabled_clipboard_survives_runtime_stop_without_opening_a_backend() {
    let fixture = super::super::tests::Fixture::new(true);
    let (stop_tx, stop) = watch::channel(false);
    let stopping = async {
        tokio::task::yield_now().await;
        stop_tx.send_replace(true);
    };
    tokio::join!(supervise(&fixture.context, stop), stopping);
    let state = fixture.context.clipboard.snapshot();
    assert!(!state.enabled);
    assert!(state.supported.is_none());
    assert_eq!(state.connected_peers, 0);
    assert!(fixture.context.clipboard.writes.borrow().is_none());
}

#[tokio::test]
async fn local_copy_fans_out_only_to_connected_permitted_peers() {
    let fixture = super::super::tests::Fixture::new(false);
    let temp = tempfile::tempdir().unwrap();
    let third = Identity::load_or_create(&temp.path().join("third")).unwrap();
    let context = &fixture.context;
    let mut store = TrustStore::load(&context.peers.trust_dir).unwrap();
    store
        .import(
            &context.peers.trust_dir,
            "third".into(),
            None,
            &std::fs::read(&third.cert_path).unwrap(),
        )
        .unwrap();
    for id in [&fixture.peer.id, &third.id] {
        store
            .set_permission(
                &context.peers.trust_dir,
                id,
                SessionPermission::ShareClipboard,
                true,
            )
            .unwrap();
    }
    context.clipboard.status.lock().unwrap().supported = Some(true);
    let (a, mut b) = tokio::io::duplex(128 * 1024);
    let (c, mut d) = tokio::io::duplex(128 * 1024);
    let (stop_tx, stop) = watch::channel(false);
    let exercise = async {
        let (first, second) = tokio::join!(wire::negotiate(&mut b), wire::negotiate(&mut d));
        first.unwrap();
        second.unwrap();
        let message = context
            .clipboard
            .engine
            .lock()
            .unwrap()
            .local("shared 🌍".into())
            .unwrap();
        context.clipboard.latest.send_replace(Some(message));
        async fn text(stream: &mut tokio::io::DuplexStream) {
            loop {
                match wire::read(stream).await.unwrap() {
                    ClipboardMessage::Ping => {
                        wire::write(stream, &ClipboardMessage::Pong).await.unwrap()
                    }
                    ClipboardMessage::Text { text, .. } => {
                        assert_eq!(text, "shared 🌍");
                        break;
                    }
                    _ => panic!("unexpected message"),
                }
            }
        }
        tokio::join!(text(&mut b), text(&mut d));
        assert_eq!(context.clipboard.snapshot().connected_peers, 2);
        stop_tx.send_replace(true);
    };
    let (first, second, ()) = tokio::time::timeout(Duration::from_secs(5), async {
        tokio::join!(
            session(context, &fixture.peer.id, a, stop.clone()),
            session(context, &third.id, c, stop),
            exercise
        )
    })
    .await
    .unwrap();
    first.unwrap();
    second.unwrap();
    assert_eq!(context.clipboard.snapshot().connected_peers, 0);
}
