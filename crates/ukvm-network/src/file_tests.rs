use crate::files::*;
use std::{
    path::Path,
    sync::atomic::{AtomicBool, Ordering},
};
use std::{sync::Arc, time::Duration};
use ukvm_core::files::FileSource;
use ukvm_platform::files::linux::{Receiver, Source};
use ukvm_protocol::files::{Entry, Kind, Message, Offer, CHUNK_BYTES};
fn check() -> Check {
    Arc::new(|| Ok(()))
}
fn observer() -> Observer {
    Arc::new(|_| Ok(()))
}
async fn copy(paths: &[std::path::PathBuf], root: &Path) -> String {
    let source = Source::new(paths, new_id().unwrap()).unwrap();
    let root = root.to_owned();
    let (mut a, mut b) = tokio::io::duplex(CHUNK_BYTES * 2);
    let (sent, received) = tokio::join!(
        send(&mut a, Box::new(source), check(), observer()),
        receive(&mut b, check(), observer(), move |o| Ok(Box::new(
            Receiver::new(&root, o)?
        )))
    );
    assert_eq!(sent.as_ref().unwrap(), received.as_ref().unwrap());
    sent.unwrap()
}
#[tokio::test]
async fn text_binary_empty_unicode_spaces_tree_multiple_and_reconnect() {
    let t = tempfile::tempdir().unwrap();
    let root = t.path().join("received");
    let src = t.path().join("source");
    std::fs::create_dir(&src).unwrap();
    for (name, bytes) in [
        ("你好 file.txt", "héllo 世界".as_bytes()),
        ("empty.bin", &[][..]),
        ("binary.bin", &[0, 255, 128, 13, 10][..]),
    ] {
        let path = src.join(name);
        std::fs::write(&path, bytes).unwrap();
        let result = copy(&[path], &root).await;
        assert_eq!(std::fs::read(root.join(result)).unwrap(), bytes);
    }
    let tree = src.join("folder with spaces");
    std::fs::create_dir_all(tree.join("deep/empty")).unwrap();
    std::fs::write(tree.join("deep/é.txt"), "directory text").unwrap();
    let result = copy(std::slice::from_ref(&tree), &root).await;
    assert_eq!(
        std::fs::read(root.join(&result).join("deep/é.txt")).unwrap(),
        b"directory text"
    );
    assert!(root.join(result).join("deep/empty").is_dir());
    let name = copy(&[src.join("binary.bin"), src.join("empty.bin")], &root).await;
    assert!(root.join(name).join("empty.bin").is_file());
    let name = copy(&[src.join("binary.bin")], &root).await;
    assert_eq!(name, "binary (1).bin");
    let selected = [src.join("binary.bin")];
    let (a, b) = tokio::join!(copy(&selected, &root), copy(&selected, &root));
    assert_ne!(a, b);
}
#[tokio::test]
async fn large_stream_yields_to_other_work_and_keeps_memory_bounded() {
    let t = tempfile::tempdir().unwrap();
    let file = t.path().join("large.bin");
    let mut f = std::fs::File::create(&file).unwrap();
    use std::io::Write;
    let block = [77u8; CHUNK_BYTES];
    for _ in 0..128 {
        f.write_all(&block).unwrap();
    }
    drop(f);
    let mut source = Source::new(std::slice::from_ref(&file), new_id().unwrap()).unwrap();
    assert_eq!(source.read_chunk(0).unwrap().len(), CHUNK_BYTES);
    let done = Arc::new(AtomicBool::new(false));
    let flag = done.clone();
    let root = t.path().join("received");
    let work = async {
        tokio::time::sleep(Duration::from_millis(20)).await;
        assert!(!flag.load(Ordering::Acquire));
    };
    let transfer = async {
        let name = copy(std::slice::from_ref(&file), &root).await;
        done.store(true, Ordering::Release);
        assert_eq!(
            std::fs::read(file).unwrap(),
            std::fs::read(root.join(name)).unwrap()
        );
    };
    tokio::join!(transfer, work);
}
async fn hostile(mode: &str) -> (String, bool) {
    let t = tempfile::tempdir().unwrap();
    let root = t.path().join("receive");
    let out = root.clone();
    let mode = mode.to_owned();
    let (mut a, mut b) = tokio::io::duplex(CHUNK_BYTES * 2);
    let offer = Offer {
        id: new_id().unwrap(),
        entries: vec![Entry {
            path: vec!["test.bin".into()],
            kind: Kind::File,
            size: 3,
        }],
        total_bytes: 3,
    };
    let server = receive(&mut b, check(), observer(), move |o| {
        Ok(Box::new(Receiver::new(&out, o)?))
    });
    let client = async {
        write(&mut a, &Message::hello()).await.unwrap();
        read(&mut a).await.unwrap();
        write(&mut a, &Message::Offer(offer)).await.unwrap();
        assert_eq!(read(&mut a).await.unwrap(), Message::Accepted);
        if mode == "interrupt" {
            drop(a);
            return;
        }
        let msg = match mode.as_str() {
            "cancel" => Message::Cancel,
            "size" => Message::Chunk {
                index: 0,
                offset: 0,
                bytes: vec![1; 4],
            },
            "offset" => Message::Chunk {
                index: 0,
                offset: 1,
                bytes: vec![1; 3],
            },
            _ => Message::FileEnd {
                index: 0,
                size: 0,
                sha256: [0; 32],
            },
        };
        if mode == "hash" {
            write(
                &mut a,
                &Message::Chunk {
                    index: 0,
                    offset: 0,
                    bytes: vec![1; 3],
                },
            )
            .await
            .unwrap();
            read(&mut a).await.unwrap();
            write(
                &mut a,
                &Message::FileEnd {
                    index: 0,
                    size: 3,
                    sha256: [0; 32],
                },
            )
            .await
            .unwrap();
        } else {
            write(&mut a, &msg).await.unwrap();
        }
        let _ = read(&mut a).await;
    };
    let (result, _) = tokio::join!(server, client);
    (
        result.unwrap_err(),
        root.read_dir().unwrap().next().is_none(),
    )
}
#[tokio::test]
async fn hash_size_offset_interruption_and_cancellation_leave_nothing_complete() {
    for mode in ["hash", "size", "offset", "interrupt", "cancel"] {
        let (error, clean) = hostile(mode).await;
        assert!(clean, "{mode}: {error}");
        assert!(!error.contains("private payload"));
    }
}
#[tokio::test]
async fn unauthorized_and_live_revocation_stop_before_finalization() {
    let t = tempfile::tempdir().unwrap();
    let path = t.path().join("test.bin");
    std::fs::write(&path, vec![5; CHUNK_BYTES * 8]).unwrap();
    let root = t.path().join("received");
    for initially_allowed in [false, true] {
        let allowed = Arc::new(AtomicBool::new(initially_allowed));
        let flag = allowed.clone();
        let policy: Check = Arc::new(move || {
            if flag.load(Ordering::Acquire) {
                Ok(())
            } else {
                Err("receive-files denied".into())
            }
        });
        let changed = allowed.clone();
        let report: Observer = Arc::new(move |p| {
            if p.bytes > 0 {
                changed.store(false, Ordering::Release);
            }
            Ok(())
        });
        let (mut a, mut b) = tokio::io::duplex(CHUNK_BYTES * 2);
        let out = root.clone();
        let source = Source::new(std::slice::from_ref(&path), new_id().unwrap()).unwrap();
        let (s, r) = tokio::join!(
            send(&mut a, Box::new(source), check(), observer()),
            receive(&mut b, policy, report, move |o| Ok(Box::new(
                Receiver::new(&out, o)?
            )))
        );
        assert!(s.is_err());
        assert!(r.is_err());
        assert!(!root.join("test.bin").exists());
    }
}
#[tokio::test]
async fn pinned_tls_file_capability_and_old_peer_compatibility() {
    use crate::{
        identity::Identity,
        transport::{self, RuntimeConnection},
        trust::{SessionPermission, TrustStore},
    };
    let t = tempfile::tempdir().unwrap();
    let a = Identity::load_or_create(&t.path().join("a")).unwrap();
    let b = Identity::load_or_create(&t.path().join("b")).unwrap();
    let bd = t.path().join("bt");
    let mut store = TrustStore::default();
    store
        .import(&bd, "A".into(), None, &std::fs::read(&a.cert_path).unwrap())
        .unwrap();
    assert!(store
        .authorize(&a.id, SessionPermission::ReceiveFiles)
        .is_err());
    store
        .set_permission(&bd, &a.id, SessionPermission::ReceiveFiles, true)
        .unwrap();
    assert!(store
        .authorize(&a.id, SessionPermission::ControlThisNode)
        .is_err());
    assert!(store
        .authorize(&a.id, SessionPermission::ShareClipboard)
        .is_err());
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap().to_string();
    let path = t.path().join("file.txt");
    std::fs::write(&path, b"secure payload").unwrap();
    let source = Source::new(&[path], new_id().unwrap()).unwrap();
    let root = t.path().join("received");
    let out = root.clone();
    let server = async {
        let (tcp, _) = listener.accept().await.unwrap();
        let RuntimeConnection::Files {
            mut stream,
            peer_id,
        } = transport::accept_runtime(tcp, &b, &store, &bd, "B")
            .await
            .unwrap()
        else {
            panic!("wrong ALPN")
        };
        assert_eq!(peer_id, a.id);
        receive(&mut stream, check(), observer(), move |o| {
            Ok(Box::new(Receiver::new(&out, o)?))
        })
        .await
        .unwrap();
    };
    let client = async {
        let mut stream = connect(&address, &a, &b.cert_path, &b.id).await.unwrap();
        send(&mut stream, Box::new(source), check(), observer())
            .await
            .unwrap();
    };
    tokio::join!(server, client);
    assert_eq!(
        std::fs::read(root.join("file.txt")).unwrap(),
        b"secure payload"
    );
    let old = async {
        let (tcp, _) = listener.accept().await.unwrap();
        let _ = transport::accept(tcp, &b, &a.cert_path, "B").await;
    };
    let client = async {
        assert!(connect(&address, &a, &b.cert_path, &b.id).await.is_err());
    };
    tokio::join!(old, client);
}
#[tokio::test]
async fn input_heartbeat_and_clipboard_on_tls_during_bulk_transfer() {
    use crate::{
        clipboard,
        identity::Identity,
        transport::{self, RuntimeConnection},
        trust::{SessionPermission, TrustStore},
    };
    let t = tempfile::tempdir().unwrap();
    let a = Identity::load_or_create(&t.path().join("a")).unwrap();
    let b = Identity::load_or_create(&t.path().join("b")).unwrap();
    let bd = t.path().join("trust");
    let mut store = TrustStore::default();
    store
        .import(&bd, "A".into(), None, &std::fs::read(&a.cert_path).unwrap())
        .unwrap();
    store
        .set_permission(&bd, &a.id, SessionPermission::ShareClipboard, true)
        .unwrap();
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap().to_string();
    let file = t.path().join("bulk.bin");
    std::fs::write(&file, vec![17; 8 * 1024 * 1024]).unwrap();
    let root = t.path().join("receive");
    let completed = Arc::new(AtomicBool::new(false));
    let observed = completed.clone();
    let bulk = async {
        copy(&[file], &root).await;
        completed.store(true, Ordering::Release);
    };
    let traffic = async {
        for is_clipboard in [false, true] {
            let server = async {
                let (tcp, _) = listener.accept().await.unwrap();
                match transport::accept_runtime(tcp, &b, &store, &bd, "B")
                    .await
                    .unwrap()
                {
                    RuntimeConnection::Input(mut c) => {
                        let (m, _) = crate::read_frame(&mut c.stream).await.unwrap();
                        assert!(matches!(m, ukvm_protocol::WireMessage::Heartbeat(123)));
                        crate::write_frame(
                            &mut c.stream,
                            &ukvm_protocol::WireMessage::HeartbeatAck(123),
                        )
                        .await
                        .unwrap();
                    }
                    RuntimeConnection::Clipboard { mut stream, .. } => {
                        clipboard::negotiate(&mut stream).await.unwrap();
                        let message = clipboard::read(&mut stream).await.unwrap();
                        assert!(matches!(
                            message,
                            ukvm_protocol::clipboard::ClipboardMessage::Text { .. }
                        ));
                    }
                    _ => panic!("unexpected file channel"),
                }
            };
            let client = async {
                if is_clipboard {
                    let mut s = clipboard::connect(&address, &a, &b.cert_path, &b.id)
                        .await
                        .unwrap();
                    clipboard::negotiate(&mut s).await.unwrap();
                    clipboard::write(
                        &mut s,
                        &ukvm_protocol::clipboard::ClipboardMessage::Text {
                            origin: a.id.clone(),
                            epoch: [2; 16],
                            generation: 1,
                            byte_length: 4,
                            text: "text".into(),
                        },
                    )
                    .await
                    .unwrap();
                } else {
                    let mut c = transport::connect(&address, &a, &b.cert_path, "A")
                        .await
                        .unwrap();
                    crate::write_frame(&mut c.stream, &ukvm_protocol::WireMessage::Heartbeat(123))
                        .await
                        .unwrap();
                    let (m, _) = crate::read_frame(&mut c.stream).await.unwrap();
                    assert!(matches!(m, ukvm_protocol::WireMessage::HeartbeatAck(123)));
                }
            };
            tokio::time::timeout(Duration::from_millis(500), async {
                tokio::join!(server, client);
            })
            .await
            .unwrap();
            assert!(!observed.load(Ordering::Acquire));
        }
    };
    tokio::join!(bulk, traffic);
}
