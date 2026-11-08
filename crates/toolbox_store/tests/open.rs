//! `Database::open`'s checks and backups, in the order docs/store.md "Opening" gives.

mod common;

use std::fs;

use chrono::NaiveDate;
use common::*;
use pretty_assertions::assert_eq;
use toolbox_store::rusqlite::Connection;
use toolbox_store::{BackupKind, Database, Error, OpenOptions, Schema};

fn day(n: u32) -> NaiveDate {
    NaiveDate::from_ymd_opt(2026, 10, n).unwrap()
}

fn open_on(data: &DataDir, today: NaiveDate) -> toolbox_store::Result<Database> {
    Database::open(
        &data.database(),
        &OpenOptions::new(&SHIPPED)
            .backups(&data.backups)
            .daily_backup(today),
    )
}

fn count(database: &Database, sql: &str) -> i64 {
    database
        .read(|c| c.query_row(sql, [], |row| row.get(0)).map_err(Error::from))
        .unwrap()
}

#[test]
fn a_new_database_is_migrated_with_wal_foreign_keys_and_a_busy_timeout() {
    let data = DataDir::new();
    let database = data.open(&SHIPPED).unwrap();
    assert_eq!(
        count(
            &database,
            "SELECT count(*) FROM sqlite_schema WHERE type = 'table' AND name IN ('folders', 'items', 'kv')"
        ),
        3
    );
    let clone = database.clone();
    let (foreign_keys, timeout) = std::thread::spawn(move || {
        (
            count(&clone, "PRAGMA foreign_keys"),
            count(&clone, "PRAGMA busy_timeout"),
        )
    })
    .join()
    .unwrap();
    assert_eq!((foreign_keys, timeout), (1, 5000));
    let writer = database
        .write(|c| {
            Ok::<_, Error>((
                c.query_row("PRAGMA foreign_keys", [], |row| row.get::<_, i64>(0))?,
                c.query_row("PRAGMA synchronous", [], |row| row.get::<_, i64>(0))?,
            ))
        })
        .unwrap();
    assert_eq!(
        writer,
        (1, 1),
        "foreign keys on again after migrating; NORMAL is 1"
    );
    assert!(
        !data.backups.dir().exists(),
        "a new database isn't backed up"
    );
}

#[test]
fn a_foreign_database_is_refused_and_left_untouched() {
    let data = DataDir::new();
    database_with(
        &data.database(),
        &["CREATE TABLE notes (id INTEGER PRIMARY KEY, body TEXT)"],
    );
    let before = fs::read(data.database()).unwrap();
    let error = data.open(&SHIPPED).unwrap_err();
    assert!(matches!(error, Error::Foreign(_)), "{error:?}");
    assert_eq!(fs::read(data.database()).unwrap(), before);
    assert_eq!(listing(data.dir.path()), ["app.db"], "no WAL, no backups");

    let other = DataDir::new();
    database_with(
        &other.database(),
        &["CREATE TABLE migrations (id INTEGER, timestamp INTEGER, name TEXT)"],
    );
    assert!(matches!(other.open(&SHIPPED), Err(Error::Foreign(_))));
}

#[test]
fn a_file_that_isnt_a_database_is_refused_untouched() {
    let data = DataDir::new();
    fs::write(data.database(), b"just some notes, not SQLite").unwrap();
    let error = data.open(&SHIPPED).unwrap_err();
    assert!(
        matches!(
            error,
            Error::Corrupt {
                newest_backup: None,
                ..
            }
        ),
        "{error:?}"
    );
    assert_eq!(
        fs::read(data.database()).unwrap(),
        b"just some notes, not SQLite"
    );
}

#[test]
fn missing_and_empty_files_are_new_databases() {
    let data = DataDir::new();
    fs::write(data.database(), b"").unwrap();
    drop(data.open(&SHIPPED).unwrap());
    assert!(data.open(&SHIPPED).is_ok());
}

#[test]
fn a_path_that_cant_be_opened_is_refused() {
    let data = DataDir::new();
    /* A folder where the file should be: sqlez would have fallen back to an in-memory database. */
    fs::create_dir_all(data.database()).unwrap();
    let error = data.open(&SHIPPED).unwrap_err();
    assert!(
        matches!(&error, Error::Unavailable { path, .. } if *path == data.database()),
        "{error:?}"
    );
}

#[test]
fn a_daily_backup_is_written_once_a_day_and_the_eighth_is_pruned() {
    let data = DataDir::new();
    drop(open_on(&data, day(1)).unwrap());
    assert!(!data.backups.dir().exists(), "nothing to back up yet");
    for n in 2..=9 {
        drop(open_on(&data, day(n)).unwrap());
    }
    let ninth = data.backups.dir().join("app-20261009.db");
    let made_at = fs::metadata(&ninth).unwrap().modified().unwrap();
    drop(open_on(&data, day(9)).unwrap());
    assert_eq!(
        fs::metadata(&ninth).unwrap().modified().unwrap(),
        made_at,
        "one backup a day: the second open leaves it alone"
    );
    let expected: Vec<String> = (3..=9).map(|n| format!("app-202610{n:02}.db")).collect();
    assert_eq!(
        listing(data.backups.dir()),
        expected,
        "day 2's was the eighth and went"
    );
}

