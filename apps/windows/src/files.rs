//! Files the application keeps between runs: written atomically, with a
//! `.bak` copy that only ever holds a file that decoded, a damaged file kept
//! aside once before anything replaces it, and a lock shared by every Agent
//! Burn process that writes the same folder.

use std::{
    fs::File,
    io::{self, Write},
    path::{Path, PathBuf},
    sync::{
        atomic::{AtomicU64, Ordering},
        mpsc,
    },
    thread,
    time::{Duration, SystemTime, UNIX_EPOCH},
};

use serde::{Serialize, de::DeserializeOwned};
use windows::{
    Win32::Storage::FileSystem::{MOVEFILE_REPLACE_EXISTING, MOVEFILE_WRITE_THROUGH, MoveFileExW},
    core::PCWSTR,
};

use crate::native::wide;

/// Tries `operation` again while it fails for a moment only: a file another
/// program (often an antivirus scanning a file just written) holds open.
pub fn retry_transient<T>(mut operation: impl FnMut() -> io::Result<T>) -> io::Result<T> {
    const ACCESS_DENIED: i32 = 5;
    const SHARING_VIOLATION: i32 = 32;
    let mut attempt = 0;
    loop {
        match operation() {
            Err(error)
                if attempt < 10
                    && matches!(
                        error.raw_os_error(),
                        Some(ACCESS_DENIED | SHARING_VIOLATION)
                    ) =>
            {
                attempt += 1;
                thread::sleep(Duration::from_millis(20 * attempt));
            }
            result => return result,
        }
    }
}

/// Writes `bytes` to a temporary file beside `path`, then moves it over
/// `path`, so a failed write leaves the previous file whole.
pub fn write_atomically(path: &Path, bytes: &[u8]) -> io::Result<()> {
    static WRITES: AtomicU64 = AtomicU64::new(0);
    let folder = path.parent().unwrap_or(Path::new("."));
    std::fs::create_dir_all(folder)?;
    let name = path
        .file_name()
        .map_or_else(|| "file".into(), |name| name.to_string_lossy());
    let temporary = folder.join(format!(
        ".{name}.{}-{}.tmp",
        std::process::id(),
        WRITES.fetch_add(1, Ordering::Relaxed)
    ));
    let written = File::create(&temporary).and_then(|mut file| {
        file.write_all(bytes)?;
        file.sync_all()
    });
    if let Err(error) = written {
        let _ = std::fs::remove_file(&temporary);
        return Err(error);
    }
    let (source, destination) = (wide(&temporary), wide(path));
    let moved = retry_transient(|| {
        unsafe {
            MoveFileExW(
                PCWSTR::from_raw(source.as_ptr()),
                PCWSTR::from_raw(destination.as_ptr()),
                MOVEFILE_REPLACE_EXISTING | MOVEFILE_WRITE_THROUGH,
            )
        }
        .map_err(|error| io::Error::from_raw_os_error(error.code().0 & 0xFFFF))
    });
    moved.map_err(|error| {
        let _ = std::fs::remove_file(&temporary);
        io::Error::new(
            error.kind(),
            format!("{} could not be replaced: {error}", path.display()),
        )
    })?;
    remove_abandoned(folder, &name);
    Ok(())
}

/// A temporary file older than this was abandoned by a write that never
/// finished (the process ended): it holds no data the file lacks.
const ABANDONED_AFTER: std::time::Duration = std::time::Duration::from_secs(10 * 60);

/// Removes the temporary files of `name` that interrupted writes left.
fn remove_abandoned(folder: &Path, name: &str) {
    let prefix = format!(".{name}.");
    let Ok(entries) = std::fs::read_dir(folder) else {
        return;
    };
    for entry in entries.filter_map(Result::ok) {
        let file_name = entry.file_name().to_string_lossy().into_owned();
        let old = entry
            .metadata()
            .and_then(|metadata| metadata.modified())
            .ok()
            .and_then(|modified| modified.elapsed().ok())
            .is_some_and(|age| age > ABANDONED_AFTER);
        if file_name.starts_with(&prefix) && file_name.ends_with(".tmp") && old {
            let _ = std::fs::remove_file(entry.path());
        }
    }
}

