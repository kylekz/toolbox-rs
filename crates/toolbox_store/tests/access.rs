//! Reads and writes (docs/store.md "Threading"): one serialized writer, snapshot reads on pooled
//! read-only connections, and nested calls refused rather than deadlocked.

mod common;

use std::sync::{Arc, Barrier};
use std::thread;

use common::*;
use pretty_assertions::assert_eq;
use toolbox_store::{Database, Error, Kv};

const SETTINGS: Kv = Kv::new("kv");

/// An app's own store error, as the apps have one: the crate's errors convert into it.
#[derive(Debug, thiserror::Error)]
enum AppError {
    #[error("stop")]
    Stop,
    #[error(transparent)]
    Store(#[from] Error),
}

impl From<toolbox_store::rusqlite::Error> for AppError {
    fn from(error: toolbox_store::rusqlite::Error) -> Self {
        Self::Store(error.into())
    }
}

fn names(database: &Database) -> Vec<String> {
    database
        .read(|c| -> Result<_, AppError> {
            Ok(c.prepare("SELECT name FROM folders ORDER BY id")?
                .query_map([], |row| row.get(0))?
                .collect::<Result<Vec<String>, _>>()?)
        })
        .unwrap()
}

fn add_folder(database: &Database, id: i64, name: &str) -> Result<(), AppError> {
    database.write(|c| {
        c.execute("INSERT INTO folders (id, name) VALUES (?1, ?2)", (id, name))?;
        Ok(())
    })
}

#[test]
fn database_is_send_sync_and_clone() {
    fn assert_send_sync<T: Send + Sync + Clone + 'static>() {}
    assert_send_sync::<Database>();
}

#[test]
fn a_write_is_seen_by_every_later_read_on_any_thread() {
    let data = DataDir::new();
    let database = data.open(&SHIPPED).unwrap();
    add_folder(&database, 1, "Trips").unwrap();
    let clone = database.clone();
    assert_eq!(
        thread::spawn(move || names(&clone)).join().unwrap(),
        ["Trips"]
    );
}

#[test]
fn a_failed_write_rolls_back_and_the_app_gets_its_own_error() {
    let data = DataDir::new();
    let database = data.open(&SHIPPED).unwrap();
    let result: Result<(), AppError> = database.write(|c| {
        c.execute("INSERT INTO folders (id, name) VALUES (1, 'Kept out')", [])?;
        Err(AppError::Stop)
    });
    assert!(matches!(result, Err(AppError::Stop)));
    assert!(names(&database).is_empty());
    /* A constraint SQLite enforces fails the same way. */
    let result = add_folder(&database, 2, "");
    assert!(
        matches!(result, Err(AppError::Store(Error::Sqlite(_)))),
        "{result:?}"
    );
    add_folder(&database, 3, "Later").unwrap();
    assert_eq!(names(&database), ["Later"]);
}

#[test]
fn a_write_that_panics_rolls_back_and_the_next_one_works() {
    let data = DataDir::new();
    let database = data.open(&SHIPPED).unwrap();
    let clone = database.clone();
    let panicked = thread::spawn(move || {
        let _: Result<(), AppError> = clone.write(|c| {
            c.execute("INSERT INTO folders (id, name) VALUES (1, 'Half done')", [])?;
            panic!("the app's bug");
        });
    })
    .join();
    assert!(panicked.is_err());
    add_folder(&database, 2, "Next").unwrap();
    assert_eq!(names(&database), ["Next"]);
}

#[test]
fn nested_calls_inside_a_write_are_refused_not_deadlocked() {
    let data = DataDir::new();
    let database = data.open(&SHIPPED).unwrap();
    let (write, read) = database
        .write(|c| {
            c.execute("INSERT INTO folders (id, name) VALUES (1, 'Outer')", [])?;
            Ok::<_, AppError>((
                add_folder(&database, 2, "Inner"),
                database.read(|_| Ok::<_, Error>(())),
            ))
        })
        .unwrap();
    assert!(
        matches!(write, Err(AppError::Store(Error::InsideWrite))),
        "{write:?}"
    );
    assert!(matches!(read, Err(Error::InsideWrite)), "{read:?}");
    assert_eq!(names(&database), ["Outer"]);
    /* Another database is fine inside a write. */
    let other = DataDir::new();
    let other = other.open(&SHIPPED).unwrap();
    database
        .write(|_| add_folder(&other, 1, "Elsewhere"))
        .unwrap();
}

#[test]
fn a_read_cant_write() {
    let data = DataDir::new();
    let database = data.open(&SHIPPED).unwrap();
    let result = database.read(|c| -> Result<_, AppError> {
        Ok(c.execute("INSERT INTO folders (id, name) VALUES (1, 'Sneaky')", [])?)
    });
    assert!(result.is_err());
    assert!(names(&database).is_empty());
}

#[test]
fn a_read_sees_one_snapshot_while_writes_commit() {
    let data = DataDir::new();
    let database = data.open(&SHIPPED).unwrap();
    add_folder(&database, 1, "First").unwrap();
    let (inside, after_write) = database
        .read(|c| {
            let before: i64 = c.query_row("SELECT count(*) FROM folders", [], |row| row.get(0))?;
            let clone = database.clone();
            thread::spawn(move || add_folder(&clone, 2, "Second"))
                .join()
                .unwrap()?;
            let after: i64 = c.query_row("SELECT count(*) FROM folders", [], |row| row.get(0))?;
            Ok::<_, AppError>((before, after))
        })
        .unwrap();
    assert_eq!(
        (inside, after_write),
        (1, 1),
        "the write didn't wait, and the read didn't see it"
    );
    assert_eq!(names(&database), ["First", "Second"]);
}

#[test]
fn many_threads_read_and_write_at_once() {
    let data = DataDir::new();
    let database = data.open(&SHIPPED).unwrap();
    let threads = 8;
    let barrier = Arc::new(Barrier::new(threads));
    let handles: Vec<_> = (0..threads)
        .map(|n| {
            let (database, barrier) = (database.clone(), barrier.clone());
            thread::spawn(move || {
                barrier.wait();
                for i in 0..25 {
                    let id = (n * 100 + i) as i64;
                    add_folder(&database, id, &format!("Folder {id}")).unwrap();
                    database
                        .write(|c| SETTINGS.set(c, &format!("last.{n}"), &id.to_string()))
                        .unwrap();
                    assert!(names(&database).len() > i);
                }
            })
        })
        .collect();
    for handle in handles {
        handle.join().unwrap();
    }
    assert_eq!(names(&database).len(), threads * 25);
}

#[test]
fn the_key_value_table_works_inside_reads_and_writes() {
    let data = DataDir::new();
    let database = data.open(&SHIPPED).unwrap();
    database
        .write(|c| {
            SETTINGS.set(c, "volume", "80")?;
            SETTINGS.set(c, "placement", "\"window\"")
        })
        .unwrap();
    let volume = database
        .read(|c| SETTINGS.get_parsed(c, "volume", |text| text.parse::<u8>().ok()))
        .unwrap();
    assert_eq!(volume, Some(80));
    let refused = database.write(|c| SETTINGS.set(c, "broken", "not json"));
    assert!(refused.is_err(), "the app's CHECK (json_valid) still holds");
}

/**
 * A reader can't open the file (here it is unreadable; in an app, out of descriptors): the read
 * is refused, where sqlez would have read an empty in-memory database.
 */
#[cfg(unix)]
#[test]
fn a_reader_that_cant_open_the_file_is_refused() {
    use std::os::unix::fs::PermissionsExt as _;

    let data = DataDir::new();
    let database = data.open(&SHIPPED).unwrap();
    add_folder(&database, 1, "Kept").unwrap();
    /* One reader in the pool; a second read at the same time needs a new connection. */
    assert_eq!(names(&database), ["Kept"]);
    let permissions = std::fs::metadata(data.database()).unwrap().permissions();
    std::fs::set_permissions(data.database(), std::fs::Permissions::from_mode(0o000)).unwrap();
    let barrier = Arc::new(Barrier::new(2));
    let held = {
        let (database, barrier) = (database.clone(), barrier.clone());
        thread::spawn(move || {
            database.read(|_| {
                barrier.wait();
                barrier.wait();
                Ok::<_, Error>(())
            })
        })
    };
    barrier.wait();
    let refused = database.read(|_| Ok::<_, Error>(()));
    barrier.wait();
    let held = held.join().unwrap();
    std::fs::set_permissions(data.database(), permissions).unwrap();
    assert!(held.is_ok(), "{held:?}");
    assert!(
        matches!(refused, Err(Error::Unavailable { .. })),
        "{refused:?}"
    );
    assert_eq!(names(&database), ["Kept"]);
}