#[test]
fn a_failed_quick_check_names_the_newest_backup() {
    let data = DataDir::new();
    {
        let database = open_on(&data, day(1)).unwrap();
        database
            .write(|c| {
                c.execute_batch(
                    "INSERT INTO folders (id, name) VALUES (1, 'Trips');
                     WITH RECURSIVE n(i) AS (SELECT 1 UNION ALL SELECT i + 1 FROM n WHERE i < 20000)
                     INSERT INTO items (folder_id, path, hold_ms)
                     SELECT 1, CAST('/photos/item number ' || i AS BLOB), 500 FROM n;",
                )
                .map_err(Error::from)
            })
            .unwrap();
    }
    drop(open_on(&data, day(2)).unwrap());
    drop(open_on(&data, day(3)).unwrap());
    let newest = data.backups.dir().join("app-20261003.db");
    assert!(newest.is_file());

    /* Scribble over pages after the schema's. */
    let mut bytes = fs::read(data.database()).unwrap();
    let at = bytes.len() / 4 * 3;
    bytes[at..at + 16_384].fill(0xA5);
    fs::write(data.database(), bytes).unwrap();

    let error = open_on(&data, day(4)).unwrap_err();
    match &error {
        Error::Corrupt {
            newest_backup: Some(backup),
            ..
        } => assert_eq!(backup, &newest),
        other => panic!("expected Corrupt, got {other:?}"),
    }
    assert!(error.to_string().contains("app-20261003.db"), "{error}");
    assert!(
        !data.backups.dir().join("app-20261004.db").exists(),
        "a damaged database isn't copied over the good ones"
    );
}

#[test]
fn a_database_that_cant_be_backed_up_is_not_migrated() {
    let data = DataDir::new();
    closed_sqlez_database(&data.database(), 1);
    /* A file where the backups folder goes. */
    fs::write(data.backups.dir(), b"in the way").unwrap();
    let before = fs::read(data.database()).unwrap();
    let error = data.open(&SHIPPED).unwrap_err();
    assert!(
        matches!(error, Error::BackupBeforeMigration(_)),
        "{error:?}"
    );
    assert_eq!(fs::read(data.database()).unwrap(), before);
}

#[test]
fn a_failing_step_rolls_the_whole_migration_back() {
    let data = DataDir::new();
    closed_sqlez_database(&data.database(), 1);
    const BROKEN: Schema = Schema {
        domain: DOMAIN,
        steps: &[
            STEP_0,
            STEP_1,
            STEP_2,
            "CREATE TABLE nope (id INTEGER PRIMARY KEY) STRICT WITHOUT",
        ],
    };
    let error = data.open(&BROKEN).unwrap_err();
    assert!(
        matches!(error, Error::MigrationFailed { step: 3, .. }),
        "{error:?}"
    );
    let database = Database::open(&data.database(), &OpenOptions::new(&SHIPPED)).unwrap();
    assert_eq!(recorded(&database).len(), 2, "only now did step 1 run");
    assert_eq!(
        count(
            &database,
            "SELECT count(*) FROM sqlite_schema WHERE name = 'tags'"
        ),
        0,
        "step 2 ran in the rolled-back transaction"
    );
    assert_eq!(
        data.backups.list().unwrap()[0].kind,
        BackupKind::BeforeMigration { to_steps: 4 }
    );
}

#[test]
fn a_step_that_breaks_foreign_keys_rolls_back() {
    let data = DataDir::new();
    closed_sqlez_database(&data.database(), 2);
    const ORPHANING: Schema = Schema {
        domain: DOMAIN,
        steps: &[STEP_0, STEP_1, "DELETE FROM folders"],
    };
    let error = data.open(&ORPHANING).unwrap_err();
    match &error {
        Error::ForeignKeys { rows, .. } => assert_eq!(rows, &["items 1 → folders"]),
        other => panic!("expected ForeignKeys, got {other:?}"),
    }
    let database = data.open(&SHIPPED).unwrap();
    assert_eq!(count(&database, "SELECT count(*) FROM folders"), 1);
    assert_eq!(count(&database, "SELECT count(*) FROM items"), 1);
}

/**
 * The app killed partway through a checkpoint: the file has the WAL's newest page 1, whose schema
 * names pages only the WAL holds yet, so the file alone reads as malformed. With its WAL it is the
 * app's, and it opens with everything committed.
 */
#[test]
fn a_checkpoint_cut_short_is_read_with_its_wal() {
    let live = DataDir::new();
    let database = live.open(&SHIPPED).unwrap();
    database
        .write(|c| {
            c.execute("INSERT INTO folders (id, name) VALUES (7, 'Kept')", [])
                .map_err(Error::from)
        })
        .unwrap();
    /* Copied while it is open, so its WAL hasn't been checkpointed into the file. */
    let killed = DataDir::new();
    let wal = sibling(&killed.database(), "-wal");
    fs::copy(live.database(), killed.database()).unwrap();
    fs::copy(sibling(&live.database(), "-wal"), &wal).unwrap();
    drop(database);

    /* A checkpoint writes pages in order; this one stopped after page 1. */
    let frames = fs::read(&wal).unwrap();
    let page_size = u32::from_be_bytes(frames[8..12].try_into().unwrap()) as usize;
    let page_1 = frames[32..]
        .chunks_exact(24 + page_size)
        .rfind(|frame| u32::from_be_bytes(frame[0..4].try_into().unwrap()) == 1)
        .unwrap();
    let mut file = fs::read(killed.database()).unwrap();
    file.resize(file.len().max(page_size), 0);
    file[..page_size].copy_from_slice(&page_1[24..]);
    fs::write(killed.database(), &file).unwrap();
    let alone = tempfile::tempdir().unwrap();
    fs::copy(killed.database(), alone.path().join("app.db")).unwrap();
    assert!(
        Connection::open(alone.path().join("app.db"))
            .and_then(
                |c| c.query_row("SELECT count(*) FROM folders", [], |row| row
                    .get::<_, i64>(0))
            )
            .is_err(),
        "the file alone should be unreadable"
    );

    let database = killed.open(&SHIPPED).unwrap();
    assert_eq!(count(&database, "SELECT id FROM folders"), 7);
}