/// Keeps `content`, the damaged `path`, beside it as
/// `<stem>.damaged-<seconds>.<extension>`; the same content is kept once,
/// however often it is found.
pub fn keep_aside(path: &Path, content: &[u8]) -> io::Result<PathBuf> {
    let folder = path.parent().unwrap_or(Path::new("."));
    let stem = path
        .file_stem()
        .map_or_else(String::new, |stem| stem.to_string_lossy().into_owned());
    let extension = path.extension().map_or_else(String::new, |extension| {
        extension.to_string_lossy().into_owned()
    });
    let prefix = format!("{stem}.damaged-");
    if let Ok(entries) = std::fs::read_dir(folder) {
        let kept = entries
            .filter_map(Result::ok)
            .map(|entry| entry.path())
            .filter(|kept| {
                kept.file_name()
                    .and_then(|name| name.to_str())
                    .is_some_and(|name| name.starts_with(&prefix))
            })
            .find(|kept| std::fs::read(kept).is_ok_and(|bytes| bytes == content));
        if let Some(kept) = kept {
            return Ok(kept);
        }
    }
    let seconds = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |since| since.as_secs());
    let mut kept = folder.join(format!("{prefix}{seconds}.{extension}"));
    let mut attempt = 1;
    while kept.exists() {
        kept = folder.join(format!("{prefix}{seconds}-{attempt}.{extension}"));
        attempt += 1;
    }
    std::fs::write(&kept, content)?;
    Ok(kept)
}

/// A stored document with a format version; a newer version than the
/// application knows is never read nor replaced.
pub trait Versioned: Serialize + DeserializeOwned {
    const VERSION: u32;
    fn version(&self) -> u32;
}

/// What a stored file holds.
#[derive(Debug)]
pub enum Stored<T> {
    Missing,
    Found {
        value: T,
        /// The main file was damaged or gone; this came from its `.bak` copy.
        from_backup: bool,
    },
    /// Damaged, with no usable copy. Saving keeps the damaged file aside.
    Damaged(String),
    /// Unreadable, or written by a newer version: never saved over.
    Locked(String),
}

pub fn backup_path(path: &Path) -> PathBuf {
    let mut name = path.as_os_str().to_owned();
    name.push(".bak");
    PathBuf::from(name)
}

enum Read<T> {
    Missing,
    Value(T),
    Damaged(String),
    Locked(String),
}

fn read<T: Versioned>(path: &Path) -> Read<T> {
    let bytes = match std::fs::read(path) {
        Ok(bytes) => bytes,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Read::Missing,
        Err(error) => {
            return Read::Locked(format!("{} could not be read: {error}", path.display()));
        }
    };
    decode(&bytes, path)
}

fn decode<T: Versioned>(bytes: &[u8], path: &Path) -> Read<T> {
    // The version first, so a newer format is recognized even when the
    // rest no longer decodes.
    #[derive(serde::Deserialize)]
    struct Header {
        version: Option<u32>,
    }
    if let Ok(Header {
        version: Some(version),
    }) = serde_json::from_slice::<Header>(bytes)
        && version > T::VERSION
    {
        return Read::Locked(format!(
            "{} was written by a newer Agent Burn (format {version})",
            path.display()
        ));
    }
    match serde_json::from_slice::<T>(bytes) {
        Ok(value) if value.version() == T::VERSION => Read::Value(value),
        Ok(value) => Read::Damaged(format!(
            "{} has an unknown format {}",
            path.display(),
            value.version()
        )),
        Err(error) => Read::Damaged(format!("{} is damaged: {error}", path.display())),
    }
}

/// Reads `path`, or its `.bak` copy when the main file is missing or damaged.
pub fn load<T: Versioned>(path: &Path) -> Stored<T> {
    let damage = match read::<T>(path) {
        Read::Value(value) => {
            return Stored::Found {
                value,
                from_backup: false,
            };
        }
        Read::Locked(reason) => return Stored::Locked(reason),
        Read::Missing => None,
        Read::Damaged(reason) => Some(reason),
    };
    match read::<T>(&backup_path(path)) {
        Read::Value(value) => Stored::Found {
            value,
            from_backup: true,
        },
        Read::Missing => damage.map_or(Stored::Missing, Stored::Damaged),
        Read::Damaged(reason) | Read::Locked(reason) => Stored::Damaged(damage.unwrap_or(reason)),
    }
}

