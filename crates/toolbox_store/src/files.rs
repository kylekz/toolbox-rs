//! The file-level pieces the guard, the backups and the database share: `file:` URIs, temporary
//! names, connections that never create a file, and SQLite's integrity checks.

use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

use rusqlite::{Connection, OpenFlags};

use crate::error::{Error, Result};

/// A temporary file's name ends in this; it starts with `.<process id>-`.
const TEMPORARY_SUFFIX: &str = ".tmp";

/**
 * A `file:` URI for `path`, to which the caller adds its query. Everything but unreserved
 * characters, `/` and `:` is percent-encoded (SQLite's URI rules give `?`, `#` and `%` meaning);
 * a Windows path's backslashes become slashes, and a drive path gets the empty authority
 * (`file:///D:/…`).
 */
pub(crate) fn file_uri(path: &Path) -> Result<String> {
    let text = path
        .to_str()
        .ok_or_else(|| Error::NotUnicode(path.to_path_buf()))?
        .replace('\\', "/");
    let mut uri = String::from("file://");
    if !text.starts_with('/') {
        uri.push('/');
    }
    for byte in text.bytes() {
        match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'.' | b'_' | b'~' | b'/' | b':' => {
                uri.push(char::from(byte))
            }
            _ => uri.push_str(&format!("%{byte:02X}")),
        }
    }
    Ok(uri)
}

/// `path` with `suffix` added to its file name: SQLite's `app.db-wal` beside `app.db`.
pub(crate) fn sibling(path: &Path, suffix: &str) -> PathBuf {
    let mut name = path.as_os_str().to_owned();
    name.push(suffix);
    PathBuf::from(name)
}

/**
 * Opens `uri` (a `file:` URI) read-only: SQLite neither creates nor writes anything. With
 * `?mode=ro&immutable=1` it doesn't even take locks or look for a WAL, so the file and its folder
 * are left exactly as they are.
 */
pub(crate) fn open_read_only(uri: &str) -> rusqlite::Result<Connection> {
    Connection::open_with_flags(
        uri,
        OpenFlags::SQLITE_OPEN_READ_ONLY
            | OpenFlags::SQLITE_OPEN_URI
            | OpenFlags::SQLITE_OPEN_NO_MUTEX,
    )
}

/**
 * A connection of its own to the existing database file at `path` (its WAL is read too), opened
 * without `SQLITE_OPEN_CREATE`: a missing file is never made empty and read as a new database.
 */
pub(crate) fn open_existing(path: &Path) -> Result<Connection> {
    match fs::metadata(path) {
        Ok(metadata) if metadata.is_file() => {}
        Ok(_) => {
            return Err(Error::io(
                "open",
                path,
                io::Error::new(io::ErrorKind::InvalidInput, "not a file"),
            ));
        }
        Err(error) => return Err(Error::io("open", path, error)),
    }
    Connection::open_with_flags(
        file_uri(path)?,
        OpenFlags::SQLITE_OPEN_READ_WRITE
            | OpenFlags::SQLITE_OPEN_URI
            | OpenFlags::SQLITE_OPEN_NO_MUTEX,
    )
    .map_err(|source| Error::Unavailable {
        path: path.to_path_buf(),
        source,
    })
}

/// How thoroughly [`check`] reads a database.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Check {
    /**
     * `PRAGMA quick_check`: every page and record, without matching indexes to their tables.
     * Run on the live database at every open.
     */
    Quick,
    /// `PRAGMA integrity_check`: everything `Quick` reads, plus every index. Run on each copy.
    Full,
}

/**
 * Runs `check` on the existing database at `path`. `Ok(Err(detail))` is a database SQLite read
 * and found damaged; `Err` is one it couldn't read at all (or a file that isn't a database).
 */
pub(crate) fn check(path: &Path, check: Check) -> Result<Result<(), String>> {
    let pragma = match check {
        Check::Quick => "PRAGMA quick_check",
        Check::Full => "PRAGMA integrity_check",
    };
    let connection = open_existing(path)?;
    let rows = connection
        .prepare(pragma)
        .and_then(|mut statement| {
            statement
                .query_map([], |row| row.get::<_, String>(0))?
                .collect::<rusqlite::Result<Vec<String>>>()
        })
        .map_err(|source| Error::Unavailable {
            path: path.to_path_buf(),
            source,
        })?;
    Ok(if rows.len() == 1 && rows[0] == "ok" {
        Ok(())
    } else {
        Err(rows.join("; "))
    })
}

/// [`check`] as one result: damage becomes [`Error::CheckFailed`].
pub(crate) fn check_copy(path: &Path) -> Result<()> {
    check(path, Check::Full)?.map_err(|detail| Error::CheckFailed {
        path: path.to_path_buf(),
        detail,
    })
}

