//! Checked backups in one folder (docs/store.md "Backups"): a daily copy, a copy before each
//! migration, and copies before whatever else an app names ("start-over", "restore"), each kind
//! pruned to its own count; and restoring one in the database's place.
//!
//! A copy is SQLite's `VACUUM INTO` on a connection of its own (one read transaction, so a
//! consistent snapshot, and a compact file), written under a temporary name, checked with
//! `PRAGMA integrity_check`, and only then renamed into place, so a half-written or damaged copy
//! never counts as a backup. Only files named like a backup are ever listed or deleted:
//!
//! - `<prefix>-<yyyymmdd>.db`, the daily copy;
//! - `<prefix>-<yyyymmdd>T<hhmmss>Z-before-v<N>.db`, before migrating to `N` steps;
//! - `<prefix>-<yyyymmdd>T<hhmmss>Z-before-<label>.db`, before what `label` says.
//!
//! The time is UTC. Names slide wrote before this crate (`slide-2026-09-30T21-14-05.123Z-before-v2.db`)
//! are read too, so its existing backups are listed and pruned with the new ones.

use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::time::SystemTime;

use chrono::{DateTime, NaiveDate, NaiveTime, TimeDelta, Utc};

use crate::error::{Error, Result};
use crate::files::{
    check_copy, open_existing, remove_abandoned_temporaries, remove_quietly, sibling,
    temporary_path,
};

/// What a backup was made before, or that it is the day's copy.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub enum BackupKind {
    Daily,
    /// Before migrating the database to `to_steps` steps.
    BeforeMigration {
        to_steps: usize,
    },
    /// Before what the label says: `start-over`, `restore`.
    Before(String),
}

/// One backup file.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Backup {
    pub path: PathBuf,
    pub kind: BackupKind,
    /// When it was made: the file's modification time.
    pub made: SystemTime,
    pub bytes: u64,
}

impl Backup {
    /// Its file name, which [`Backups::restore`] takes.
    pub fn name(&self) -> &str {
        self.path
            .file_name()
            .and_then(|name| name.to_str())
            .unwrap_or_default()
    }
}

/// An app's backup folder and how many of each kind it keeps.
#[derive(Clone, Debug)]
pub struct Backups {
    dir: PathBuf,
    prefix: String,
    keep_daily: usize,
    keep_each: usize,
}

impl Backups {
    /// Daily copies kept unless [`Backups::keep`] says otherwise (fc's count).
    pub const KEEP_DAILY: usize = 7;
    /// Copies of each other kind kept unless [`Backups::keep`] says otherwise (both apps' count).
    pub const KEEP_EACH: usize = 5;

    /**
     * Backups in `dir`, named `<prefix>-…`: the app's name (`slide`, `fc`), as its existing
     * backups are named. The folder is made with the first copy.
     */
    pub fn new(dir: impl Into<PathBuf>, prefix: &str) -> Self {
        Self {
            dir: dir.into(),
            prefix: format!("{prefix}-"),
            keep_daily: Self::KEEP_DAILY,
            keep_each: Self::KEEP_EACH,
        }
    }

    /**
     * Keeps the newest `daily` daily copies, and the newest `each` of every other kind (all the
     * pre-migration copies together, and each label on its own). The copy just made is always
     * kept, so 0 means 1.
     */
    pub fn keep(mut self, daily: usize, each: usize) -> Self {
        self.keep_daily = daily;
        self.keep_each = each;
        self
    }

    pub fn dir(&self) -> &Path {
        &self.dir
    }

    /// Which kind of backup a file named `name` is, if it is named like one of these backups.
    pub fn kind_of(&self, name: &str) -> Option<BackupKind> {
        self.parse(name).map(|(kind, _)| kind)
    }

