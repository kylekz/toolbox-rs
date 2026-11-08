//! An app's SQLite database on rusqlite (docs/store.md), the layer slide and fc each built on
//! their GPL copies of Zed's sqlez:
//!
//! - [`Database`]: one writer connection behind a mutex, pooled read connections, every write one
//!   `BEGIN IMMEDIATE` transaction, every read one snapshot;
//! - [`Database::open`]: the foreign-database guard, `PRAGMA quick_check`, the daily and
//!   pre-migration backups, then WAL and the migrations, in that order;
//! - [`Schema`]: migrations recorded in sqlez's own `migrations` table, so a database sqlez
//!   migrated carries on from the step it stopped at;
//! - [`Backups`]: checked copies, pruned by kind, and restoring one;
//! - [`Kv`]: the key-value table;
//! - [`OsPath`] / [`OsPathBuf`]: paths stored as the OS's bytes, as sqlez stored them;
//! - [`Error`]: the typed errors.
//!
//! Queries are plain rusqlite. Apps use the re-exported [`rusqlite`], so their `ToSql` and
//! `FromSql` impls are for the same version this crate builds.
//!
//! gpui: **never** (`script/check`).

mod backup;
mod database;
mod error;
mod files;
mod guard;
mod kv;
mod migrate;
mod path;

pub use rusqlite;

pub use backup::{Backup, BackupKind, Backups};
pub use database::{Database, OpenOptions};
pub use error::{Error, Result};
pub use kv::Kv;
pub use migrate::Schema;
pub use path::{OsPath, OsPathBuf};