/// Saves `value` to `path`. The file it replaces becomes the `.bak` copy when
/// it decodes, and is kept aside when it is damaged: the kept copy is
/// returned. Nothing is written over an unreadable file or one from a newer
/// version.
pub fn save<T: Versioned>(path: &Path, value: &T) -> Result<Option<PathBuf>, String> {
    let mut kept = None;
    match std::fs::read(path) {
        Ok(previous) => match decode::<T>(&previous, path) {
            Read::Value(_) => {
                let backup = backup_path(path);
                if std::fs::read(&backup).ok().as_deref() != Some(previous.as_slice()) {
                    write_atomically(&backup, &previous).map_err(|error| {
                        format!(
                            "the copy {} could not be written: {error}",
                            backup.display()
                        )
                    })?;
                }
            }
            Read::Locked(reason) => return Err(reason),
            Read::Damaged(_) | Read::Missing => {
                kept = Some(keep_aside(path, &previous).map_err(|error| {
                    format!(
                        "the damaged {} could not be kept aside: {error}",
                        path.display()
                    )
                })?);
            }
        },
        Err(error) if error.kind() == io::ErrorKind::NotFound => {}
        Err(error) => return Err(format!("{} could not be read: {error}", path.display())),
    }
    let bytes = serde_json::to_vec(value).map_err(|error| error.to_string())?;
    write_atomically(path, &bytes).map_err(|error| error.to_string())?;
    Ok(kept)
}

/// An exclusive lock on `<folder>\<name>.lock`, held until dropped; other
/// Agent Burn processes that write the same files wait for it, in turn.
/// Windows releases it if the process ends.
#[derive(Debug)]
pub struct Lock {
    file: File,
}

impl Lock {
    /// Waits at most `wait` for the lock; a zero `wait` only tries.
    pub fn acquire(folder: &Path, name: &str, wait: Duration) -> io::Result<Self> {
        std::fs::create_dir_all(folder)?;
        let file = std::fs::OpenOptions::new()
            .create(true)
            .truncate(false)
            .read(true)
            .write(true)
            .open(folder.join(format!("{name}.lock")))?;
        let busy = || {
            io::Error::new(
                io::ErrorKind::TimedOut,
                "another Agent Burn process is still writing its history",
            )
        };
        match file.try_lock() {
            Ok(()) => return Ok(Self { file }),
            Err(std::fs::TryLockError::WouldBlock) if wait.is_zero() => return Err(busy()),
            Err(std::fs::TryLockError::WouldBlock) => {}
            Err(std::fs::TryLockError::Error(error)) => return Err(error),
        }
        // Windows grants a blocking request in turn, so no writer starves
        // as it could by trying again and again. The wait runs on its own
        // thread to bound it; given up, the lock is released when granted.
        let (sender, granted) = mpsc::channel();
        thread::spawn(move || {
            let locked = file.lock().map(|()| file);
            if let Err(mpsc::SendError(Ok(file))) = sender.send(locked) {
                let _ = file.unlock();
            }
        });
        match granted.recv_timeout(wait) {
            Ok(Ok(file)) => Ok(Self { file }),
            Ok(Err(error)) => Err(error),
            Err(_) => Err(busy()),
        }
    }
}

