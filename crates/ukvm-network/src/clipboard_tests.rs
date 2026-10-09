use super::*;
use crate::{
    identity::Identity,
    transport::{self, RuntimeConnection},
    trust::{cert_path, SessionPermission, TrustStore},
};
#[tokio::test]
async fn pinned_tls_clipboard_and_legacy_input_negotiate_independently() {
    let temp = tempfile::tempdir().unwrap();
    let a = Identity::load_or_create(&temp.path().join("a")).unwrap();
    let b = Identity::load_or_create(&temp.path().join("b")).unwrap();
    let a_trust = temp.path().join("a-trust");
    let b_trust = temp.path().join("b-trust");
    let mut a_store = TrustStore::default();
    a_store
        .import(
            &a_trust,
            "B".into(),
            None,
            &std::fs::read(&b.cert_path).unwrap(),
        )
        .unwrap();
    let mut b_store = TrustStore::default();
    b_store
        .import(
            &b_trust,
            "A".into(),
            None,
            &std::fs::read(&a.cert_path).unwrap(),
        )
        .unwrap();
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap().to_string();
    let cert = cert_path(&a_trust, &b.id);
    let server = async {
        let (tcp, _) = listener.accept().await.unwrap();
        assert!(transport::accept_runtime(tcp, &b, &b_store, &b_trust, "B")
            .await
            .is_err());
    };
    let client = async {
        let _ = connect(&address, &a, &cert, &b.id).await;
    };
    tokio::join!(server, client);
    b_store
        .set_permission(&b_trust, &a.id, SessionPermission::ShareClipboard, true)
        .unwrap();
    let server = async {
        let (tcp, _) = listener.accept().await.unwrap();
        let RuntimeConnection::Clipboard {
            mut stream,
            peer_id,
        } = transport::accept_runtime(tcp, &b, &b_store, &b_trust, "B")
            .await
            .unwrap()
        else {
            panic!("wrong channel")
        };
        assert_eq!(peer_id, a.id);
        negotiate(&mut stream).await.unwrap();
        let payload = read(&mut stream).await.unwrap();
        assert!(matches!(payload,ClipboardMessage::Text { ref text,.. } if text == "TLS text"));
    };
    let client = async {
        let mut stream = connect(&address, &a, &cert, &b.id).await.unwrap();
        negotiate(&mut stream).await.unwrap();
        write(
            &mut stream,
            &ClipboardMessage::Text {
                origin: a.id.clone(),
                epoch: [1; 16],
                generation: 1,
                byte_length: 8,
                text: "TLS text".into(),
            },
        )
        .await
        .unwrap();
    };
    tokio::join!(server, client);
    let server = async {
        let (tcp, _) = listener.accept().await.unwrap();
        assert!(matches!(
            transport::accept_runtime(tcp, &b, &b_store, &b_trust, "B")
                .await
                .unwrap(),
            RuntimeConnection::Input(_)
        ));
    };
    let client = async {
        transport::connect(&address, &a, &cert, "A").await.unwrap();
    };
    tokio::join!(server, client);
}

#[tokio::test]
async fn old_peer_has_no_clipboard_negotiation_or_input_session() {
    let temp = tempfile::tempdir().unwrap();
    let a = Identity::load_or_create(&temp.path().join("a")).unwrap();
    let b = Identity::load_or_create(&temp.path().join("b")).unwrap();
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap().to_string();
    let server = async {
        let (tcp, _) = listener.accept().await.unwrap();
        assert!(transport::accept(tcp, &b, &a.cert_path, "B").await.is_err());
    };
    let client = async {
        assert!(connect(&address, &a, &b.cert_path, &b.id)
            .await
            .unwrap_err()
            .contains("does not support"));
    };
    tokio::join!(server, client);
}
