# toolbox_store: an app's SQLite database

`toolbox_store` is the database layer slide and fc each built on their own GPL copy of Zed's
`sqlez`, as one MIT crate on [rusqlite](https://github.com/rusqlite/rusqlite) (PLAN §5a). It holds
what both apps had in common:

- one writer and the way reads are served;
- migrations, recorded in sqlez's own `migrations` table, so the owner's existing databases carry
  on where they are;
- the foreign-database guard;
- checked backups, plus restoring one;
- the key-value table;
- typed errors.

Each app keeps its own tables, queries, file names and error wording. The crate has no gpui
dependency (`script/check` asserts it).

## Contents

1. [Dependencies](#1-dependencies)
2. [The API](#2-the-api)
3. [Design: what came from which app, and why](#3-design-what-came-from-which-app-and-why)
4. [sqlez → rusqlite translation table](#4-sqlez--rusqlite-translation-table)
5. [Adopting it: slide](#5-adopting-it-slide)
6. [Adopting it: fc](#6-adopting-it-fc)
7. [Proving a real database still opens](#7-proving-a-real-database-still-opens)

## 1. Dependencies

| Crate | Licence | Why |
|---|---|---|
| `rusqlite` 0.41 | MIT | SQLite's standard Rust crate. |
| `libsqlite3-sys` 0.39.1 (through rusqlite) | MIT; SQLite itself is public domain | Bundles SQLite 3.54.1. |
| `hashlink`, `hashbrown`, `foldhash` (rusqlite's statement cache) | MIT/Apache-2.0, Zlib | `prepare_cached`. |
| `chrono` (no default features) | MIT/Apache-2.0 | Backup names and the daily date. |
| `tempfile` | MIT/Apache-2.0 | The guard's scratch copy. |
| `thiserror`, `log` | MIT/Apache-2.0 | |

- **`bundled`:** rusqlite compiles its own SQLite, as both apps already do (`libsqlite3-sys
  =0.30.1`, SQLite 3.46.0). That pins one known SQLite on every OS: Windows ships none, macOS's
  changes with the OS, and fc's search needs FTS5's trigram tokenizer (3.34+). The bundled build
  has FTS5 and JSON.
- **`fallible_uint`:** lets `u64` and `usize` bind and read, as they did in sqlez. Both apps use
  them. A value too large for SQLite's `i64` is now an error, where sqlez silently wrapped it.
- **cargo-deny:** every licence above is already on slide's and fc's `deny.toml` allow-lists, and
  everything comes from crates.io. Nothing in the apps' `deny.toml` needs to change.
- **One SQLite per app:** `libsqlite3-sys` declares `links = "sqlite3"`, so Cargo allows only one
  version of it in a build. An app can't keep sqlez's `=0.30.1` and add this crate at the same
  time. The adoption order (§5, §6) starts by moving the app's pin to `=0.39.1`.

## 2. The API

```rust
use toolbox_store::rusqlite::{OptionalExtension as _, params};
use toolbox_store::{Backups, Database, Kv, OpenOptions, Schema};

/** The app's migrations: append-only, shipped steps never edited. */
pub const SCHEMA: Schema = Schema { domain: "slide", steps: &[MIGRATION_1, MIGRATION_2] };
const SETTINGS: Kv = Kv::new("kv");

pub struct Store {
    db: Database,
    data_dir: PathBuf,
}

impl Store {
    pub fn open(data_dir: &Path) -> Result<Self> {
        let backups = Backups::new(data_dir.join("backups"), "slide");
        let db = Database::open(
            &data_dir.join("slide.db"),
            &OpenOptions::new(&SCHEMA).backups(&backups),
        )?;
        Ok(Self { db, data_dir: data_dir.to_path_buf() })
    }

    pub fn kv_set(&self, key: &str, value: &str) -> Result<()> {
        Ok(self.db.write(|tx| SETTINGS.set(tx, key, value))?)
    }

    pub fn folder(&self, id: FolderId) -> Result<Option<Folder>> {
        self.db.read(|c| {
            Ok(c.query_one("SELECT id, name FROM folders WHERE id = ?1", [id], Folder::from_row)
                .optional()?)
        })
    }
}
```

| Item | What it is |
|---|---|
| `Database::open(path, &OpenOptions)` | Checks, backs up and migrates, then opens the database (see "Opening"). |
| `Database::read(\|c\| …)` | One read transaction on a pooled read-only connection. |
| `Database::write(\|tx\| …)` | One `BEGIN IMMEDIATE` transaction on the writer. Commits on `Ok` and rolls back on `Err` or a panic. |
| `Database::path()` | The database file. |
| `OpenOptions::new(&SCHEMA).backups(&backups).daily_backup(today)` | What `open` does besides opening. |
| `Schema { domain, steps }` | The app's sqlez domain name and its steps. |
| `Backups::new(dir, prefix).keep(daily, each)` | A backup folder. Methods: `list`, `newest`, `daily`, `before(db, label)`, `restore(name, db)`, `kind_of`. |
| `Kv::new(table)` | `get`, `set`, `delete` and `get_parsed` on a `key`/`value` table, inside a read or write. |
| `OsPath(&path)` / `OsPathBuf` | A path bound and read as the OS's bytes, as sqlez stored paths. |
| `Error` | The typed errors, below. |
| `toolbox_store::rusqlite` | The re-exported rusqlite. Apps import it from here, so their `ToSql`/`FromSql` impls match this crate's version. |

`read` and `write` are generic over the app's own error type: any `E: From<toolbox_store::Error>`.
Inside a closure, `?` on a rusqlite call needs `E: From<rusqlite::Error>` as well. An app's
`StoreError` gets both:

```rust
#[error(transparent)]
Database(#[from] toolbox_store::Error),

impl From<rusqlite::Error> for StoreError {
    fn from(error: rusqlite::Error) -> Self {
        Self::Database(error.into())
    }
}
```

### Threading

`Database` is a cheap-to-clone, `Send + Sync` handle. Every method blocks on disk I/O, so apps call
it from background threads, never while painting.

- **Writes** run one at a time, on the caller's thread, on one connection behind a mutex. Each
  write is one `BEGIN IMMEDIATE` transaction. A write returns once it has committed, so any read
  started afterwards, on any thread, sees it. This is what sqlez's `locking_queue` did.
- **Reads** take a connection from a small pool (up to 4 kept idle, more opened while reads
  overlap) and run in one read transaction, so every query in a read sees the same snapshot. The
  database is in WAL mode, so reads run in parallel and never wait for the writer. Read
  connections are `query_only`, so a stray `INSERT` in a read fails instead of slipping past the
  writer.
- **Calling `read` or `write` inside a `write` on the same thread** returns
  `Error::InsideWrite`. A nested write would deadlock, as it did with sqlez. A nested read used to
  see the open transaction's changes, because sqlez gave each thread one connection; now it would
  read on another connection and silently miss them. Use the transaction the outer write gave
  you instead.
- **A write that panics** rolls back as it unwinds, and the next write works.
- **Every connection** has a 5 s busy timeout, foreign keys on and `synchronous = NORMAL`.
  `NORMAL` is safe in WAL mode: a power cut can lose the last few commits, never the database.

### Opening

`Database::open` runs these steps in an order that never writes to a file it shouldn't:

1. **The guard** reads an existing file read-only. If a non-empty `-wal` sits beside it, it reads
   a scratch copy of both, so steps that only reached the WAL before a crash still count.
   - Another program's tables, a `migrations` table that isn't sqlez's, or another sqlez domain's
     database: `Error::Foreign`.
   - A file SQLite can't read as a database: `Error::Corrupt`.
   - More recorded steps than the app knows: `Error::Newer`.
   
   None of these touch the file: no WAL, no `-shm`, no backup.
2. **`PRAGMA quick_check`** on an existing database. Damage is `Error::Corrupt`, which names
   `Backups::newest()`.
3. **The daily copy**, if `daily_backup(today)` was asked for: the first open of each day. A
   failed copy is logged, not returned. Then, **if a step is pending**, the pre-migration copy.
   If that copy fails, the open fails with `Error::BackupBeforeMigration` and the database is left
   as it was.
4. **The writer opens**, switches the file to WAL and runs the pending steps.

A missing file, an empty file, or one holding only sqlez's empty `migrations` table counts as a
new database. The folder must already exist. SQLite failing to open the file (a folder in the way,
permissions, no file descriptors left) is `Error::Unavailable`. sqlez used to fall back to an
empty in-memory database here, which both apps had to guard against.

### Migrations

**sqlez's table, as the apps' databases have it** (read from `crates/sqlez/src/migrations.rs` and
from fc's `tests/fixtures/db/v2.db`):

```sql
CREATE TABLE migrations (domain TEXT, step INTEGER, migration TEXT)  -- no key, no constraints
```

- One row is written per step that ran: `domain` is the app's name (`slide`, `fc`), `step` is the
  step's index counting from 0, and `migration` is the step's SQL.
- The SQL is stored as `sqlformat` 0.2 printed it. sqlez ran that formatted text and recorded it.
  `sqlformat` only re-indents and re-wraps; it changes nothing else.
- On open, sqlez read the domain's rows `ORDER BY step` and matched them to the app's steps by
  position.
- It refused a database whose recorded step differed from the app's, comparing both after
  re-formatting.
- It ran the remaining steps with foreign keys off, inside savepoints, then deleted every row
  whose foreign key pointed at a missing parent.
- It ignored rows past the steps it knew. Both apps refused those databases before sqlez saw
  them.

**This crate keeps that table and that contract:**

- A new database gets exactly that table.
- Rows keep the same meaning.
- A database sqlez migrated to step N runs only steps N and later. New rows hold the step's SQL
  as the app wrote it.

**What changed, and why:**

| | sqlez | here | why |
|---|---|---|---|
| Comparing a recorded step | Re-format both with `sqlformat`, then compare | Compare with whitespace and comments outside quotes ignored | Gives the same answer for every step either app shipped (slide's 2, fc's 2: checked against `sqlformat` 0.2.6's output and fc's real database). It doesn't depend on a formatter's version, and editing a comment in a shipped step is harmless. |
| Rows past the known steps | Ignored | `Error::Newer` | Both apps already refused these before opening; now the crate does it. |
| Foreign keys after a migration | Orphaned rows silently deleted | `Error::ForeignKeys` names the rows, and the whole migration rolls back | Silently deleting the owner's rows is the wrong default. A step that breaks references is a bug to fix before shipping. The pre-migration backup is still there either way. |
| Transactions | Nested savepoints, retried 10 times "in case another process migrates" | One `BEGIN IMMEDIATE` transaction | A second process waits on the busy timeout, then sees the steps already done. Nothing needs retrying. |

Each step may hold several statements, as before. Steps run with foreign keys off, so a step can
rebuild a table (SQLite's "12-step" `ALTER TABLE` recipe). `PRAGMA foreign_key_check` then runs
before the commit.

### Backups

Copies go into one folder, named by the app's prefix:

| Name | Kind | Kept |
|---|---|---|
| `fc-20261101.db` | `Daily` | `keep_daily` (7 by default) |
| `fc-20261101T142233Z-before-v3.db` | `BeforeMigration { to_steps: 3 }` | `keep_each` (5) across all versions |
| `fc-20261101T142233Z-before-start-over.db` | `Before("start-over")` | `keep_each` (5) per label |

- **How a copy is made:** `VACUUM INTO` on a connection of its own, which gives one consistent
  snapshot and a compact file. The copy is written under a temporary name (`.<pid>-….tmp`),
  checked with `PRAGMA integrity_check`, and only then renamed into place, so a half-written or
  damaged copy never counts as a backup. Temporary files from a crashed run are deleted the next
  time.
- **Pruning** removes the oldest of the same kind, judged by the time in the name. It never
  removes the copy just made, even when a clock that was once ahead makes that copy's name look
  oldest.
- **Only files named like a backup** are ever listed or deleted. Links are ignored.
- **Old names:** slide's older names (`slide-2026-09-30T21-14-05.123Z-before-v2.db`) are still
  read, so they're listed and pruned along with the new ones.
- **`Backup::made`** is the file's modification time. The rename into place keeps it.

**`Backups::restore(name, database)`** puts a backup in the database's place. Call it before
`Database::open`. It:

1. checks the backup;
2. copies the current database as `…-before-restore.db`;
3. copies the backup beside it and checks the copy;
4. removes the old `-wal` and `-shm`, then renames the copy over the database.

If the old WAL can't be removed, the restore stops there, because SQLite would replay it onto
the restored copy. A restore that fails leaves the database as it was. fc's "restore on next
start" marker file stays in fc.

**Known limit (as in fc today):** if the current database is too damaged to copy, step 2 fails
and the restore stops. Moving the damaged file aside by hand is the way out.

### Key-value

`Kv::new("kv")` (slide) or `Kv::new("app_state")` (fc) names a table the app's own migration
created, with `key TEXT PRIMARY KEY` and `value TEXT` columns. The name is checked when the build
compiles the constant.

- `get`, `set` (an upsert) and `delete` take a connection, so they run inside the caller's read or
  write.
- `get_parsed(c, key, parse)` reads a missing or unparsable value as `None` and logs the
  unparsable one. One bad row then gives the app's default instead of an error; this is slide's
  `lenient`.
- Values are text. fc stores JSON; its two `serde_json` wrappers stay in fc.

### Paths

sqlez stored a `Path` as a blob of its OS bytes: raw bytes on Unix, WTF-8 on Windows. Slide's
`items.path` holds such blobs.

- `OsPath(&path)` binds a path the same way.
- `row.get::<_, OsPathBuf>(i)?.0` reads one back.

Any path the OS gives round-trips exactly, including non-Unicode names. On Windows, the bytes are
checked as WTF-8 and decoded without `unsafe`.

### Errors

| Variant | When |
|---|---|
| `Sqlite(rusqlite::Error)` | A query, transaction or pragma failed. |
| `Io { action, path, source }` | A file operation failed. |
| `Unavailable { path, source }` | SQLite couldn't open the file. |
| `Foreign(path)` | Another program's database. Left untouched. |
| `Newer { path, applied, known }` | A newer build migrated it. Left untouched. |
| `Corrupt { path, detail, newest_backup }` | Unreadable, or failed `quick_check`. |
| `MigrationChanged { domain, step, ran, now }` | A shipped step was edited. |
| `MigrationFailed { domain, step, source }` | A step failed. Everything rolled back. |
| `ForeignKeys { domain, rows }` | A step left broken references. Everything rolled back. |
| `BackupBeforeMigration(Box<Error>)` | Couldn't copy the database first, so it wasn't migrated. |
| `CopyFailed`, `CheckFailed`, `NotABackup`, `BadLabel`, `NotUnicode` | Backups and restores. |
| `InsideWrite` | `read`/`write` called inside a `write` on the same thread. |

`Error` is `#[non_exhaustive]`. An app keeps its own `StoreError` with its own wording and wraps
this one. Its alert can match the variants it words specially: slide's `fatal.rs` matches
`Foreign`, fc's matches `Corrupt` and `Newer`.

## 3. Design: what came from which app, and why

Both copies started from folio's store. Where they drifted:

| Topic | slide | fc | Here | Why |
|---|---|---|---|---|
| Writes | sqlez `locking_queue`: a mutex, on the caller's thread | same | A mutex around one connection | Same behaviour, without sqlez's queue. The closure no longer has to be `Send + 'static`, so the `key.to_owned()` copies made before each write go, and so does `futures::executor::block_on`. |
| Reads | One connection per thread, kept for the thread's life | same | A pool, at most 4 idle | Connections no longer pile up per thread, and none ever falls back to memory. |
| `read` vs `read_snapshot` | both | both | `read` is always a snapshot | A read transaction costs microseconds. One rule is simpler than choosing each time. |
| Write closure's argument | `&Connection` | `&Transaction` (derefs to `Connection`) | `&rusqlite::Transaction`, which derefs to `Connection` | Both apps' helper signatures keep working. |
| Busy timeout | 5 s, implicit in sqlez | 5 s, explicit | 5 s | |
| Guard and WAL | Reads the file immutable first. Copies the WAL only when the file looks unclaimed or unreadable. Counts steps on a separate read-write connection. | Copies whenever a non-empty WAL exists. One read gives the verdict and the count. | fc's | One read-only pass is right in every case, including steps a newer build left in the WAL. Slide's cut-short-checkpoint test is kept (`tests/open.rs`). |
| Unreadable file | A database error | `Corrupt`, naming the newest backup | fc's | The owner learns where a good copy is. |
| `quick_check` at open | no | yes | yes, for both | It stops a damaged database before a backup copies it over the good ones or a migration writes to it. Slide's database is small, so the check is quick. |
| Check on a copy | `integrity_check` | `quick_check` | `integrity_check` | Copies are rare, at most daily. A backup is only worth keeping if it's sound. |
| Backup kinds | Before migration (keeps 5) | Daily (7), before migration, before start-over, before restore (5 each) | fc's, with labels | Slide can turn dailies on with one call. |
| Backup names | `slide-2026-09-30T21-14-05.123Z-before-v2.db` | `fc-20261101T142233Z-before-v2.db` | fc's written, both read | Slide's existing backups stay listed and pruned. |
| Restore | none | marker file plus restore at open | The file handling is here; the marker stays in fc | Getting the WAL handling right is the hard part, and it's generic. |
| Key-value | `kv`, text | `app_state`, JSON with a `json_valid` `CHECK` | The app names the table; values are text | Both tables keep their names and rows. |
| Sqlite check | none | FTS5 trigram and version at open | stays in fc | One app's requirement. With `bundled`, the check now always passes. |

## 4. sqlez → rusqlite translation table

Imports come from `toolbox_store::rusqlite`. `c` is a `&Connection` or a `&Transaction`.

| sqlez | rusqlite | Notes |
|---|---|---|
| `ThreadSafeConnection::builder::<D>(uri, true)…build()` + `block_on` | `Database::open(&path, &OpenOptions::new(&SCHEMA)…)` | |
| `impl Domain for AppDomain { NAME, MIGRATIONS }` | `const SCHEMA: Schema = Schema { domain, steps }` | Same name, same steps. |
| `store.write(move \|c\| …)` with `Send + 'static` | `db.write(\|tx\| …)` | Borrows freely. Drop the `to_owned()`/`clone()` made only to move values into the closure. |
| `store.read(…)` / `store.read_snapshot(…)` | `db.read(\|c\| …)` | Always a snapshot. |
| `connection.persistent()` checks | delete | Opening fails instead. |
| `c.exec(sql)?()?` | one statement: `c.execute(sql, [])?`; several: `c.execute_batch(sql)?` | `execute` refuses a string with several statements. |
| `c.exec_bound::<(A, B)>(sql)?((a, b))?` | `c.execute(sql, (a, b))?` or `c.execute(sql, params![a, b])?` | Returns the number of rows changed. Tuples up to 16 are parameters. |
| a prepared `exec_bound` called in a loop | `let mut s = c.prepare(sql)?;` then `s.execute((a, b))?` in the loop | |
| `c.select::<T>(sql)?()?` | `c.prepare(sql)?.query_map([], \|r\| r.get(0))?.collect::<Result<Vec<T>, _>>()?` | |
| `c.select_bound::<B, T>(sql)?(b)?` | `c.prepare(sql)?.query_map(b, \|r\| …)?.collect::<Result<Vec<_>, _>>()?` | |
| `c.select_row::<T>(sql)?()?` | `c.query_one(sql, [], \|r\| r.get(0)).optional()?` | `query_one` keeps sqlez's rule that more than one row is an error; `query_row` silently takes the first. Needs `OptionalExtension`. |
| `c.select_row_bound::<B, T>(sql)?(b)?` | `c.query_one(sql, b, \|r\| r.get(0)).optional()?` | |
| A tuple read as columns, `(A, B, C)` | `\|r\| <(A, B, C)>::try_from(r)` or `Ok((r.get(0)?, r.get(1)?, r.get(2)?))` | |
| A struct read as consecutive columns (`impl Column`) | `fn from_row(r: &Row) -> rusqlite::Result<Self>`, passed as the mapping function | Read by index or by column name (`r.get("name")`). |
| `impl StaticColumnCount + Bind for T` | `impl ToSql for T` | |
| `impl Column for T` | `impl FromSql for T` | Return `FromSqlError::Other(…)` for a word no variant spells. |
| A word enum bound with `as_str()` | `ToSqlOutput::Borrowed(ValueRef::Text(self.as_str().as_bytes()))` | |
| An id newtype over `i64` | `self.0.to_sql()` / `i64::column_result(v).map(Self)` | |
| slide's uuid ids, bound as hyphenated text | `ToSqlOutput::from(self.0.hyphenated().to_string())`; read with `Uuid::parse_str(v.as_str()?)` | Don't enable rusqlite's `uuid` feature: it stores a blob and would no longer match the existing rows. |
| `PathBuf` / `&Path` bound | `OsPath(&path)` / `r.get::<_, OsPathBuf>(i)?.0` | Same bytes as sqlez stored. |
| `Option<T>` | `Option<T>` | Same. |
| `Option<(A, B)>` (`None` when the first column is NULL) | Read each column as `Option` | |
| **NULL read into a non-`Option` type** | an error (`InvalidColumnType`) | **Behaviour change:** sqlez returned 0, `""` or an empty blob. Check `max()`/`min()`/`sum()` over possibly empty sets, and `LEFT JOIN` columns (about 22 such queries across both apps): read them as `Option`, or use `coalesce(…, 0)`/`total()` in the SQL. |
| `bool`, `i64`, `f64`, `String`, `Vec<u8>`, `u32` | same | |
| `u64`, `usize` | same, with `fallible_uint` (on) | Too large for `i64` is now an error instead of wrapping. |
| `anyhow::Error` from sqlez | `rusqlite::Error`, carried in `toolbox_store::Error::Sqlite` | |
| `Connection::open_read_only(&file_uri(p))` (tests) | `Connection::open_with_flags(p, OpenFlags::SQLITE_OPEN_READ_ONLY)` | |
| `Connection::open_file(p)` (tests) | `Connection::open(p)` | |
| `with_savepoint` | `tx.savepoint()?` | Neither app uses it. |

## 5. Adopting it: slide

slide has about 60 query call sites in `crates/slide_core/src/store` (4.3k lines, a third of them
tests), plus the binding macros in `ids.rs` and `lib.rs`.

| File | Change | Call sites |
|---|---|---|
| `Cargo.toml` (workspace) | Remove `sqlez`, `sqlformat`, `thread_local`, `indoc`, `libsqlite3-sys`; add `toolbox_store` at the tag. `parking_lot`/`futures` stay if anything else uses them. | |
| `crates/sqlez/` | Delete. Also delete the README's and CLAUDE.md's sqlez notes and the GPL licence notice for it. | |
| `slide_core/src/lib.rs` | The word-enum macro: `Bind`/`Column` become `ToSql`/`FromSql`. | 1 macro |
| `slide_core/src/ids.rs` | The uuid-id macro: text, as now. | 1 macro |
| `store/mod.rs` | `Inner` holds a `Database`. `open` becomes `Database::open` with `Backups::new(…, "slide")`. Delete `connection()`, `Transaction`, `file_uri`, `DB_INITIALIZE_QUERY`. `StoreError` wraps `toolbox_store::Error`. Keep the `NewerLibrary`/`ForeignDatabase` wording by matching the variants. | 10 |
| `store/schema.rs` | `SlideDomain` becomes `SCHEMA`. | 1 |
| `store/guard.rs` | Delete (the crate's guard). Its tests move into the crate. | — |
| `store/backup.rs` | Delete. Keep `pub use toolbox_store::{Backup, Backups}` if `slide_ui` needs to list backups. | — |
| `store/kv.rs` | `get`/`set`/`delete` become `Kv`. `lenient` becomes `get_parsed`. | 3 |
| `store/entries.rs`, `items.rs`, `slideshows.rs`, `folders.rs`, `tree.rs`, `rows.rs` | The translation table. `items.path` uses `OsPath`/`OsPathBuf`. | 35 |
| `store/test_support.rs`, `model_test.rs` | `select_row` becomes `query_one(…).optional()`. | 1 |
| `slide/src/fatal.rs` | Match `StoreError::Database(toolbox_store::Error::Foreign(_))`, or keep slide's own `ForeignDatabase` variant mapped in `open`. | 1 |

**Order** (each step leaves the app building and its tests passing):

1. **Pin `libsqlite3-sys = "=0.39.1"`** while sqlez is still in place. This is SQLite 3.46 → 3.54
   on its own. Every FFI function and constant sqlez calls still exists in 0.39.1 by name (checked
   2026-11-09). Run `script/check`. A failure here is about SQLite, not the API. If sqlez doesn't
   compile against the new bindings, do steps 1 and 2 together.
2. **Bump the toolbox tag and add `toolbox_store`.** Both now share the one `libsqlite3-sys`.
3. **Write the `ToSql`/`FromSql` impls** next to the sqlez ones (`lib.rs`, `ids.rs`), with a
   round-trip test against the stored words.
4. **Rewrite `store/mod.rs`** on `Database`, and delete `guard.rs` and `backup.rs`. From here
   the module doesn't compile until step 5 is done: work file by file with the compiler's list.
5. **Convert the query files**, smallest first: `kv.rs`, `tree.rs`, `folders.rs`,
   `slideshows.rs`, `items.rs`, `entries.rs`, then the tests.
6. **Remove sqlez** (crate, workspace entries and notes) and run `cargo deny check`.
7. **Prove it on a real library** (§7) on the owner's Mac and PC.

**Effort:** 1 day. The PLAN's "1–2 days" assumed about 250 call sites. A count of the real
code finds about 60, and the guard, backups, migrations and the in-memory fallback, about a
quarter of the store, are deleted rather than ported.

## 6. Adopting it: fc

fc has about 400 query call sites in `crates/fc_core/src/store` (15k lines with tests; about 8.6k
without), plus the `columns.rs` macros.

| File | Change | Call sites |
|---|---|---|
| `Cargo.toml` (workspace, `fc_core`) | As slide's. `sqlformat` goes too: it is only in `schema.rs`'s test. | |
| `crates/sqlez/`, `principle_one.rs`'s list entry | Delete. | |
| `store/columns.rs` | `id_columns!`/`text_columns!` become `ToSql`/`FromSql`. | 2 macros |
| `store/mod.rs` | As slide's. Keep `check_sqlite` (FTS5 trigram) before `Database::open`. `open_on(data_dir, today)` passes `daily_backup(today)`. `Corrupt`/`NewerDatabase`/`DatabaseUnavailable` come from the crate's variants. | 18 |
| `store/schema.rs` | `FcDomain` becomes `SCHEMA`. The `strict_survives_sqlformat` test goes; `shipped_databases_migrate_to_head` stays and is the proof in §7. | 3 |
| `store/guard.rs` | Delete. | — |
| `store/backup.rs` (550 lines) | Shrinks to the restore marker (`request_restore`, `pending_restore`, `cancel_restore`, and calling `Backups::restore` at open) and a mapping from `BackupKind` to fc's four kinds for `fc_ui/src/settings/restore.rs`. `before_start_over` becomes `backups.before(path, "start-over")`. | 2 |
| `store/library.rs` (`app_state`/`set_app_state`) | Wrap `Kv::new("app_state")` with `serde_json`. | 38 |
| `store/edits.rs`, `health.rs`, `entities.rs`, `downloads.rs`, `video_meta.rs`, `accept.rs`, `root.rs`, `invariants.rs`, `moves.rs`, `search.rs`, `runs.rs`, `proposals.rs`, `probe.rs`, `downloads_folders.rs`, `thumbs.rs`, `matcher.rs`, `download_match.rs`, `covers.rs`, `inbox.rs`, `onboarding.rs`, `cards.rs` | The translation table. | about 290 |
| `*_tests.rs`, `search/tests.rs`, `test_support.rs` | The translation table. | about 65 |
| `paths.rs` | `temporary_path`/`remove_abandoned_temporaries` stay (fc uses them for other files). | — |

**Order:** as slide's steps 1–7, with three differences:

- In step 4, `backup.rs` shrinks rather than going.
- In step 5, go from the leaves inward: `columns.rs`, `library.rs`'s `app_state` helpers,
  `covers.rs`/`inbox.rs`/`onboarding.rs`/`cards.rs`, then the big four (`edits.rs`, `entities.rs`,
  `downloads.rs`, `health.rs`), then the tests.
- In step 7, `shipped_databases_migrate_to_head` must pass unchanged against
  `tests/fixtures/db/v2.db`, the only shipped fixture today.

**Effort:** 2–3 days, against the PLAN's 2–4. There are about 400 call sites, not 650. They are
mechanical, but there are many, and the NULL-into-a-non-`Option` change needs a careful pass over
fc's aggregates.

## 7. Proving a real database still opens

The crate's own tests build databases exactly as sqlez leaves them, in plain SQL:

- the `migrations` table as sqlez creates it;
- each step's text as `sqlformat` 0.2.6 printed it;
- WAL mode;
- a crash with steps only in the WAL.

They then check that migrations carry on from the step where they stopped
(`crates/toolbox_store/tests/sqlez_compat.rs`). During development, a copy of fc's shipped
`tests/fixtures/db/v2.db`, opened with fc's real two steps, ran nothing and kept its 66 idols.
With a third step added, it backed up and then ran only that step.

In each app, after the move:

1. **A fixture test, kept forever:**
   - fc already has one: `shipped_databases_migrate_to_head`.
   - slide adds the same: check in `tests/fixtures/db/v2.db`, saved by the *current* sqlez build
     before the move (`Store::open` on a temp dir, a few rows, then `backup::back_up_database`),
     and a test that opens a copy with the new store and reads the rows back.
   
   This is the proof that no shipped step is run twice or mismatched.
2. **The owner's real database, by hand, on each OS:**
   1. Quit the app.
   2. Copy the data dir (`slide.db` or `fc.db`, with any `-wal`, plus `backups/`) somewhere safe.
   3. Run the new build against the copy (`--data-dir <copy>`).
   4. Check that the log shows no migration (or exactly the new steps, after a
      `…-before-vN.db`), that the library looks the same, and that `SELECT step FROM migrations`
      on the copy shows the same rows as before plus any new ones.
   5. Only then run it on the real data dir.
3. **Downgrade check:** open the migrated copy with the old build. If a new step was added, it
   must refuse with its "newer version" message and leave the file alone. If none was added, it
   must open normally: the rows are unchanged.
