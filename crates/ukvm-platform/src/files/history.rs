use fs2::FileExt;
use std::{
    fs::{self, File, OpenOptions},
    io::{Read, Write},
    os::unix::fs::{OpenOptionsExt, PermissionsExt},
    path::{Path, PathBuf},
    time::{SystemTime, UNIX_EPOCH},
};
use ukvm_core::files::TransferStatus;
use ukvm_protocol::files::valid_id;
pub const HISTORY_LIMIT: usize = 64;
fn now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}
#[derive(Clone)]
pub struct History {
    dir: PathBuf,
}
pub struct Record {
    history: History,
    pub status: TransferStatus,
    _lock: File,
    last_write: std::time::Instant,
}
impl History {
    pub fn new(dir: &Path) -> Result<Self, String> {
        let dir = dir.join("file-transfers");
        fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
        if fs::symlink_metadata(&dir)
            .map_err(|e| e.to_string())?
            .file_type()
            .is_symlink()
        {
            return Err("linked file transfer state is not supported".into());
        }
        fs::set_permissions(&dir, fs::Permissions::from_mode(0o700)).map_err(|e| e.to_string())?;
        Ok(Self { dir })
    }
    fn lock(&self, name: &str) -> Result<File, String> {
        OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .mode(0o600)
            .custom_flags(rustix::fs::OFlags::NOFOLLOW.bits() as i32)
            .open(self.dir.join(name))
            .map_err(|_| "file transfer state unavailable".into())
    }
    fn read(&self) -> Result<Vec<TransferStatus>, String> {
        let path = self.dir.join("history.json");
        let mut file = match OpenOptions::new()
            .read(true)
            .custom_flags(rustix::fs::OFlags::NOFOLLOW.bits() as i32)
            .open(path)
        {
            Ok(f) => f,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(vec![]),
            Err(_) => return Err("file transfer history unavailable".into()),
        };
        let mut source = String::new();
        Read::by_ref(&mut file)
            .take(1024 * 1024 + 1)
            .read_to_string(&mut source)
            .map_err(|_| "file transfer history unreadable")?;
        if source.len() > 1024 * 1024 {
            return Err("file transfer history exceeds size limit".into());
        }
        let rows: Vec<TransferStatus> =
            serde_json::from_str(&source).map_err(|_| "file transfer history malformed")?;
        if rows.len() > HISTORY_LIMIT || rows.iter().any(|r| !valid_id(&r.id)) {
            return Err("file transfer history invalid".into());
        }
        Ok(rows)
    }
    fn write(&self, rows: &[TransferStatus]) -> Result<(), String> {
        let mut staged = tempfile::NamedTempFile::new_in(&self.dir).map_err(|e| e.to_string())?;
        serde_json::to_writer(&mut staged, rows)
            .map_err(|_| "cannot serialize transfer history")?;
        staged.flush().map_err(|e| e.to_string())?;
        staged.as_file().sync_all().map_err(|e| e.to_string())?;
        staged
            .persist(self.dir.join("history.json"))
            .map_err(|e| e.error.to_string())?;
        Ok(())
    }
    fn active(&self, row: &TransferStatus) -> bool {
        if !row.active() {
            return false;
        }
        self.lock(&(row.id.clone() + ".lock"))
            .is_ok_and(|lock| FileExt::try_lock_shared(&lock).is_err())
    }
    pub fn list(&self) -> Result<Vec<TransferStatus>, String> {
        let lock = self.lock(".history.lock")?;
        FileExt::lock_shared(&lock).map_err(|e| e.to_string())?;
        let mut rows = self.read()?;
        for row in &mut rows {
            if row.active() && !self.active(row) {
                row.state = "interrupted".into();
                row.error =
                    Some("Transfer owner stopped; no unverified files were finalized".into());
            }
        }
        Ok(rows)
    }
    pub fn begin(&self, mut status: TransferStatus) -> Result<Record, String> {
        if !valid_id(&status.id) {
            return Err("invalid transfer identifier".into());
        }
        let global = self.lock(".history.lock")?;
        global.lock_exclusive().map_err(|e| e.to_string())?;
        let mut rows = self.read()?;
        if rows
            .iter()
            .filter(|r| r.direction == status.direction && self.active(r))
            .count()
            >= 4
        {
            return Err("file transfer capacity reached (four per direction)".into());
        }
        if rows.iter().any(|r| r.id == status.id) {
            return Err("transfer identifier already recorded".into());
        }
        let lock = self.lock(&(status.id.clone() + ".lock"))?;
        lock.try_lock_exclusive()
            .map_err(|_| "transfer identifier busy")?;
        status.started = now();
        status.updated = status.started;
        while rows.len() >= HISTORY_LIMIT {
            let index = rows
                .iter()
                .position(|r| !self.active(r))
                .ok_or("transfer history is full")?;
            let removed = rows.remove(index);
            let _ = fs::remove_file(self.dir.join(removed.id.clone() + ".cancel"));
            let _ = fs::remove_file(self.dir.join(removed.id + ".lock"));
        }
        rows.push(status.clone());
        self.write(&rows)?;
        Ok(Record {
            history: self.clone(),
            status,
            _lock: lock,
            last_write: std::time::Instant::now(),
        })
    }
    pub fn cancel(&self, id: &str) -> Result<(), String> {
        if !valid_id(id) {
            return Err("invalid transfer identifier".into());
        }
        let global = self.lock(".history.lock")?;
        global.lock_exclusive().map_err(|e| e.to_string())?;
        let rows = self.read()?;
        let row = rows.iter().find(|r| r.id == id).ok_or("unknown transfer")?;
        if !self.active(row) {
            return Err("transfer is no longer active".into());
        }
        let _ = self.lock(&(id.to_string() + ".cancel"))?;
        Ok(())
    }
    pub fn cancelled(&self, id: &str) -> bool {
        !valid_id(id)
            || self
                .dir
                .join(id.to_string() + ".cancel")
                .try_exists()
                .unwrap_or(true)
    }
}
impl Record {
    pub fn update(&mut self, force: bool) -> Result<(), String> {
        if !force && self.last_write.elapsed() < std::time::Duration::from_millis(500) {
            return Ok(());
        }
        self.status.updated = now();
        let global = self.history.lock(".history.lock")?;
        global.lock_exclusive().map_err(|e| e.to_string())?;
        let mut rows = self.history.read()?;
        let row = rows
            .iter_mut()
            .find(|r| r.id == self.status.id)
            .ok_or("transfer history entry missing")?;
        *row = self.status.clone();
        self.history.write(&rows)?;
        self.last_write = std::time::Instant::now();
        Ok(())
    }
}
