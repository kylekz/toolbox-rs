//! The foreign-database guard (docs/store.md "Opening"): an existing file is read read-only, and
//! only opened for writing once it is known to be the app's or empty, because even switching it to
//! WAL would write to another program's database.
//!
//! The file is read immutable, so SQLite takes no locks and creates no journal, WAL or
//! shared-memory file, but then it skips `<file>-wal` too, where committed but uncheckpointed
//! transactions live (the app's new schema after a crash, steps a newer build ran, pages a
//! checkpoint cut short). So whenever a non-empty WAL sits beside the file, both are copied to a
//! scratch folder and the copy, WAL replayed, is read instead: one read gives both the verdict and
//! the true step count.

use std::fs;
use std::io;
use std::path::Path;

use rusqlite::Connection;

use crate::error::{Error, Result};
use crate::files::{copy_new, file_uri, open_read_only, sibling};
use crate::migrate::recorded_steps;

/// What the guard found at the database's path.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Found {
    /**
     * Nothing the schema could clash with: no file, an empty one, no tables, or only sqlez's
     * `migrations` table. A folder in the way counts too; opening it then fails as
     * [`Error::Unavailable`].
     */
    Nothing,
    /// A `migrations` table recording this many of the domain's steps.
    Ours { steps: usize },
}

/**
 * Reads what the file at `path` holds for `domain`. Another program's database is
 * [`Error::Foreign`]; a file SQLite can't read as a database is [`Error::Corrupt`] (without a
 * backup named: the opener knows where backups live). Neither is touched.
 */
pub(crate) fn inspect(path: &Path, domain: &str) -> Result<Found> {
    let metadata = match fs::metadata(path) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(Found::Nothing),
        Err(error) => return Err(Error::io("read", path, error)),
    };
    if !metadata.is_file() {
        return Ok(Found::Nothing);
    }
    let wal = sibling(path, "-wal");
    let has_wal = match fs::metadata(&wal) {
        Ok(metadata) => metadata.is_file() && metadata.len() > 0,
        Err(error) if error.kind() == io::ErrorKind::NotFound => false,
        Err(error) => return Err(Error::io("read", wal, error)),
    };
    let found = if has_wal {
        inspect_with_wal(path, &wal, domain)?
    } else if metadata.len() == 0 {
        Ok(Some(Found::Nothing))
    } else {
        read(&format!("{}?mode=ro&immutable=1", file_uri(path)?), domain)
    };
    match found {
        Ok(Some(found)) => Ok(found),
        Ok(None) => Err(Error::Foreign(path.to_path_buf())),
        Err(error) => Err(Error::Corrupt {
            path: path.to_path_buf(),
            detail: error.to_string(),
            newest_backup: None,
        }),
    }
}

/**
 * Reads the database at `uri` on a read-only connection closed before returning: `None` when
 * it is foreign.
 */
fn read(uri: &str, domain: &str) -> rusqlite::Result<Option<Found>> {
    let connection = open_read_only(uri)?;
    let tables: Vec<String> = connection
        .prepare(
            "SELECT name FROM sqlite_schema WHERE type = 'table' AND name NOT GLOB 'sqlite_*'",
        )?
        .query_map([], |row| row.get(0))?
        .collect::<rusqlite::Result<_>>()?;
    if tables.is_empty() {
        return Ok(Some(Found::Nothing));
    }
    /*
     * SQLite names are case-insensitive: `CREATE TABLE IF NOT EXISTS migrations` would take a
     * `Migrations` table for its own.
     */
    if !tables
        .iter()
        .any(|table| table.eq_ignore_ascii_case("migrations"))
        || !is_sqlez_table(&connection)?
    {
        return Ok(None);
    }
    let steps = recorded_steps(&connection, domain)?;
    Ok(if steps > 0 {
        Some(Found::Ours { steps })
    } else if tables.len() == 1 {
        Some(Found::Nothing)
    } else {
        None
    })
}

/// Whether `migrations` has sqlez's columns, rather than being another tool's table of that name.
fn is_sqlez_table(connection: &Connection) -> rusqlite::Result<bool> {
    let columns: Vec<String> = connection
        .prepare("SELECT name FROM pragma_table_info('migrations')")?
        .query_map([], |row| row.get(0))?
        .collect::<rusqlite::Result<_>>()?;
    Ok(["domain", "step", "migration"].iter().all(|wanted| {
        columns
            .iter()
            .any(|column| column.eq_ignore_ascii_case(wanted))
    }))
}

/**
 * [`read`] on a scratch copy of `path` and its `wal`, opened read-only but not immutable, so
 * SQLite replays the WAL's committed transactions into the copy's view. The originals are only
 * read, and the `-shm` beside them isn't copied: SQLite rebuilds one for the copy.
 */
fn inspect_with_wal(
    path: &Path,
    wal: &Path,
    domain: &str,
) -> Result<rusqlite::Result<Option<Found>>> {
    let scratch = tempfile::Builder::new()
        .prefix("toolbox-store-check-")
        .tempdir()
        .map_err(|error| Error::io("create a scratch folder in", std::env::temp_dir(), error))?;
    let copy = scratch.path().join("database.db");
    /*
     * The WAL first: a checkpoint another process runs in between then only moves pages the
     * copied WAL already has into the main file, so the copy still shows every committed table.
     */
    copy_new(wal, &sibling(&copy, "-wal"))?;
    copy_new(path, &copy)?;
    let found = read(&format!("{}?mode=ro", file_uri(&copy)?), domain);
    if let Err(error) = scratch.close() {
        log::warn!("could not remove the database check's scratch copy: {error}");
    }
    Ok(found)
}
