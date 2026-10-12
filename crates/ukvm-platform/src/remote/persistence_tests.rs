//! Private-bus tests use real ashpd SelectSources/Start requests, never a desktop portal.
use super::*;
use std::{
    collections::HashMap,
    io::{BufRead, BufReader},
    process::{Child, Command, Stdio},
    sync::{atomic::AtomicU32, Mutex},
};
use zbus::{
    message::Header,
    zvariant::{OwnedObjectPath, OwnedValue, Str},
};

struct Bus(Child);
impl Drop for Bus {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}
fn value(text: &str) -> OwnedValue {
    OwnedValue::from(Str::from(text))
}
async fn response(
    connection: &zbus::Connection,
    header: &Header<'_>,
    options: &HashMap<String, OwnedValue>,
    code: u32,
    results: HashMap<String, OwnedValue>,
) -> OwnedObjectPath {
    let sender = header.sender().unwrap();
    let token = <&str>::try_from(&options["handle_token"])
        .unwrap()
        .to_owned();
    let path = OwnedObjectPath::try_from(format!(
        "/org/freedesktop/portal/desktop/request/{}/{}",
        sender.as_str().trim_start_matches(':').replace('.', "_"),
        token
    ))
    .unwrap();
    connection
        .emit_signal(
            Some(sender.as_str()),
            &path,
            "org.freedesktop.portal.Request",
            "Response",
            &(code, results),
        )
        .await
        .unwrap();
    path
}
type Selections = Arc<Mutex<Vec<(Option<u32>, Option<String>)>>>;
struct Portal {
    selections: Selections,
    starts: AtomicU32,
    reject: bool,
    version: u32,
    persist: bool,
    reissue_same_value: bool,
}
#[zbus::interface(name = "org.freedesktop.portal.ScreenCast")]
impl Portal {
    #[zbus(property, name = "version")]
    fn version(&self) -> u32 {
        self.version
    }
    #[zbus(property)]
    fn available_source_types(&self) -> u32 {
        1
    }
    async fn create_session(
        &self,
        options: HashMap<String, OwnedValue>,
        #[zbus(header)] header: Header<'_>,
        #[zbus(connection)] connection: &zbus::Connection,
    ) -> OwnedObjectPath {
        let sender = header
            .sender()
            .unwrap()
            .as_str()
            .trim_start_matches(':')
            .replace('.', "_");
        let token = <&str>::try_from(&options["session_handle_token"])
            .unwrap()
            .to_owned();
        let mut results = HashMap::new();
        results.insert(
            "session_handle".into(),
            value(&format!(
                "/org/freedesktop/portal/desktop/session/{sender}/{token}"
            )),
        );
        response(connection, &header, &options, 0, results).await
    }
    async fn select_sources(
        &self,
        _session: OwnedObjectPath,
        options: HashMap<String, OwnedValue>,
        #[zbus(header)] header: Header<'_>,
        #[zbus(connection)] connection: &zbus::Connection,
    ) -> OwnedObjectPath {
        assert_eq!(u32::try_from(&options["types"]).unwrap(), 1);
        assert!(!bool::try_from(&options["multiple"]).unwrap());
        let mode = options
            .get("persist_mode")
            .map(|v| u32::try_from(v).unwrap());
        let token = options
            .get("restore_token")
            .map(|value| <&str>::try_from(value).unwrap().to_owned());
        self.selections.lock().unwrap().push((mode, token));
        response(connection, &header, &options, 0, HashMap::new()).await
    }
    async fn start(
        &self,
        _session: OwnedObjectPath,
        _parent: String,
        options: HashMap<String, OwnedValue>,
        #[zbus(header)] header: Header<'_>,
        #[zbus(connection)] connection: &zbus::Connection,
    ) -> OwnedObjectPath {
        let index = self.starts.fetch_add(1, Ordering::SeqCst) + 1;
        let index = if self.reissue_same_value { 1 } else { index };
        let mut results = HashMap::new();
        let streams = vec![(42u32, HashMap::<String, OwnedValue>::new())];
        results.insert(
            "streams".into(),
            OwnedValue::try_from(zbus::zvariant::Value::from(streams)).unwrap(),
        );
        if self.persist {
            results.insert(
                "restore_token".into(),
                value(&format!("private-token-{index}")),
            );
        }
        response(
            connection,
            &header,
            &options,
            if self.reject { 2 } else { 0 },
            results,
        )
        .await
    }
}
async fn fixture(
    reject: bool,
    version: u32,
    persist: bool,
    reissue_same_value: bool,
) -> (
    tempfile::TempDir,
    Bus,
    zbus::Connection,
    Screencast,
    Selections,
) {
    let dir = tempfile::tempdir().unwrap();
    let mut child = Command::new("dbus-daemon")
        .args(["--session", "--nofork", "--print-address=1"])
        .stdout(Stdio::piped())
        .spawn()
        .unwrap();
    let mut address = String::new();
    BufReader::new(child.stdout.take().unwrap())
        .read_line(&mut address)
        .unwrap();
    let bus = Bus(child);
    let selections = Arc::new(Mutex::new(Vec::new()));
    let server = zbus::connection::Builder::address(address.trim())
        .unwrap()
        .name("org.freedesktop.portal.Desktop")
        .unwrap()
        .serve_at(
            "/org/freedesktop/portal/desktop",
            Portal {
                selections: selections.clone(),
                starts: AtomicU32::new(0),
                reject,
                version,
                persist,
                reissue_same_value,
            },
        )
        .unwrap()
        .build()
        .await
        .unwrap();
    let client = zbus::connection::Builder::address(address.trim())
        .unwrap()
        .build()
        .await
        .unwrap();
    let portal = Screencast::with_connection(client).await.unwrap();
    (dir, bus, server, portal, selections)
}