    /// The backups in the folder, newest first; none when it doesn't exist yet.
    pub fn list(&self) -> Result<Vec<Backup>> {
        let mut backups: Vec<Backup> = self
            .entries()?
            .into_iter()
            .map(|(path, kind, stamp)| self.made(path, kind, stamp))
            .collect();
        backups.sort_by(|a, b| (b.made, &b.path).cmp(&(a.made, &a.path)));
        Ok(backups)
    }

    /// The most recently made backup, for the message when the database is damaged.
    pub fn newest(&self) -> Option<PathBuf> {
        self.list()
            .ok()?
            .into_iter()
            .next()
            .map(|backup| backup.path)
    }

    /**
     * Today's copy of the database at `database`, unless there is one already. `today` is the
     * app's calendar day (local time, as the owner sees it). Returns the copy when made.
     */
    pub fn daily(&self, database: &Path, today: NaiveDate) -> Result<Option<Backup>> {
        let path = self
            .dir
            .join(format!("{}{}.db", self.prefix, today.format("%Y%m%d")));
        if path.exists() {
            return Ok(None);
        }
        self.copy_into_place(database, &path)?;
        self.prune(&BackupKind::Daily, &path);
        Ok(Some(self.made(path, BackupKind::Daily, Stamp::Day(today))))
    }

    /**
     * A copy of the database at `database` before what `label` says: lowercase words joined by
     * `-` (`start-over`, `restore`), never `v` and a number, which name pre-migration copies.
     */
    pub fn before(&self, database: &Path, label: &str) -> Result<Backup> {
        if !is_label(label) {
            return Err(Error::BadLabel(label.to_string()));
        }
        self.timestamped(database, label, BackupKind::Before(label.to_string()))
    }

    /// A copy of the database at `database` before migrating it to `to_steps` steps.
    pub(crate) fn before_migration(&self, database: &Path, to_steps: usize) -> Result<Backup> {
        self.timestamped(
            database,
            &format!("v{to_steps}"),
            BackupKind::BeforeMigration { to_steps },
        )
    }

    /**
     * Puts the backup named `name` in the database's place, for an app to call before it opens
     * the database. The current database, if any, is copied first as `…-before-restore.db`; the
     * backup is checked, copied beside it, checked again, and renamed over `database` once the
     * old WAL and shared-memory files are gone (one that can't be removed stops the restore,
     * leaving `database` as it was, since SQLite would replay that WAL onto the restored copy).
     * Returns the backup restored.
     */
    pub fn restore(&self, name: &str, database: &Path) -> Result<PathBuf> {
        let from = self.dir.join(name);
        if self.kind_of(name).is_none() || !from.is_file() {
            return Err(Error::NotABackup(name.to_string()));
        }
        check_copy(&from)?;
        if database.exists() {
            self.before(database, "restore")?;
        }
        let staged = temporary_path(&self.dir);
        let restored = fs::copy(&from, &staged)
            .map_err(|error| Error::io("copy", &from, error))
            .and_then(|_| check_copy(&staged))
            .and_then(|()| {
                for suffix in ["-wal", "-shm"] {
                    let path = sibling(database, suffix);
                    match fs::remove_file(&path) {
                        Err(error) if error.kind() != io::ErrorKind::NotFound => {
                            return Err(Error::io("remove", path, error));
                        }
                        _ => {}
                    }
                }
                fs::rename(&staged, database)
                    .map_err(|error| Error::io("move into place", database, error))
            });
        if restored.is_err() {
            remove_quietly(&staged);
        }
        restored?;
        log::info!("restored the database from {}", from.display());
        Ok(from)
    }

    /// `<prefix>-<time>-before-<what>.db`, of `kind`, then the kind pruned.
    fn timestamped(&self, database: &Path, what: &str, kind: BackupKind) -> Result<Backup> {
        /* A free name: two copies in the same second get the next one. */
        let mut at = Utc::now();
        let path = loop {
            let path = self.dir.join(format!(
                "{}{}-before-{what}.db",
                self.prefix,
                at.format("%Y%m%dT%H%M%SZ")
            ));
            if !path.exists() {
                break path;
            }
            at += TimeDelta::seconds(1);
        };
        self.copy_into_place(database, &path)?;
        self.prune(&kind, &path);
        log::info!("backed the database up to {}", path.display());
        Ok(self.made(path, kind, Stamp::Time(at)))
    }

