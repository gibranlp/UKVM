use super::{history::*, linux::*};
use std::{
    fs,
    os::unix::fs::{symlink, PermissionsExt},
};
use ukvm_core::files::{FileReceiver, FileSource, TransferStatus};
use ukvm_protocol::files::{Entry, Kind, Offer, MAX_BYTES};
fn offer(name: &str) -> Offer {
    Offer {
        id: "a".repeat(32),
        entries: vec![Entry {
            path: vec![name.into()],
            kind: Kind::File,
            size: 0,
        }],
        total_bytes: 0,
    }
}
#[test]
fn unsafe_paths_symlinks_special_objects_and_shared_receive_directory_rejected() {
    let t = tempfile::tempdir().unwrap();
    let p = t.path();
    fs::write(p.join("plain"), b"data").unwrap();
    symlink(p.join("plain"), p.join("link")).unwrap();
    assert!(Source::new(&[p.join("link")], "a".repeat(32)).is_err());
    rustix::fs::mknodat(
        rustix::fs::CWD,
        p.join("fifo"),
        rustix::fs::FileType::Fifo,
        rustix::fs::Mode::from_bits_truncate(0o600),
        0,
    )
    .unwrap();
    assert!(Source::new(&[p.join("fifo")], "a".repeat(32)).is_err());
    symlink(p, p.join("linked-folder")).unwrap();
    assert!(Receiver::new(&p.join("linked-folder/receive"), offer("x")).is_err());
    let shared = p.join("shared");
    fs::create_dir(&shared).unwrap();
    fs::set_permissions(&shared, fs::Permissions::from_mode(0o777)).unwrap();
    assert!(Receiver::new(&shared, offer("x")).is_err());
    for name in ["../x", "/root", "C:\\root", "..", "a/b"] {
        assert!(Receiver::new(&p.join("receive"), offer(name)).is_err());
    }
}
#[test]
fn staging_cleanup_private_modes_and_disk_space_limit() {
    let t = tempfile::tempdir().unwrap();
    let root = t.path().join("receive");
    {
        let mut r = Receiver::new(&root, offer("empty")).unwrap();
        r.finish_file(0).unwrap();
        assert!(!root.join("empty").exists());
    }
    assert!(root.read_dir().unwrap().next().is_none());
    let mut r = Receiver::new(&root, offer("empty")).unwrap();
    r.finish_file(0).unwrap();
    let name = r.commit().unwrap();
    assert_eq!(name, "empty");
    assert_eq!(
        fs::metadata(root.join(name)).unwrap().permissions().mode() & 0o777,
        0o600
    );
    let mut huge = offer("huge");
    huge.total_bytes = MAX_BYTES;
    huge.entries[0].size = MAX_BYTES;
    assert!(Receiver::new(&root, huge)
        .unwrap_err_safe()
        .contains("space"));
}
trait ErrorOnly {
    fn unwrap_err_safe(self) -> String;
}
impl<T> ErrorOnly for Result<T, String> {
    fn unwrap_err_safe(self) -> String {
        match self {
            Err(e) => e,
            Ok(_) => panic!("unexpected success"),
        }
    }
}
fn row(id: usize) -> TransferStatus {
    TransferStatus {
        id: format!("{id:032x}"),
        peer_id: "f".repeat(64),
        peer_name: "peer".into(),
        direction: "send".into(),
        name: "file.bin".into(),
        state: "transferring".into(),
        bytes: 0,
        total_bytes: 5,
        entries: 1,
        started: 0,
        updated: 0,
        destination: None,
        error: None,
    }
}
#[test]
fn history_cancel_restart_bounded_and_payload_free() {
    let t = tempfile::tempdir().unwrap();
    let h = History::new(t.path()).unwrap();
    let r = h.begin(row(1)).unwrap();
    assert_eq!(h.list().unwrap()[0].state, "transferring");
    h.cancel(&r.status.id).unwrap();
    assert!(h.cancelled(&r.status.id));
    assert!(h.cancel("../bad").is_err());
    drop(r);
    assert_eq!(h.list().unwrap()[0].state, "interrupted");
    assert!(h.cancel(&format!("{:032x}", 1)).is_err());
    for n in 2..100 {
        let mut r = h.begin(row(n)).unwrap();
        r.status.state = "completed".into();
        r.update(true).unwrap();
    }
    assert_eq!(h.list().unwrap().len(), HISTORY_LIMIT);
    let json = fs::read_to_string(t.path().join("file-transfers/history.json")).unwrap();
    assert!(!json.contains("payload"));
    assert!(!json.contains("sha256"));
    assert_eq!(
        fs::metadata(t.path().join("file-transfers/history.json"))
            .unwrap()
            .permissions()
            .mode()
            & 0o777,
        0o600
    );
}
#[test]
fn sources_bound_metadata_and_detect_file_changes() {
    let t = tempfile::tempdir().unwrap();
    let p = t.path().join("file");
    fs::write(&p, b"small").unwrap();
    let mut source = Source::new(std::slice::from_ref(&p), "a".repeat(32)).unwrap();
    assert_eq!(source.offer().total_bytes, 5);
    fs::remove_file(&p).unwrap();
    symlink("/etc/passwd", &p).unwrap();
    assert!(source.read_chunk(0).is_err());
    assert!(Source::new(&vec![p; 65], "a".repeat(32)).is_err());
}
