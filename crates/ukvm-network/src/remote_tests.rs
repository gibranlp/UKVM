//! Real pinned-TLS regressions; no OS consent or physical input is simulated as success.
use crate::{
    identity::Identity,
    read_frame,
    transport::{self, RuntimeConnection},
    trust::{SessionPermission, TrustStore},
    write_frame,
};
use std::{
    sync::{Arc, Mutex},
    time::Duration,
};
use ukvm_core::controlled::{InputSink, SinkAction, SinkKind};
use ukvm_protocol::{InputEvent, KeyCode, WireMessage};
#[derive(Default)]
struct Evidence {
    begun: usize,
    entered: usize,
    events: usize,
    held: bool,
    ended: usize,
}
struct Sink(Arc<Mutex<Evidence>>);
impl InputSink for Sink {
    fn kind(&self) -> SinkKind {
        SinkKind::RemoteInput
    }
    fn begin_session(&mut self) -> Result<(), String> {
        self.0.lock().unwrap().begun += 1;
        Ok(())
    }
    fn enter_window(
        &mut self,
        _: ukvm_protocol::display::WindowTarget,
        _: f64,
        _: f64,
    ) -> Result<(), String> {
        self.0.lock().unwrap().entered += 1;
        Ok(())
    }
    fn handle_event(&mut self, _: &InputEvent) -> Result<SinkAction, String> {
        let mut e = self.0.lock().unwrap();
        e.events += 1;
        e.held = true;
        Ok(SinkAction::Continue)
    }
    fn end_session(&mut self) -> Result<(), String> {
        let mut e = self.0.lock().unwrap();
        e.ended += 1;
        e.held = false;
        Ok(())
    }
}
struct Fixture {
    temp: tempfile::TempDir,
    a: Identity,
    b: Identity,
    store: TrustStore,
}
impl Fixture {
    fn new(view: bool, control: bool) -> Self {
        let temp = tempfile::tempdir().unwrap();
        let a = Identity::load_or_create(&temp.path().join("a")).unwrap();
        let b = Identity::load_or_create(&temp.path().join("b")).unwrap();
        let mut store = TrustStore::default();
        let dir = temp.path().join("trust");
        store
            .import(&dir, "a".into(), None, a.tls_cert().as_ref())
            .unwrap();
        store
            .set_permission(&dir, "a", SessionPermission::ViewScreen, view)
            .unwrap();
        store
            .set_permission(&dir, "a", SessionPermission::ControlThisNode, control)
            .unwrap();
        Self { temp, a, b, store }
    }
}
#[tokio::test]
async fn display_alpn_is_denied_before_capture_and_independent_of_input() {
    for (view, control) in [(false, false), (false, true), (true, false), (true, true)] {
        let f = Fixture::new(view, control);
        let listener = transport::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap().to_string();
        let trust = f.temp.path().join("trust");
        let b = f.b.clone();
        let store = f.store.clone();
        let receiver = tokio::spawn(async move {
            let (tcp, _) = listener.accept().await.unwrap();
            transport::accept_runtime(tcp, &b, &store, &trust, "b").await
        });
        let tls = crate::display::connect(&address, &f.a, &f.b.cert_path, &f.b.id)
            .await
            .unwrap();
        let result = receiver.await.unwrap();
        assert_eq!(result.is_ok(), view);
        if let Ok(result) = result {
            assert!(matches!(result,RuntimeConnection::Display{peer_id,..}if peer_id==f.a.id));
        }
        drop(tls);
    }
}
#[tokio::test]
async fn window_control_requires_both_viewing_and_control_permission() {
    for (view, control) in [(false, true), (true, false)] {
        let f = Fixture::new(view, control);
        let listener = transport::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap().to_string();
        let evidence = Arc::new(Mutex::new(Evidence::default()));
        let receiver_evidence = evidence.clone();
        let b = f.b.clone();
        let trust = f.temp.path().join("trust");
        let store = f.store.clone();
        let receiver = tokio::spawn(async move {
            let (tcp, _) = listener.accept().await.unwrap();
            let connection = transport::accept_runtime(tcp, &b, &store, &trust, "b")
                .await
                .unwrap();
            let RuntimeConnection::Input(connection) = connection else {
                panic!("input expected")
            };
            transport::receive_session(connection, &mut Sink(receiver_evidence), |_| {}).await
        });
        let connection = transport::connect(&address, &f.a, &f.b.cert_path, "a")
            .await
            .unwrap();
        assert!(transport::start_window_sender(connection).await.is_err());
        assert!(receiver.await.unwrap().is_err());
        assert_eq!(evidence.lock().unwrap().begun, 0);
    }
}
#[tokio::test]
async fn disconnect_release_and_live_view_revocation_clean_held_input() {
    for revoke in [false, true] {
        let f = Fixture::new(true, true);
        let listener = transport::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap().to_string();
        let evidence = Arc::new(Mutex::new(Evidence::default()));
        let receiver_evidence = evidence.clone();
        let b = f.b.clone();
        let trust = f.temp.path().join("trust");
        let store = f.store.clone();
        let receiver = tokio::spawn(async move {
            let (tcp, _) = listener.accept().await.unwrap();
            let RuntimeConnection::Input(connection) =
                transport::accept_runtime(tcp, &b, &store, &trust, "b")
                    .await
                    .unwrap()
            else {
                panic!("input expected")
            };
            transport::receive_session(connection, &mut Sink(receiver_evidence), |_| {}).await
        });
        let mut connection = transport::connect(&address, &f.a, &f.b.cert_path, "a")
            .await
            .unwrap();
        write_frame(&mut connection.stream, &WireMessage::WindowSessionStart)
            .await
            .unwrap();
        assert_eq!(
            read_frame(&mut connection.stream).await.unwrap().0,
            WireMessage::RemoteSessionAccept
        );
        write_frame(
            &mut connection.stream,
            &WireMessage::WindowEntry {
                target: window_target(),
                x: -500.,
                y: 200.,
            },
        )
        .await
        .unwrap();
        assert_eq!(
            read_frame(&mut connection.stream).await.unwrap().0,
            WireMessage::WindowEntered
        );
        for event in [
            InputEvent::PointerAbsolute { x: -499., y: 201. },
            InputEvent::Key {
                code: KeyCode::LinuxEvdev(42),
                pressed: true,
            },
            InputEvent::PointerButton {
                button: 272,
                pressed: true,
            },
        ] {
            write_frame(&mut connection.stream, &WireMessage::Input(event))
                .await
                .unwrap();
        }
        tokio::time::timeout(Duration::from_secs(2), async {
            while evidence.lock().unwrap().events < 3 {
                tokio::time::sleep(Duration::from_millis(5)).await;
            }
        })
        .await
        .unwrap();
        assert!(evidence.lock().unwrap().held);
        if revoke {
            let dir = f.temp.path().join("trust");
            let mut store = TrustStore::load(&dir).unwrap();
            store
                .set_permission(&dir, "a", SessionPermission::ViewScreen, false)
                .unwrap();
        } else {
            drop(connection);
        }
        assert!(tokio::time::timeout(Duration::from_secs(2), receiver)
            .await
            .unwrap()
            .unwrap()
            .is_err());
        let e = evidence.lock().unwrap();
        assert!(!e.held);
        assert_eq!(e.entered, 1);
        assert_eq!(e.ended, 1);
    }
}
#[tokio::test]
async fn absolute_input_is_rejected_in_physical_edge_session() {
    let f = Fixture::new(true, true);
    let listener = transport::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap().to_string();
    let b = f.b.clone();
    let trust = f.temp.path().join("trust");
    let store = f.store.clone();
    let receiver = tokio::spawn(async move {
        let (tcp, _) = listener.accept().await.unwrap();
        let RuntimeConnection::Input(connection) =
            transport::accept_runtime(tcp, &b, &store, &trust, "b")
                .await
                .unwrap()
        else {
            panic!("input expected")
        };
        transport::receive_session(
            connection,
            &mut Sink(Arc::new(Mutex::new(Evidence::default()))),
            |_| {},
        )
        .await
        .unwrap_err()
    });
    let mut connection = transport::connect(&address, &f.a, &f.b.cert_path, "a")
        .await
        .unwrap();
    write_frame(&mut connection.stream, &WireMessage::RemoteSessionStart)
        .await
        .unwrap();
    read_frame(&mut connection.stream).await.unwrap();
    write_frame(
        &mut connection.stream,
        &WireMessage::Input(InputEvent::PointerAbsolute { x: 0., y: 0. }),
    )
    .await
    .unwrap();
    assert!(receiver
        .await
        .unwrap()
        .contains("absolute input outside window session"));
}

fn window_target() -> ukvm_protocol::display::WindowTarget {
    ukvm_protocol::display::WindowTarget {
        origin_known: true,
        screen: ukvm_protocol::display::ScreenRect {
            x: -1000,
            y: 0,
            width: 1920,
            height: 1080,
        },
        mapping_id: None,
    }
}