/**
 * A fresh name in `dir` to write a file under before renaming it into place:
 * `.<process id>-<time>-<counter>.tmp`. The process id tells a crashed run's leftovers from files
 * this process is still writing; the time keeps a reused process id from meeting them.
 */
pub(crate) fn temporary_path(dir: &Path) -> PathBuf {
    static COUNTER: AtomicU64 = AtomicU64::new(0);
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();
    dir.join(format!(
        ".{}-{nanos}-{}{TEMPORARY_SUFFIX}",
        std::process::id(),
        COUNTER.fetch_add(1, Ordering::Relaxed)
    ))
}

/**
 * The process that named `file_name`, when it is a temporary file's name: `.<pid>-<anything>.tmp`,
 * which also covers the `.<pid>-<uuid>.tmp` names slide's own backups used.
 */
fn temporary_owner(file_name: &str) -> Option<u32> {
    let (pid, rest) = file_name
        .strip_prefix('.')?
        .strip_suffix(TEMPORARY_SUFFIX)?
        .split_once('-')?;
    if rest.is_empty() || pid.is_empty() || !pid.bytes().all(|byte| byte.is_ascii_digit()) {
        return None;
    }
    pid.parse().ok()
}

/**
 * Deletes the temporary files in `dir` that another process started and never renamed (a crash,
 * a kill); this process's own are still being written.
 */
pub(crate) fn remove_abandoned_temporaries(dir: &Path) {
    let this_process = std::process::id();
    let Ok(entries) = fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let abandoned = entry
            .file_name()
            .to_str()
            .and_then(temporary_owner)
            .is_some_and(|owner| owner != this_process);
        if abandoned && entry.file_type().is_ok_and(|kind| kind.is_file()) {
            remove_quietly(&entry.path());
        }
    }
}

/// Deletes `path`, logging (not returning) a failure; a missing file is fine.
pub(crate) fn remove_quietly(path: &Path) {
    if let Err(error) = fs::remove_file(path)
        && error.kind() != io::ErrorKind::NotFound
    {
        log::warn!("couldn't remove {}: {error}", path.display());
    }
}

/**
 * Copies `from` into a new file `to` through an ordinary read handle, which shares read, write
 * and delete access, so a program that has `from` open carries on unhindered.
 */
pub(crate) fn copy_new(from: &Path, to: &Path) -> Result<()> {
    let mut source = fs::File::open(from).map_err(|error| Error::io("read", from, error))?;
    let mut target = fs::File::create_new(to).map_err(|error| Error::io("write", to, error))?;
    io::copy(&mut source, &mut target).map_err(|error| Error::io("copy", from, error))?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn file_uris_escape_what_sqlite_reads_as_syntax() {
        assert_eq!(
            file_uri(Path::new("/data/My Library?/a b#%.db")).unwrap(),
            "file:///data/My%20Library%3F/a%20b%23%25.db"
        );
        assert_eq!(
            file_uri(Path::new(r"D:\Slide\slide.db")).unwrap(),
            "file:///D:/Slide/slide.db"
        );
    }

    #[test]
    fn temporary_names_name_their_process() {
        let dir = Path::new("/backups");
        let name = temporary_path(dir);
        let file_name = name.file_name().unwrap().to_str().unwrap();
        assert_eq!(temporary_owner(file_name), Some(std::process::id()));
        assert_ne!(temporary_path(dir), name, "each call gets a new name");
        assert_eq!(temporary_owner(".4242-0190a6f2c3d4.tmp"), Some(4242));
        for other in [
            "4242-7.tmp",
            ".-7.tmp",
            ".4242-.tmp",
            ".42x2-7.tmp",
            ".4242-7.db",
        ] {
            assert_eq!(temporary_owner(other), None, "{other}");
        }
    }

    #[test]
    fn abandoned_temporaries_go_and_everything_else_stays() {
        let dir = tempfile::tempdir().unwrap();
        let ours = temporary_path(dir.path());
        let theirs = dir
            .path()
            .join(format!(".{}-0-0.tmp", std::process::id().wrapping_add(7)));
        let kept = dir.path().join("notes.txt");
        for path in [&ours, &theirs, &kept] {
            fs::write(path, b"x").unwrap();
        }
        remove_abandoned_temporaries(dir.path());
        assert!(ours.exists(), "this process may still be writing it");
        assert!(!theirs.exists());
        assert!(kept.exists());
    }

    #[test]
    fn a_missing_file_is_never_created() {
        let dir = tempfile::tempdir().unwrap();
        let missing = dir.path().join("missing.db");
        assert!(open_existing(&missing).is_err());
        assert!(check(&missing, Check::Quick).is_err());
        assert!(!missing.exists());
    }
}