    /**
     * The backup at `path`. `made` is the file's modification time, which the rename into place
     * keeps; the name's time or day stands in when the file system has none.
     */
    fn made(&self, path: PathBuf, kind: BackupKind, stamp: Stamp) -> Backup {
        let metadata = fs::metadata(&path).ok();
        let named: SystemTime = match stamp {
            Stamp::Time(time) => time.into(),
            Stamp::Day(day) => day.and_time(NaiveTime::MIN).and_utc().into(),
        };
        Backup {
            made: metadata
                .as_ref()
                .and_then(|metadata| metadata.modified().ok())
                .unwrap_or(named),
            bytes: metadata.map_or(0, |metadata| metadata.len()),
            path,
            kind,
        }
    }

    /// `VACUUM INTO` a temporary name, checked, then renamed to `path`.
    fn copy_into_place(&self, database: &Path, path: &Path) -> Result<()> {
        fs::create_dir_all(&self.dir).map_err(|error| Error::io("create", &self.dir, error))?;
        remove_abandoned_temporaries(&self.dir);
        let staged = temporary_path(&self.dir);
        let copied = vacuum_into(database, &staged)
            .and_then(|()| check_copy(&staged))
            .and_then(|()| {
                fs::rename(&staged, path).map_err(|error| Error::io("move into place", path, error))
            });
        if copied.is_err() {
            remove_quietly(&staged);
        }
        copied
    }

    /**
     * Deletes the oldest backups of `kind`'s group until the group's count remain, never `made`:
     * a clock that was once ahead can make the newest copy's name the oldest. A file that can't
     * be deleted is logged and left for the next time.
     */
    fn prune(&self, kind: &BackupKind, made: &Path) {
        let Ok(entries) = self.entries() else {
            return;
        };
        let keep = match kind {
            BackupKind::Daily => self.keep_daily,
            _ => self.keep_each,
        };
        let mut others: Vec<(Stamp, PathBuf)> = entries
            .into_iter()
            .filter(|(path, other, _)| path != made && same_group(kind, other))
            .map(|(path, _, stamp)| (stamp, path))
            .collect();
        others.sort();
        let excess = others.len().saturating_sub(keep.saturating_sub(1));
        for (_, path) in &others[..excess] {
            remove_quietly(path);
        }
    }

    /// Every plain file in the folder named like a backup, with its kind and its name's time.
    fn entries(&self) -> Result<Vec<(PathBuf, BackupKind, Stamp)>> {
        let entries = match fs::read_dir(&self.dir) {
            Ok(entries) => entries,
            Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(Vec::new()),
            Err(error) => return Err(Error::io("list", &self.dir, error)),
        };
        let mut found = Vec::new();
        for entry in entries {
            let entry = entry.map_err(|error| Error::io("list", &self.dir, error))?;
            /* Not followed: a link named like a backup isn't one the app made. */
            if !entry.file_type().is_ok_and(|kind| kind.is_file()) {
                continue;
            }
            if let Some((kind, stamp)) =
                entry.file_name().to_str().and_then(|name| self.parse(name))
            {
                found.push((entry.path(), kind, stamp));
            }
        }
        Ok(found)
    }

    fn parse(&self, name: &str) -> Option<(BackupKind, Stamp)> {
        let stem = name.strip_prefix(&self.prefix)?.strip_suffix(".db")?;
        if let Some(day) = parse_day(stem) {
            return Some((BackupKind::Daily, Stamp::Day(day)));
        }
        let (time, what) = stem.split_once("-before-")?;
        let time = parse_time(time)?;
        let kind = match what.strip_prefix('v').and_then(parse_number) {
            Some(to_steps) => BackupKind::BeforeMigration { to_steps },
            None if is_label(what) => BackupKind::Before(what.to_string()),
            None => return None,
        };
        Some((kind, Stamp::Time(time)))
    }
}

