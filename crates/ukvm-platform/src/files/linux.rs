use rustix::fs::{self, Mode, OFlags, RenameFlags};
use std::{
    fs::File,
    io::{Read, Write},
    os::{
        fd::{AsRawFd, OwnedFd},
        unix::fs::MetadataExt,
    },
    path::{Component, Path, PathBuf},
};
use ukvm_core::files::{FileReceiver, FileSource};
use ukvm_protocol::files::{component, Entry, Kind, Offer, CHUNK_BYTES, MAX_ENTRIES};
fn error(e: impl std::fmt::Display) -> String {
    format!("file storage: {e}")
}
fn directory(parent: &OwnedFd, name: &std::ffi::OsStr, create: bool) -> Result<OwnedFd, String> {
    if create {
        match fs::mkdirat(parent, name, Mode::from_bits_truncate(0o700)) {
            Ok(()) => {}
            Err(rustix::io::Errno::EXIST) => {}
            Err(e) => return Err(error(e)),
        }
    }
    fs::openat(
        parent,
        name,
        OFlags::RDONLY | OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::CLOEXEC,
        Mode::empty(),
    )
    .map_err(error)
}
fn absolute(path: &Path) -> Result<PathBuf, String> {
    let path = if path.is_absolute() {
        path.to_owned()
    } else {
        std::env::current_dir().map_err(error)?.join(path)
    };
    let mut out = PathBuf::from("/");
    for part in path.components() {
        match part {
            Component::Normal(s) => out.push(s),
            Component::ParentDir => {
                out.pop();
            }
            Component::RootDir | Component::CurDir => {}
            _ => return Err("invalid local filesystem path".into()),
        }
    }
    Ok(out)
}
fn open_directory(path: &Path, create: bool) -> Result<OwnedFd, String> {
    let mut fd: OwnedFd = File::open("/").map_err(error)?.into();
    for part in absolute(path)?.components() {
        if let Component::Normal(s) = part {
            fd = directory(&fd, s, create)?;
        }
    }
    Ok(fd)
}
fn parent(root: &OwnedFd, path: &[String]) -> Result<OwnedFd, String> {
    let mut fd = rustix::io::dup(root).map_err(error)?;
    for part in path {
        fd = directory(&fd, part.as_ref(), false)?;
    }
    Ok(fd)
}
fn regular(parent: &OwnedFd, name: &str) -> Result<File, String> {
    component(name)?;
    let fd = fs::openat(
        parent,
        name,
        OFlags::RDONLY | OFlags::NOFOLLOW | OFlags::NONBLOCK | OFlags::CLOEXEC,
        Mode::empty(),
    )
    .map_err(error)?;
    let file = File::from(fd);
    if !file.metadata().map_err(error)?.is_file() {
        return Err("symlinks and special files cannot be sent".into());
    }
    Ok(file)
}
pub fn default_receive_directory() -> Result<PathBuf, String> {
    dirs::download_dir()
        .or_else(|| dirs::home_dir().map(|p| p.join("Downloads")))
        .map(|p| p.join("UKVM"))
        .ok_or("cannot locate user Downloads directory".into())
}
pub struct Source {
    offer: Offer,
    roots: Vec<(String, OwnedFd)>,
    current: Option<(usize, File)>,
}
impl Source {
    pub fn new(paths: &[PathBuf], id: String) -> Result<Self, String> {
        if paths.is_empty() || paths.len() > 64 {
            return Err("send requires between 1 and 64 paths".into());
        }
        let mut source = Self {
            offer: Offer {
                id,
                entries: vec![],
                total_bytes: 0,
            },
            roots: vec![],
            current: None,
        };
        for path in paths {
            let path = absolute(path)?;
            let name = path
                .file_name()
                .and_then(|s| s.to_str())
                .ok_or("source needs a UTF-8 filename")?
                .to_string();
            component(&name)?;
            let root = open_directory(path.parent().ok_or("cannot send filesystem root")?, false)?;
            source.walk(&root, vec![name.clone()])?;
            source.roots.push((name, root));
        }
        source.offer.validate()?;
        Ok(source)
    }
    fn walk(&mut self, base: &OwnedFd, path: Vec<String>) -> Result<(), String> {
        if self.offer.entries.len() >= MAX_ENTRIES || path.len() > 64 {
            return Err("file tree exceeds transfer limits".into());
        }
        let name = path.last().ok_or("missing filename")?;
        component(name)?;
        let fd = fs::openat(
            base,
            name.as_str(),
            OFlags::RDONLY | OFlags::NOFOLLOW | OFlags::NONBLOCK | OFlags::CLOEXEC,
            Mode::empty(),
        )
        .map_err(error)?;
        let file = File::from(fd);
        let metadata = file.metadata().map_err(error)?;
        if metadata.is_file() {
            self.offer.total_bytes = self
                .offer
                .total_bytes
                .checked_add(metadata.len())
                .ok_or("transfer size overflow")?;
            self.offer.entries.push(Entry {
                path,
                kind: Kind::File,
                size: metadata.len(),
            });
        } else if metadata.is_dir() {
            self.offer.entries.push(Entry {
                path: path.clone(),
                kind: Kind::Directory,
                size: 0,
            });
            let fd: OwnedFd = file.into();
            let mut names = Vec::new();
            for entry in fs::Dir::read_from(&fd).map_err(error)? {
                let entry = entry.map_err(error)?;
                if entry.file_name().to_bytes() == b"." || entry.file_name().to_bytes() == b".." {
                    continue;
                }
                if names.len() >= MAX_ENTRIES {
                    return Err("directory exceeds transfer entry limit".into());
                }
                names.push(entry.file_name().to_owned());
            }
            names.sort();
            for name in names {
                let name = name
                    .to_str()
                    .map_err(|_| "source contains non-UTF-8 filename")?;
                if name == "." || name == ".." {
                    continue;
                }
                let mut next = path.clone();
                next.push(name.into());
                self.walk(&fd, next)?;
            }
        } else {
            return Err("symlinks and special files cannot be sent".into());
        }
        Ok(())
    }
}
impl FileSource for Source {
    fn offer(&self) -> &Offer {
        &self.offer
    }
    fn read_chunk(&mut self, index: usize) -> Result<Vec<u8>, String> {
        let entry = self
            .offer
            .entries
            .get(index)
            .ok_or("invalid source file index")?;
        if entry.kind != Kind::File {
            return Err("cannot read directory contents as a file".into());
        }
        if self.current.as_ref().map(|c| c.0) != Some(index) {
            let root = &self
                .roots
                .iter()
                .find(|r| r.0 == entry.path[0])
                .ok_or("missing source root")?
                .1;
            let dir = parent(root, &entry.path[..entry.path.len() - 1])?;
            self.current = Some((
                index,
                regular(&dir, entry.path.last().ok_or("missing source file")?)?,
            ));
        }
        let mut bytes = vec![0; CHUNK_BYTES];
        let size = self
            .current
            .as_mut()
            .ok_or("source file unavailable")?
            .1
            .read(&mut bytes)
            .map_err(error)?;
        bytes.truncate(size);
        Ok(bytes)
    }
}
pub struct Receiver {
    root: OwnedFd,
    stage: OwnedFd,
    stage_name: String,
    offer: Offer,
    current: Option<(usize, File)>,
    committed: bool,
    written: Vec<u64>,
    finished: Vec<bool>,
}
impl Receiver {
    pub fn new(root: &Path, offer: Offer) -> Result<Self, String> {
        offer.validate()?;
        let root = open_directory(root, true)?;
        let metadata = File::from(rustix::io::dup(&root).map_err(error)?)
            .metadata()
            .map_err(error)?;
        if metadata.uid() != rustix::process::geteuid().as_raw() || metadata.mode() & 0o022 != 0 {
            return Err(
                "receive directory must belong to this user and disallow group/other writes".into(),
            );
        }
        let free = fs::fstatvfs(&root).map_err(error)?;
        if free.f_files > 0 && free.f_favail < offer.entries.len() as u64 + 16 {
            return Err("not enough free filesystem entries for transfer".into());
        }
        if offer.total_bytes.saturating_add(64 * 1024 * 1024)
            > free.f_bavail.saturating_mul(free.f_frsize)
        {
            return Err("not enough free space for transfer and reserve".into());
        }
        let random = tempfile::Builder::new()
            .rand_bytes(12)
            .tempfile()
            .map_err(error)?;
        let stage_name = format!(
            ".ukvm-incomplete-{}",
            random
                .path()
                .file_name()
                .ok_or("temporary filename unavailable")?
                .to_string_lossy()
        );
        fs::mkdirat(&root, stage_name.as_str(), Mode::from_bits_truncate(0o700)).map_err(error)?;
        let stage = directory(&root, stage_name.as_ref(), false)?;
        let receiver = Self {
            root,
            stage,
            stage_name,
            offer: offer.clone(),
            current: None,
            committed: false,
            written: vec![0; offer.entries.len()],
            finished: vec![false; offer.entries.len()],
        };
        for entry in &receiver.offer.entries {
            if entry.kind == Kind::Directory {
                let dir = parent(&receiver.stage, &entry.path[..entry.path.len() - 1])?;
                fs::mkdirat(
                    &dir,
                    entry.path.last().ok_or("missing directory")?.as_str(),
                    Mode::from_bits_truncate(0o700),
                )
                .map_err(error)?;
                fs::fsync(&dir).map_err(error)?;
            }
        }
        Ok(receiver)
    }
    fn open_file(&mut self, index: usize) -> Result<&mut File, String> {
        if self.current.as_ref().map(|c| c.0) != Some(index) {
            let entry = self
                .offer
                .entries
                .get(index)
                .ok_or("invalid receive file index")?;
            if entry.kind != Kind::File {
                return Err("invalid receive entry kind".into());
            }
            let dir = parent(&self.stage, &entry.path[..entry.path.len() - 1])?;
            let fd = fs::openat(
                &dir,
                entry.path.last().ok_or("missing filename")?.as_str(),
                OFlags::WRONLY | OFlags::CREATE | OFlags::EXCL | OFlags::NOFOLLOW | OFlags::CLOEXEC,
                Mode::from_bits_truncate(0o600),
            )
            .map_err(error)?;
            self.current = Some((index, File::from(fd)));
        }
        Ok(&mut self.current.as_mut().ok_or("missing receive file")?.1)
    }
}
impl FileReceiver for Receiver {
    fn write_chunk(&mut self, index: usize, bytes: &[u8]) -> Result<(), String> {
        let entry = self.offer.entries.get(index).ok_or("invalid file index")?;
        if self.finished[index]
            || self.written[index].saturating_add(bytes.len() as u64) > entry.size
        {
            return Err("receive size mismatch".into());
        }
        self.open_file(index)?.write_all(bytes).map_err(error)?;
        self.written[index] += bytes.len() as u64;
        Ok(())
    }
    fn finish_file(&mut self, index: usize) -> Result<(), String> {
        if self
            .offer
            .entries
            .get(index)
            .is_none_or(|e| e.size != self.written[index])
            || self.finished[index]
        {
            return Err("receive size mismatch".into());
        }
        self.open_file(index)?.sync_all().map_err(error)?;
        self.finished[index] = true;
        self.current = None;
        let entry = &self.offer.entries[index];
        let dir = parent(&self.stage, &entry.path[..entry.path.len() - 1])?;
        fs::fsync(&dir).map_err(error)?;
        Ok(())
    }
    fn commit(&mut self) -> Result<String, String> {
        if self
            .offer
            .entries
            .iter()
            .enumerate()
            .any(|(i, e)| e.kind == Kind::File && !self.finished[i])
        {
            return Err("cannot publish an incomplete transfer".into());
        }
        fs::fsync(&self.stage).map_err(error)?;
        let roots = self
            .offer
            .entries
            .iter()
            .filter(|e| e.path.len() == 1)
            .collect::<Vec<_>>();
        let single = roots.len() == 1;
        let name = if single {
            roots[0].path[0].clone()
        } else {
            format!("UKVM files {}", self.offer.id)
        };
        let is_file = single && roots[0].kind == Kind::File;
        // El renombrado sin reemplazo hace indivisible la publicación del lote completo.
        for n in 0..10000 {
            let candidate = collision_name(&name, n, is_file);
            let result = if single {
                fs::renameat_with(
                    &self.stage,
                    name.as_str(),
                    &self.root,
                    candidate.as_str(),
                    RenameFlags::NOREPLACE,
                )
            } else {
                fs::renameat_with(
                    &self.root,
                    self.stage_name.as_str(),
                    &self.root,
                    candidate.as_str(),
                    RenameFlags::NOREPLACE,
                )
            };
            match result {
                Ok(()) => {
                    self.committed = true;
                    fs::fsync(&self.root).map_err(error)?;
                    if single {
                        let _ = fs::unlinkat(
                            &self.root,
                            self.stage_name.as_str(),
                            fs::AtFlags::REMOVEDIR,
                        );
                    }
                    return Ok(candidate);
                }
                Err(rustix::io::Errno::EXIST) => {}
                Err(e) => return Err(error(e)),
            }
        }
        Err("too many destination filename collisions".into())
    }
}
fn collision_name(name: &str, n: usize, file: bool) -> String {
    if n == 0 {
        return name.into();
    }
    let suffix = format!(" ({n})");
    let (stem, extension) = if file {
        name.rsplit_once('.')
            .filter(|(stem, ext)| !stem.is_empty() && ext.len() + suffix.len() + 1 < 240)
            .map(|(stem, ext)| (stem, format!(".{ext}")))
            .unwrap_or((name, String::new()))
    } else {
        (name, String::new())
    };
    let limit = 240 - suffix.len() - extension.len();
    let mut end = stem.len().min(limit);
    while !stem.is_char_boundary(end) {
        end -= 1;
    }
    format!("{}{suffix}{extension}", &stem[..end])
}
impl Drop for Receiver {
    fn drop(&mut self) {
        if !self.committed {
            let root = PathBuf::from(format!("/proc/self/fd/{}", self.root.as_raw_fd()));
            let _ = std::fs::remove_dir_all(root.join(&self.stage_name));
        }
    }
}