#[tokio::test]
async fn official_screen_cast_options_consume_rotate_and_accept_reissued_tokens() {
    for reissue in [false, true] {
        let (dir, _bus, _server, portal, selections) = fixture(false, 4, true, reissue).await;
        let state = dir.path().join("ukvm/portal-authorization");
        for _ in 0..2 {
            let mut authorization =
                Authorization::begin_in(&state, Kind::ScreenCast, Context::current(4)).unwrap();
            let session = portal.create_session(Default::default()).await.unwrap();
            let response = tokio::time::timeout(
                Duration::from_secs(5),
                authorize_monitor(&portal, &session, &mut authorization),
            )
            .await
            .unwrap()
            .unwrap();
            assert_eq!(response.streams().len(), 1);
        }
        assert_eq!(
            *selections.lock().unwrap(),
            vec![(Some(2), None), (Some(2), Some("private-token-1".into()))]
        );
        let next = Authorization::begin_in(&state, Kind::ScreenCast, Context::current(4)).unwrap();
        assert_eq!(
            next.restore_token(),
            Some(if reissue {
                "private-token-1"
            } else {
                "private-token-2"
            })
        );
        assert!(!std::fs::read_to_string(state.join("screen-cast.json"))
            .unwrap()
            .contains("private-token"));
    }
}
#[tokio::test]
async fn rejected_restoration_never_replays_a_screen_token_or_logs_it() {
    let (dir, _bus, _server, portal, _) = fixture(true, 4, true, false).await;
    let state = dir.path().join("ukvm/portal-authorization");
    let mut approval =
        Authorization::begin_in(&state, Kind::ScreenCast, Context::current(4)).unwrap();
    approval.granted(Some("secret-old-screen-token")).unwrap();
    drop(approval);
    let mut restored =
        Authorization::begin_in(&state, Kind::ScreenCast, Context::current(4)).unwrap();
    assert_eq!(restored.restore_token(), Some("secret-old-screen-token"));
    let session = portal.create_session(Default::default()).await.unwrap();
    let error = authorize_monitor(&portal, &session, &mut restored)
        .await
        .unwrap_err();
    assert!(error.contains("Start denied"));
    assert!(!error.contains("secret-old-screen-token"));
    drop(restored);
    let fresh = Authorization::begin_in(&state, Kind::ScreenCast, Context::current(4)).unwrap();
    assert!(fresh.restore_token().is_none());
}
#[tokio::test]
async fn older_portals_and_missing_tokens_do_not_claim_persistence() {
    for version in [3, 4] {
        let (dir, _bus, _server, portal, selections) = fixture(false, version, false, false).await;
        let state = dir.path().join("ukvm/portal-authorization");
        for _ in 0..2 {
            let mut authorization =
                Authorization::begin_in(&state, Kind::ScreenCast, Context::current(version))
                    .unwrap();
            assert!(authorization.restore_token().is_none());
            let session = portal.create_session(Default::default()).await.unwrap();
            authorize_monitor(&portal, &session, &mut authorization)
                .await
                .unwrap();
        }
        let mode = if version >= 4 { Some(2) } else { None };
        assert_eq!(
            *selections.lock().unwrap(),
            vec![(mode, None), (mode, None)]
        );
    }
}