/// When a backup's name says it was made: a day for daily copies, a time for the others.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
enum Stamp {
    Day(NaiveDate),
    Time(DateTime<Utc>),
}

/// Whether `other` is pruned together with `kind`: dailies, all pre-migration copies, one label.
fn same_group(kind: &BackupKind, other: &BackupKind) -> bool {
    match (kind, other) {
        (BackupKind::Daily, BackupKind::Daily) => true,
        (BackupKind::BeforeMigration { .. }, BackupKind::BeforeMigration { .. }) => true,
        (BackupKind::Before(a), BackupKind::Before(b)) => a == b,
        _ => false,
    }
}

/// Lowercase words joined by single `-`, and not `v` with or without a number.
fn is_label(label: &str) -> bool {
    !label.is_empty()
        && label.split('-').all(|word| {
            !word.is_empty()
                && word
                    .bytes()
                    .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit())
        })
        && !label
            .strip_prefix('v')
            .is_some_and(|rest| rest.bytes().all(|b| b.is_ascii_digit()))
}

/// A run of ASCII digits as a number; no sign, no spaces.
fn parse_number(text: &str) -> Option<usize> {
    if text.is_empty() || !text.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    text.parse().ok()
}

/// The digits of `text` at `range` as a number, when they are all digits.
fn digits(text: &str, range: std::ops::Range<usize>) -> Option<u32> {
    let part = text.get(range)?;
    parse_number(part).and_then(|number| u32::try_from(number).ok())
}

/// `yyyymmdd`.
fn parse_day(text: &str) -> Option<NaiveDate> {
    if text.len() != 8 {
        return None;
    }
    NaiveDate::from_ymd_opt(
        i32::try_from(digits(text, 0..4)?).ok()?,
        digits(text, 4..6)?,
        digits(text, 6..8)?,
    )
}

/**
 * `yyyymmddThhmmssZ`, the names written now, or `yyyy-mm-ddThh-mm-ss.mmmZ`, the names slide
 * wrote before this crate.
 */
fn parse_time(text: &str) -> Option<DateTime<Utc>> {
    let bytes = text.as_bytes();
    let (day, hms, milli) = match bytes.len() {
        16 if bytes[8] == b'T' && bytes[15] == b'Z' => (
            parse_day(&text[..8])?,
            [
                digits(text, 9..11)?,
                digits(text, 11..13)?,
                digits(text, 13..15)?,
            ],
            0,
        ),
        24 if [
            (4, b'-'),
            (7, b'-'),
            (10, b'T'),
            (13, b'-'),
            (16, b'-'),
            (19, b'.'),
            (23, b'Z'),
        ]
        .iter()
        .all(|&(at, byte)| bytes[at] == byte) =>
        {
            (
                NaiveDate::from_ymd_opt(
                    i32::try_from(digits(text, 0..4)?).ok()?,
                    digits(text, 5..7)?,
                    digits(text, 8..10)?,
                )?,
                [
                    digits(text, 11..13)?,
                    digits(text, 14..16)?,
                    digits(text, 17..19)?,
                ],
                digits(text, 20..23)?,
            )
        }
        _ => return None,
    };
    let [hour, minute, second] = hms;
    Some(
        day.and_hms_milli_opt(hour, minute, second, milli)?
            .and_utc(),
    )
}

/// `VACUUM INTO` the new file `to`, on a connection of its own closed before returning.
fn vacuum_into(from: &Path, to: &Path) -> Result<()> {
    let target = to
        .to_str()
        .ok_or_else(|| Error::NotUnicode(to.to_path_buf()))?;
    open_existing(from)?
        .execute("VACUUM INTO ?1", [target])
        .map_err(|source| Error::CopyFailed {
            from: from.to_path_buf(),
            source,
        })?;
    Ok(())
}
