//! The crate's typed errors (docs/store.md "Errors"). Each refusal names the file it is about, so
//! an app can word its own message (`--data-dir`, its name) around the variant.

use std::io;
use std::path::PathBuf;

use thiserror::Error;

/// Everything [`Database`](crate::Database), [`Backups`](crate::Backups) and the guard can fail with.
#[derive(Debug, Error)]
#[non_exhaustive]
pub enum Error {
    /// A SQLite error from a query, a transaction or a pragma.
    #[error(transparent)]
    Sqlite(#[from] rusqlite::Error),

    #[error("could not {action} {}: {source}", .path.display())]
    Io {
        action: &'static str,
        path: PathBuf,
        #[source]
        source: io::Error,
    },

    /**
     * SQLite couldn't open the file: a folder in its way, no permission, no file descriptors
     * left. Nothing was written.
     */
    #[error("could not open the database at {}: {source}", .path.display())]
    Unavailable {
        path: PathBuf,
        #[source]
        source: rusqlite::Error,
    },

    /**
     * The file holds tables but none of this schema's migration steps: another program's
     * database, or another sqlez domain's. It was left untouched.
     */
    #[error("{} holds a database this app didn't create", .0.display())]
    Foreign(PathBuf),

    /**
     * The database has more of this schema's steps than the app knows: a newer build migrated
     * it. It was left untouched, since this build would misread or damage it.
     */
    #[error(
        "{} was upgraded by a newer version of this app ({applied} schema steps; this one knows \
         {known})",
        .path.display()
    )]
    Newer {
        path: PathBuf,
        applied: usize,
        known: usize,
    },

    /**
     * SQLite can't read the file as a database, or `PRAGMA quick_check` found damage. Nothing
     * was written to it; `newest_backup` is the latest copy in the app's backups, if any.
     */
    #[error(
        "the database at {} is damaged ({detail}).{}",
        .path.display(),
        match .newest_backup {
            Some(backup) => format!(" The newest backup is {}.", backup.display()),
            None => " There is no backup of it.".to_string(),
        }
    )]
    Corrupt {
        path: PathBuf,
        detail: String,
        newest_backup: Option<PathBuf>,
    },

    /**
     * A step the database already ran differs from the app's step at the same index, beyond
     * whitespace and comments (docs/store.md "Migrations"): a shipped step was edited.
     */
    #[error(
        "migration step {step} of {domain} changed since this database ran it\n\nRan:\n{ran}\n\n\
         Now:\n{now}"
    )]
    MigrationChanged {
        domain: &'static str,
        step: usize,
        ran: String,
        now: String,
    },

    /// A migration step failed; the whole migration rolled back.
    #[error("migration step {step} of {domain} failed: {source}")]
    MigrationFailed {
        domain: &'static str,
        step: usize,
        #[source]
        source: rusqlite::Error,
    },

    /**
     * After the migration, rows point at parents that don't exist (`PRAGMA foreign_key_check`,
     * one entry per row as `table rowid → parent`). The whole migration rolled back.
     */
    #[error("the migration of {domain} left broken foreign keys: {}", .rows.join("; "))]
    ForeignKeys {
        domain: &'static str,
        rows: Vec<String>,
    },

    /// The database needed a migration and couldn't be backed up first, so it wasn't touched.
    #[error("couldn't back up the database before upgrading it, so it was left as it was: {0}")]
    BackupBeforeMigration(#[source] Box<Error>),

    /// `VACUUM INTO` couldn't copy the database at `from`.
    #[error("could not copy the database at {}: {source}", .from.display())]
    CopyFailed {
        from: PathBuf,
        #[source]
        source: rusqlite::Error,
    },

    /// A copy (a backup, or one being restored) failed its integrity check.
    #[error("{} failed its integrity check: {detail}", .path.display())]
    CheckFailed { path: PathBuf, detail: String },

    /// [`Backups::restore`](crate::Backups::restore) was given a name that isn't one of its backups.
    #[error("{0:?} isn't one of the app's backups")]
    NotABackup(String),

    /**
     * `read` or `write` was called on a thread already inside a `write` on the same database.
     * It would deadlock (a write) or not see the open transaction's changes (a read): use the
     * transaction the outer `write` gave you.
     */
    #[error(
        "read or write called inside a write on the same database; use the write's transaction"
    )]
    InsideWrite,

    /// A path SQLite must be given as text (a `file:` URI, `VACUUM INTO`) isn't valid Unicode.
    #[error("{} is not valid Unicode", .0.display())]
    NotUnicode(PathBuf),

    /// A backup label that isn't lowercase words joined by `-`, or reads as a schema version.
    #[error("{0:?} can't name a backup: use lowercase words joined by '-'")]
    BadLabel(String),
}

impl Error {
    pub(crate) fn io(action: &'static str, path: impl Into<PathBuf>, source: io::Error) -> Self {
        Self::Io {
            action,
            path: path.into(),
            source,
        }
    }
}

pub type Result<T, E = Error> = std::result::Result<T, E>;
