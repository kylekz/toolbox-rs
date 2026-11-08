//! [`Database`]: the handle an app's store wraps (docs/store.md "Threading" and "Opening").

use std::cell::Cell;
use std::fmt;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::time::Duration;

use chrono::NaiveDate;
use rusqlite::{Connection, ErrorCode, OpenFlags, Transaction, TransactionBehavior};

use crate::backup::Backups;
use crate::error::{Error, Result};
use crate::files::{Check, check};
use crate::guard::{Found, inspect};
use crate::migrate::{Schema, migrate};

/// How long a connection waits for another's lock before failing with `SQLITE_BUSY`.
const BUSY_TIMEOUT: Duration = Duration::from_secs(5);
/// Read connections kept open between reads; more are opened while reads overlap, then closed.
const IDLE_READERS: usize = 4;

/// What [`Database::open`] does besides opening: the schema, and the backups it makes.
#[derive(Clone, Copy, Debug)]
pub struct OpenOptions<'a> {
    schema: &'a Schema,
    backups: Option<&'a Backups>,
    today: Option<NaiveDate>,
}

impl<'a> OpenOptions<'a> {
    /// Opens with `schema`'s migrations and no backups.
    pub fn new(schema: &'a Schema) -> Self {
        Self {
            schema,
            backups: None,
            today: None,
        }
    }

    /**
     * Copies an existing database into `backups` before migrating it; one that can't be copied
     * isn't migrated ([`Error::BackupBeforeMigration`]). A damaged database's error names the
     * newest copy.
     */
    pub fn backups(mut self, backups: &'a Backups) -> Self {
        self.backups = Some(backups);
        self
    }

    /**
     * Also makes `today`'s daily copy, on the first open of the day, when [`OpenOptions::backups`]
     * is set. A copy that fails is logged, not returned: a missed day costs little, a refused
     * start the whole app.
     */
    pub fn daily_backup(mut self, today: NaiveDate) -> Self {
        self.today = Some(today);
        self
    }
}

/**
 * An app's open database: a cheap-to-clone, `Send + Sync` handle. Every method is synchronous
 * and may block on disk I/O, so apps call it from background threads, never while painting.
 *
 * - **Writes** run on one connection behind a mutex, on the caller's thread, each as one
 *   `BEGIN IMMEDIATE` transaction: they are serialized, and a write returns once it has
 *   committed, so any read issued afterwards, on any thread, sees it.
 * - **Reads** run on pooled read-only connections, each in one read transaction, so every query
 *   in a read sees the same snapshot. The database is in WAL mode, so reads run in parallel and
 *   never wait for the writer.
 */
#[derive(Clone)]
pub struct Database {
    inner: Arc<Inner>,
}

struct Inner {
    path: PathBuf,
    writer: Mutex<Connection>,
    /// The [`thread_token`] of the thread inside [`Database::write`], or 0.
    writing: AtomicU64,
    readers: Mutex<Vec<Connection>>,
}

impl fmt::Debug for Database {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Database")
            .field("path", &self.inner.path)
            .finish_non_exhaustive()
    }
}

impl Database {
    /**
     * Opens the database at `path`, creating it if missing (its folder must exist), checking and
     * backing up an existing one first, in the order that touches nothing it shouldn't:
     *
     * 1. the foreign-database guard reads the file read-only: another program's database is
     *    [`Error::Foreign`], one a newer build migrated is [`Error::Newer`], and one SQLite can't
     *    read is [`Error::Corrupt`];
     * 2. `PRAGMA quick_check` on an existing database ([`Error::Corrupt`] names the newest backup);
     * 3. the daily copy, then, if a step is pending, the pre-migration copy;
     * 4. the writer opens, switches to WAL and runs the pending steps.
     */
    pub fn open(path: &Path, options: &OpenOptions<'_>) -> Result<Self> {
        let schema = options.schema;
        let corrupt = |detail: String| Error::Corrupt {
            path: path.to_path_buf(),
            detail,
            newest_backup: options.backups.and_then(Backups::newest),
        };
        let found = match inspect(path, schema.domain) {
            Err(Error::Corrupt { detail, .. }) => return Err(corrupt(detail)),
            found => found?,
        };
        if let Found::Ours { steps } = found {
            let known = schema.steps.len();
            if steps > known {
                return Err(Error::Newer {
                    path: path.to_path_buf(),
                    applied: steps,
                    known,
                });
            }
            match check(path, Check::Quick) {
                Ok(Ok(())) => {}
                Ok(Err(detail)) => return Err(corrupt(detail)),
                Err(Error::Unavailable { source, .. }) if is_damage(&source) => {
                    return Err(corrupt(source.to_string()));
                }
                Err(error) => return Err(error),
            }
            if let Some(backups) = options.backups {
                if let Some(today) = options.today
                    && let Err(error) = backups.daily(path, today)
                {
                    log::warn!(
                        "couldn't make today's backup of {}: {error}",
                        path.display()
                    );
                }
                if steps < known {
                    backups
                        .before_migration(path, known)
                        .map_err(|error| Error::BackupBeforeMigration(Box::new(error)))?;
                }
            }
        }
        let mut writer = connect(
            path,
            OpenFlags::SQLITE_OPEN_READ_WRITE | OpenFlags::SQLITE_OPEN_CREATE,
        )?;
        writer.pragma_update(None, "journal_mode", "WAL")?;
        let ran = migrate(&mut writer, schema)?;
        if ran < schema.steps.len() {
            log::info!(
                "migrated {} from {ran} to {} {} steps",
                path.display(),
                schema.steps.len(),
                schema.domain
            );
        }
        Ok(Self {
            inner: Arc::new(Inner {
                path: path.to_path_buf(),
                writer: Mutex::new(writer),
                writing: AtomicU64::new(0),
                readers: Mutex::new(Vec::new()),
            }),
        })
    }

