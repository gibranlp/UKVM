use super::*;
use ukvm_core::files::{FileReceiver, TransferStatus};
use ukvm_network::{
    files::{self, Check, Observer},
    trust::{permission_guard, SessionPermission},
};
use ukvm_platform::files::{
    history::{History, Record},
    linux::Receiver,
};
struct Authorized {
    inner: Receiver,
    dir: PathBuf,
    peer: String,
    history: History,
    id: String,
}
impl Authorized {
    fn guard(&self) -> Result<std::fs::File, String> {
        if self.history.cancelled(&self.id) {
            return Err("transfer cancelled".into());
        }
        permission_guard(&self.dir, &self.peer, SessionPermission::ReceiveFiles)
    }
}
impl FileReceiver for Authorized {
    fn write_chunk(&mut self, index: usize, bytes: &[u8]) -> Result<(), String> {
        let _guard = self.guard()?;
        self.inner.write_chunk(index, bytes)
    }
    fn finish_file(&mut self, index: usize) -> Result<(), String> {
        let _guard = self.guard()?;
        self.inner.finish_file(index)
    }
    fn commit(&mut self) -> Result<String, String> {
        let _guard = self.guard()?;
        self.inner.commit()
    }
}
pub(super) async fn session<S: tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin>(
    context: &Context,
    peer: &str,
    stream: &mut S,
    stop: watch::Receiver<bool>,
) -> Result<(), String> {
    let history = History::new(&context.peers.trust_dir)?;
    let row: Arc<Mutex<Option<Record>>> = Arc::new(Mutex::new(None));
    let dir = context.peers.trust_dir.clone();
    let peer = peer.to_owned();
    let other = peer.clone();
    let state = history.clone();
    let observed = Arc::clone(&row);
    let check: Check = Arc::new(move || {
        if *stop.borrow() {
            return Err("runtime stopping; transfer cancelled".into());
        }
        TrustStore::load(&dir)?.authorize(&other, SessionPermission::ReceiveFiles)?;
        if let Some(record) = observed
            .lock()
            .map_err(|_| "transfer history lock failed")?
            .as_ref()
        {
            if state.cancelled(&record.status.id) {
                return Err("transfer cancelled".into());
            }
        }
        Ok(())
    });
    let observed = Arc::clone(&row);
    let observer: Observer = Arc::new(move |progress| {
        if let Some(record) = observed
            .lock()
            .map_err(|_| "transfer history lock failed")?
            .as_mut()
        {
            record.status.state = progress.state.into();
            record.status.bytes = progress.bytes;
            record.status.destination = progress.destination;
            record.update(false)?;
        }
        Ok(())
    });
    let name = TrustStore::load(&context.peers.trust_dir)?
        .find_id(&peer)
        .ok_or("peer disappeared")?
        .alias
        .clone();
    let destination = context
        .peers
        .config
        .file_transfer
        .receive_directory
        .as_ref()
        .map(PathBuf::from)
        .map(Ok)
        .unwrap_or_else(ukvm_platform::files::linux::default_receive_directory)?;
    let dir = context.peers.trust_dir.clone();
    let recorded = Arc::clone(&row);
    let result = files::receive(stream, check, observer, move |offer| {
        let _guard = permission_guard(&dir, &peer, SessionPermission::ReceiveFiles)?;
        let id = offer.id.clone();
        let label = if offer.entries.iter().filter(|e| e.path.len() == 1).count() == 1 {
            offer.entries[0].path[0].clone()
        } else {
            format!("{} items", offer.entries.len())
        };
        let record = history.begin(TransferStatus {
            id: id.clone(),
            peer_id: peer.clone(),
            peer_name: name,
            direction: "receive".into(),
            name: label,
            state: "preparing".into(),
            bytes: 0,
            total_bytes: offer.total_bytes,
            entries: offer.entries.len(),
            started: 0,
            updated: 0,
            destination: None,
            error: None,
        })?;
        *recorded
            .lock()
            .map_err(|_| "transfer history lock failed")? = Some(record);
        let inner = Receiver::new(&destination, offer)?;
        Ok(Box::new(Authorized {
            inner,
            dir,
            peer,
            history,
            id,
        }) as Box<dyn FileReceiver>)
    })
    .await;
    let error = result.as_ref().err().cloned();
    tokio::task::spawn_blocking(move || {
        if let Some(record) = row
            .lock()
            .map_err(|_| "transfer history lock failed")?
            .as_mut()
        {
            if record.status.state != "completed" {
                if let Some(error) = error {
                    record.status.state = if error.contains("cancel") || error.contains("stopping")
                    {
                        "cancelled"
                    } else {
                        "failed"
                    }
                    .into();
                    record.status.error = Some(error);
                }
            }
            record.update(true)?;
        }
        Ok::<_, String>(())
    })
    .await
    .map_err(|_| "transfer history worker failed")??;
    result.map(|_| ())
}
#[cfg(test)]
mod tests {
    use super::*;
    use ukvm_network::identity::Identity;
    use ukvm_protocol::files::{Entry, Kind, Offer};
    #[test]
    fn real_permission_revocation_prevents_writes_and_publication() {
        let temp = tempfile::tempdir().unwrap();
        let dir = temp.path().join("trust");
        let identity = Identity::load_or_create(&temp.path().join("identity")).unwrap();
        let mut trust = TrustStore::default();
        trust
            .import(
                &dir,
                "peer".into(),
                None,
                &std::fs::read(identity.cert_path).unwrap(),
            )
            .unwrap();
        trust
            .set_permission(&dir, &identity.id, SessionPermission::ReceiveFiles, true)
            .unwrap();
        let history = History::new(&dir).unwrap();
        let root = temp.path().join("received");
        let offer = Offer {
            id: "b".repeat(32),
            entries: vec![Entry {
                path: vec!["file.bin".into()],
                kind: Kind::File,
                size: 2,
            }],
            total_bytes: 2,
        };
        let mut receiver = Authorized {
            inner: Receiver::new(&root, offer.clone()).unwrap(),
            dir: dir.clone(),
            peer: identity.id.clone(),
            history,
            id: offer.id,
        };
        receiver.write_chunk(0, &[1]).unwrap();
        {
            let _lock = ukvm_network::trust::edit_lock(&dir).unwrap();
            trust
                .set_permission(&dir, &identity.id, SessionPermission::ReceiveFiles, false)
                .unwrap();
        }
        assert!(receiver.write_chunk(0, &[2]).is_err());
        assert!(receiver.finish_file(0).is_err());
        assert!(receiver.commit().is_err());
        drop(receiver);
        assert!(root.read_dir().unwrap().next().is_none());
        assert!(
            !TrustStore::load(&dir)
                .unwrap()
                .find_id(&identity.id)
                .unwrap()
                .permissions
                .receive_files
        );
    }
}
