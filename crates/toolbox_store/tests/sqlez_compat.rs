//! Databases sqlez migrated keep working (docs/store.md "Migrations"): each is built with plain
//! SQL as sqlez leaves one, then opened through this crate.

mod common;

use std::fs;

use common::*;
use pretty_assertions::assert_eq;
use toolbox_store::rusqlite::OptionalExtension as _;
use toolbox_store::{BackupKind, Error, Schema};

#[test]
fn a_fully_migrated_sqlez_database_opens_and_runs_nothing() {
    let data = DataDir::new();
    closed_sqlez_database(&data.database(), 2);
    let database = data.open(&SHIPPED).unwrap();
    assert_eq!(
        recorded(&database),
        [
            (DOMAIN.to_string(), 0, STEP_0_RECORDED.to_string()),
            (DOMAIN.to_string(), 1, STEP_1_RECORDED.to_string()),
        ],
        "sqlez's rows stay as they were; a step run again would have failed on its CREATE TABLE"
    );
    let item: (String, i64) = database
        .read(|c| {
            c.query_row(
                "SELECT f.name, i.rotation FROM items i JOIN folders f ON f.id = i.folder_id",
                [],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .map_err(Error::from)
        })
        .unwrap();
    assert_eq!(item, ("Trips".to_string(), 0));
    assert!(
        !data.backups.dir().exists(),
        "no step was pending, so nothing was backed up"
    );
}

#[test]
fn a_sqlez_database_continues_from_the_step_it_stopped_at() {
    let data = DataDir::new();
    closed_sqlez_database(&data.database(), 1);
    let database = data.open(&NEXT).unwrap();
    let rows = recorded(&database);
    assert_eq!(
        rows,
        [
            (DOMAIN.to_string(), 0, STEP_0_RECORDED.to_string()),
            (DOMAIN.to_string(), 1, STEP_1.to_string()),
            (DOMAIN.to_string(), 2, STEP_2.to_string()),
        ],
        "new rows carry the step as the app wrote it, numbered on from sqlez's"
    );
    database
        .write(|c| {
            c.execute_batch(
                "UPDATE items SET rotation = 90;
                 INSERT INTO tags (id, name) VALUES (1, 'beach');
                 INSERT INTO item_tags (item_id, tag_id) VALUES (1, 1);",
            )
            .map_err(Error::from)
        })
        .unwrap();

    /* The copy made first is the database as sqlez left it, before step 1. */
    let backups = data.backups.list().unwrap();
    assert_eq!(backups.len(), 1);
    assert_eq!(backups[0].kind, BackupKind::BeforeMigration { to_steps: 3 });
    let copy = toolbox_store::rusqlite::Connection::open(&backups[0].path).unwrap();
    let columns: Vec<String> = copy
        .prepare("SELECT name FROM pragma_table_info('items')")
        .unwrap()
        .query_map([], |row| row.get(0))
        .unwrap()
        .collect::<Result<_, _>>()
        .unwrap();
    assert_eq!(columns, ["id", "folder_id", "path", "hold_ms"]);
}

#[test]
fn a_migrated_database_reopens_without_running_anything() {
    let data = DataDir::new();
    closed_sqlez_database(&data.database(), 2);
    drop(data.open(&NEXT).unwrap());
    let before = fs::read(data.database()).unwrap();
    let database = data.open(&NEXT).unwrap();
    assert_eq!(recorded(&database).len(), 3);
    drop(database);
    assert_eq!(
        data.backups.list().unwrap().len(),
        1,
        "only the first open migrated"
    );
    assert_eq!(fs::read(data.database()).unwrap(), before);
}

#[test]
fn an_edited_shipped_step_is_refused_and_nothing_runs() {
    let data = DataDir::new();
    closed_sqlez_database(&data.database(), 1);
    const EDITED: Schema = Schema {
        domain: DOMAIN,
        steps: &[
            "CREATE TABLE folders (id INTEGER PRIMARY KEY, name TEXT NOT NULL);",
            STEP_1,
        ],
    };
    let error = data.open(&EDITED).unwrap_err();
    assert!(
        matches!(error, Error::MigrationChanged { step: 0, .. }),
        "{error:?}"
    );
    let reopened = data.open(&SHIPPED).unwrap();
    assert_eq!(
        recorded(&reopened).len(),
        2,
        "the refused open ran nothing: step 1 ran only now"
    );
}

#[test]
fn a_sqlez_database_with_more_steps_is_refused_untouched() {
    let data = DataDir::new();
    closed_sqlez_database(&data.database(), 2);
    const OLDER: Schema = Schema {
        domain: DOMAIN,
        steps: &[STEP_0],
    };
    let (before, files) = (fs::read(data.database()).unwrap(), listing(data.dir.path()));
    let error = data.open(&OLDER).unwrap_err();
    assert!(
        matches!(
            error,
            Error::Newer {
                applied: 2,
                known: 1,
                ..
            }
        ),
        "{error:?}"
    );
    assert_eq!(fs::read(data.database()).unwrap(), before);
    assert_eq!(
        listing(data.dir.path()),
        files,
        "no backup, no WAL left behind"
    );
}

/**
 * A crash while the app still had its database open: sqlez's steps are only in the WAL, and the
 * file alone looks empty. The guard reads the WAL too, so it counts them: the database is backed
 * up before the pending step, and a build that knows fewer steps refuses it untouched.
 */
#[test]
fn steps_still_in_the_wal_count() {
    let live = tempfile::tempdir().unwrap();
    let path = live.path().join("app.db");
    let connection = sqlez_database(&path, 2);
    let crashed = || {
        let data = DataDir::new();
        fs::copy(&path, data.database()).unwrap();
        fs::copy(sibling(&path, "-wal"), sibling(&data.database(), "-wal")).unwrap();
        data
    };

    let data = crashed();
    const OLDER: Schema = Schema {
        domain: DOMAIN,
        steps: &[STEP_0],
    };
    let files = listing(data.dir.path());
    assert!(matches!(
        data.open(&OLDER),
        Err(Error::Newer { applied: 2, .. })
    ));
    assert_eq!(listing(data.dir.path()), files, "not even a -shm was made");

    let data = crashed();
    let database = data.open(&NEXT).unwrap();
    assert_eq!(recorded(&database).len(), 3);
    let backups = data.backups.list().unwrap();
    assert_eq!(
        backups.len(),
        1,
        "the steps in the WAL made it a database to back up"
    );
    drop(connection);
}

#[test]
fn another_sqlez_apps_database_is_foreign_but_a_bare_table_is_not() {
    let data = DataDir::new();
    database_with(
        &data.database(),
        &[
            SQLEZ_MIGRATIONS_TABLE,
            "INSERT INTO migrations VALUES ('folio', 0, 'CREATE TABLE x (id)')",
            "CREATE TABLE x (id)",
        ],
    );
    assert!(matches!(
        data.open(&SHIPPED),
        Err(Error::Foreign(path)) if path == data.database()
    ));

    let bare = DataDir::new();
    database_with(&bare.database(), &[SQLEZ_MIGRATIONS_TABLE]);
    let database = bare.open(&SHIPPED).unwrap();
    assert_eq!(recorded(&database).len(), 2);
}

#[test]
fn a_new_database_looks_like_one_sqlez_made() {
    let data = DataDir::new();
    let database = data.open(&SHIPPED).unwrap();
    let table: Option<String> = database
        .read(|c| {
            c.query_row(
                "SELECT sql FROM sqlite_schema WHERE name = 'migrations'",
                [],
                |row| row.get(0),
            )
            .optional()
            .map_err(Error::from)
        })
        .unwrap();
    assert_eq!(
        table.as_deref(),
        Some("CREATE TABLE migrations (domain TEXT, step INTEGER, migration TEXT)")
    );
    let journal: String = database
        .write(|c| {
            c.query_row("PRAGMA journal_mode", [], |row| row.get(0))
                .map_err(Error::from)
        })
        .unwrap();
    assert_eq!(journal, "wal");
}