    /// The database file.
    pub fn path(&self) -> &Path {
        &self.inner.path
    }

    /**
     * Runs `f` in one read transaction on a pooled read-only connection, so every query in it
     * sees the same snapshot. A statement that writes fails. Called on a thread inside
     * [`Database::write`] it fails with [`Error::InsideWrite`]: it couldn't see the write's
     * uncommitted changes.
     */
    pub fn read<T, E>(&self, f: impl FnOnce(&Connection) -> Result<T, E>) -> Result<T, E>
    where
        E: From<Error>,
    {
        self.refuse_inside_write()?;
        let mut connection = self.take_reader()?;
        let read = snapshot(&mut connection, f);
        /* Back to the pool whatever `f` returned: its transaction has ended either way. */
        self.return_reader(connection);
        read
    }

    /**
     * Runs `f` as one `BEGIN IMMEDIATE` transaction on the writer, committing on `Ok` and rolling
     * back on `Err` or a panic. Writes are serialized: this waits for any other thread's write.
     * Called again inside `f` on the same thread it fails with [`Error::InsideWrite`] instead of
     * deadlocking; use the transaction `f` was given.
     */
    pub fn write<T, E>(&self, f: impl FnOnce(&Transaction<'_>) -> Result<T, E>) -> Result<T, E>
    where
        E: From<Error>,
    {
        self.refuse_inside_write()?;
        let mut writer = self.lock_writer();
        let _writing = Writing::enter(&self.inner.writing);
        let transaction = writer
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(Error::from)?;
        let value = f(&transaction)?;
        transaction.commit().map_err(Error::from)?;
        Ok(value)
    }

    fn refuse_inside_write(&self) -> Result<()> {
        if self.inner.writing.load(Ordering::Acquire) == thread_token() {
            return Err(Error::InsideWrite);
        }
        Ok(())
    }

    /**
     * The writer. A write that panicked poisoned the mutex, but its transaction rolled back as
     * it unwound, so the connection is as good as before.
     */
    fn lock_writer(&self) -> MutexGuard<'_, Connection> {
        self.inner
            .writer
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
    }

    fn take_reader(&self) -> Result<Connection> {
        let idle = self
            .inner
            .readers
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .pop();
        match idle {
            Some(connection) => Ok(connection),
            None => {
                let connection = connect(&self.inner.path, OpenFlags::SQLITE_OPEN_READ_WRITE)?;
                connection.pragma_update(None, "query_only", true)?;
                Ok(connection)
            }
        }
    }

    fn return_reader(&self, connection: Connection) {
        let mut readers = self
            .inner
            .readers
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        if readers.len() < IDLE_READERS {
            readers.push(connection);
        }
    }
}

/**
 * A connection to `path` with every connection's settings: a busy timeout, foreign keys
 * enforced, and `synchronous = NORMAL` (safe in WAL mode: a crash loses at most the last
 * transactions, never the database). It never falls back to an in-memory database, as sqlez's
 * did when the file couldn't be opened: that is [`Error::Unavailable`].
 */
fn connect(path: &Path, flags: OpenFlags) -> Result<Connection> {
    let unavailable = |source| Error::Unavailable {
        path: path.to_path_buf(),
        source,
    };
    let connection = Connection::open_with_flags(path, flags | OpenFlags::SQLITE_OPEN_NO_MUTEX)
        .map_err(unavailable)?;
    connection.busy_timeout(BUSY_TIMEOUT).map_err(unavailable)?;
    connection
        .execute_batch("PRAGMA foreign_keys = ON; PRAGMA synchronous = NORMAL;")
        .map_err(unavailable)?;
    Ok(connection)
}

/// `f` in one read transaction on `connection`, rolled back if `f` fails.
fn snapshot<T, E>(
    connection: &mut Connection,
    f: impl FnOnce(&Connection) -> Result<T, E>,
) -> Result<T, E>
where
    E: From<Error>,
{
    let transaction = connection
        .transaction_with_behavior(TransactionBehavior::Deferred)
        .map_err(Error::from)?;
    let value = f(&transaction)?;
    transaction.commit().map_err(Error::from)?;
    Ok(value)
}

/// Whether `error` says the file isn't a database, or is a damaged one.
fn is_damage(error: &rusqlite::Error) -> bool {
    matches!(
        error.sqlite_error_code(),
        Some(ErrorCode::DatabaseCorrupt | ErrorCode::NotADatabase)
    )
}

/// A number for this thread, never 0, the same for its lifetime: what [`Inner::writing`] holds.
fn thread_token() -> u64 {
    static NEXT: AtomicU64 = AtomicU64::new(1);
    thread_local! {
        static TOKEN: Cell<u64> = const { Cell::new(0) };
    }
    TOKEN.with(|token| {
        if token.get() == 0 {
            token.set(NEXT.fetch_add(1, Ordering::Relaxed));
        }
        token.get()
    })
}

/// Marks this thread as inside a write until dropped, so a nested call is refused.
struct Writing<'a>(&'a AtomicU64);

impl<'a> Writing<'a> {
    fn enter(writing: &'a AtomicU64) -> Self {
        writing.store(thread_token(), Ordering::Release);
        Self(writing)
    }
}

impl Drop for Writing<'_> {
    fn drop(&mut self) {
        self.0.store(0, Ordering::Release);
    }
}