impl Drop for Lock {
    fn drop(&mut self) {
        let _ = self.file.unlock();
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use serde::Deserialize;

    use super::*;

    #[derive(Debug, PartialEq, Serialize, Deserialize)]
    struct Note {
        version: u32,
        text: String,
    }

    impl Versioned for Note {
        const VERSION: u32 = 1;
        fn version(&self) -> u32 {
            self.version
        }
    }

    fn note(text: &str) -> Note {
        Note {
            version: 1,
            text: text.into(),
        }
    }

    pub fn set_read_only(path: &Path, read_only: bool) {
        let mut permissions = std::fs::metadata(path).unwrap().permissions();
        permissions.set_readonly(read_only);
        std::fs::set_permissions(path, permissions).unwrap();
    }

    fn found(stored: Stored<Note>) -> (String, bool) {
        match stored {
            Stored::Found { value, from_backup } => (value.text, from_backup),
            other => panic!("expected a value, got {other:?}"),
        }
    }

    fn damaged_copies(folder: &Path) -> Vec<PathBuf> {
        std::fs::read_dir(folder)
            .unwrap()
            .map(|entry| entry.unwrap().path())
            .filter(|path| path.to_string_lossy().contains(".damaged-"))
            .collect()
    }

    #[test]
    fn the_copy_holds_the_previous_valid_file() {
        let folder = tempfile::tempdir().unwrap();
        let path = folder.path().join("note.json");

        save(&path, &note("first")).unwrap();
        save(&path, &note("second")).unwrap();

        assert_eq!(found(load(&path)), ("second".into(), false));
        let backup: Note =
            serde_json::from_slice(&std::fs::read(backup_path(&path)).unwrap()).unwrap();
        assert_eq!(backup.text, "first");
    }

    #[test]
    fn a_damaged_file_falls_back_to_its_copy_and_is_kept_aside_once() {
        let folder = tempfile::tempdir().unwrap();
        let path = folder.path().join("note.json");
        save(&path, &note("first")).unwrap();
        save(&path, &note("second")).unwrap();
        std::fs::write(&path, b"{\"version\":1,\"text\":").unwrap();

        assert_eq!(found(load(&path)), ("first".into(), true));
        save(&path, &note("third")).unwrap();
        // The copy was not replaced by the damaged file.
        let backup: Note =
            serde_json::from_slice(&std::fs::read(backup_path(&path)).unwrap()).unwrap();
        assert_eq!(backup.text, "first");
        save(&path, &note("fourth")).unwrap();

        assert_eq!(found(load(&path)), ("fourth".into(), false));
        let kept = damaged_copies(folder.path());
        assert_eq!(kept.len(), 1);
        assert_eq!(
            std::fs::read(&kept[0]).unwrap(),
            b"{\"version\":1,\"text\":"
        );
    }

    #[test]
    fn a_newer_format_is_neither_read_nor_replaced() {
        let folder = tempfile::tempdir().unwrap();
        let path = folder.path().join("note.json");
        let newer = b"{\"version\":2,\"text\":\"from the future\",\"more\":[]}";
        std::fs::write(&path, newer).unwrap();

        assert!(matches!(load::<Note>(&path), Stored::Locked(_)));
        assert!(save(&path, &note("older")).is_err());
        assert_eq!(std::fs::read(&path).unwrap(), newer);
    }

    #[test]
    fn a_failed_save_leaves_the_last_valid_file() {
        let folder = tempfile::tempdir().unwrap();
        let path = folder.path().join("note.json");
        save(&path, &note("kept")).unwrap();
        // A read-only file cannot be replaced.
        set_read_only(&path, true);

        assert!(save(&path, &note("lost")).is_err());

        set_read_only(&path, false);
        assert_eq!(found(load(&path)), ("kept".into(), false));
        let leftovers = std::fs::read_dir(folder.path())
            .unwrap()
            .filter(|entry| entry.as_ref().unwrap().path().extension() == Some("tmp".as_ref()))
            .count();
        assert_eq!(leftovers, 0, "no temporary file left behind");
    }

    #[test]
    fn nothing_and_damage_without_a_copy_are_told_apart() {
        let folder = tempfile::tempdir().unwrap();
        let path = folder.path().join("note.json");
        assert!(matches!(load::<Note>(&path), Stored::Missing));

        std::fs::write(&path, b"not json").unwrap();
        assert!(matches!(load::<Note>(&path), Stored::Damaged(_)));
    }

    #[test]
    fn a_second_writer_waits_for_the_lock() {
        let folder = tempfile::tempdir().unwrap();
        let first = Lock::acquire(folder.path(), "history", Duration::ZERO).unwrap();

        let busy = Lock::acquire(folder.path(), "history", Duration::from_millis(50));
        assert_eq!(busy.unwrap_err().kind(), io::ErrorKind::TimedOut);

        drop(first);
        Lock::acquire(folder.path(), "history", Duration::ZERO).unwrap();
    }
}
