//! `Backups` (docs/store.md "Backups"): the names it reads and writes, pruning by kind, restoring.

mod common;

use std::fs;
use std::time::{Duration, SystemTime};

use chrono::NaiveDate;
use common::*;
use pretty_assertions::assert_eq;
use toolbox_store::{BackupKind, Backups, Database, Error};

fn kind(name: &str) -> Option<BackupKind> {
    Backups::new("/backups", "fc").kind_of(name)
}

fn before(label: &str) -> Option<BackupKind> {
    Some(BackupKind::Before(label.to_string()))
}

#[test]
fn backup_names_are_recognised_and_nothing_else() {
    assert_eq!(kind("fc-20261101.db"), Some(BackupKind::Daily));
    assert_eq!(
        kind("fc-20261101T142233Z-before-v2.db"),
        Some(BackupKind::BeforeMigration { to_steps: 2 })
    );
    assert_eq!(
        kind("fc-20261101T142233Z-before-start-over.db"),
        before("start-over")
    );
    assert_eq!(
        kind("fc-20261101T142233Z-before-restore.db"),
        before("restore")
    );
    /* The names slide wrote before this crate. */
    assert_eq!(
        Backups::new("/backups", "slide").kind_of("slide-2026-09-26T00-00-00.123Z-before-v2.db"),
        Some(BackupKind::BeforeMigration { to_steps: 2 })
    );
    for other in [
        "fc.db",
        "fc-2026110.db",
        "fc-20261101.db-wal",
        "fc-2026110x.db",
        "fc-20261301.db",
        "fc-20261101T1422Z-before-v2.db",
        "fc-20261101T142233Z-before-v.db",
        "fc-20261101T142233Z-before-.db",
        "fc-20261101T142233Z-before-Start.db",
        "fc-20261101T142233Z-before-start--over.db",
        "fc-20261101T142233Z.db",
        "fc-2026-09-26T00-00-00.123Z.db",
        "fc-2026-13-26T00-00-00.123Z-before-v2.db",
        "folio-20261101.db",
        "notes.txt",
        ".123-abc.tmp",
    ] {
        assert_eq!(kind(other), None, "{other}");
    }
}

#[test]
fn a_backup_is_the_database_and_passes_its_check() {
    let data = DataDir::new();
    closed_sqlez_database(&data.database(), 2);
    let backup = data.backups.before(&data.database(), "start-over").unwrap();
    assert_eq!(backup.kind, BackupKind::Before("start-over".into()));
    assert!(backup.bytes > 0);
    assert!(backup.name().starts_with("app-") && backup.name().ends_with("-before-start-over.db"));
    assert_eq!(data.backups.list().unwrap(), std::slice::from_ref(&backup));

    let copy = DataDir::new();
    fs::copy(&backup.path, copy.database()).unwrap();
    let database = copy.open(&SHIPPED).unwrap();
    assert_eq!(recorded(&database).len(), 2);
}

#[test]
fn labels_are_lowercase_words() {
    let data = DataDir::new();
    closed_sqlez_database(&data.database(), 2);
    for bad in [
        "",
        "Start",
        "start over",
        "v2",
        "start--over",
        "-x",
        "x/../y",
    ] {
        assert!(
            matches!(
                data.backups.before(&data.database(), bad),
                Err(Error::BadLabel(_))
            ),
            "{bad}"
        );
    }
    assert!(!data.backups.dir().exists());
}

/**
 * Pre-migration copies keep five, whatever the version, and leave dailies, other labels and
 * other files alone. The copy just made stays even when a clock that was once ahead makes its
 * name the oldest; slide's old names are pruned with the new ones.
 */
#[test]
fn each_kind_keeps_its_own_count_and_the_newest_is_never_pruned() {
    let data = DataDir::new();
    closed_sqlez_database(&data.database(), 1);
    let dir = data.backups.dir();
    fs::create_dir_all(dir).unwrap();
    let ahead: Vec<String> = (0..Backups::KEEP_EACH)
        .map(|n| format!("app-2099-01-0{}T00-00-00.000Z-before-v1.db", n + 1))
        .collect();
    let others = [
        "app-20200101.db",
        "app-20200101T000000Z-before-start-over.db",
        "notes.txt",
    ];
    for name in ahead.iter().map(String::as_str).chain(others) {
        fs::write(dir.join(name), b"old").unwrap();
    }
    let abandoned = dir.join(format!(".{}-0-0.tmp", std::process::id().wrapping_add(3)));
    fs::write(&abandoned, b"half a copy").unwrap();

    drop(data.open(&SHIPPED).unwrap());
    let migration: Vec<_> = data
        .backups
        .list()
        .unwrap()
        .into_iter()
        .filter(|backup| matches!(backup.kind, BackupKind::BeforeMigration { .. }))
        .collect();
    assert_eq!(migration.len(), Backups::KEEP_EACH);
    assert!(
        migration
            .iter()
            .any(|backup| backup.kind == BackupKind::BeforeMigration { to_steps: 2 })
    );
    assert!(
        !dir.join(&ahead[0]).exists(),
        "the oldest of the others went"
    );
    assert!(ahead[1..].iter().all(|name| dir.join(name).exists()));
    assert!(others.iter().all(|name| dir.join(name).exists()));
    assert!(!abandoned.exists());
}

#[test]
fn backups_list_newest_first() {
    let data = DataDir::new();
    closed_sqlez_database(&data.database(), 2);
    let backups = Backups::new(data.backups.dir(), DOMAIN).keep(2, 2);
    let day = |n| NaiveDate::from_ymd_opt(2026, 11, n).unwrap();
    for n in 1..=3 {
        backups.daily(&data.database(), day(n)).unwrap();
    }
    assert_eq!(
        listing(backups.dir()),
        ["app-20261102.db", "app-20261103.db"],
        "keep(2, …): two dailies"
    );
    let old = backups.dir().join("app-20200101T000000Z-before-restore.db");
    fs::write(&old, b"old").unwrap();
    let in_2020 = SystemTime::UNIX_EPOCH + Duration::from_secs(1_577_836_800);
    fs::File::options()
        .write(true)
        .open(&old)
        .unwrap()
        .set_modified(in_2020)
        .unwrap();
    let start_over = backups.before(&data.database(), "start-over").unwrap();
    let listed = backups.list().unwrap();
    assert_eq!(listed[0].path, start_over.path);
    assert_eq!(
        listed.last().unwrap().name(),
        "app-20200101T000000Z-before-restore.db"
    );
    assert_eq!(listed.last().unwrap().made, in_2020, "the file's time");
    assert_eq!(backups.newest(), Some(start_over.path));
}

#[test]
fn a_missing_or_broken_database_is_neither_made_nor_backed_up() {
    let data = DataDir::new();
    let today = NaiveDate::from_ymd_opt(2026, 11, 1).unwrap();
    assert!(data.backups.daily(&data.database(), today).is_err());
    assert!(!data.database().exists(), "never created");
    fs::write(data.database(), b"not a database at all, just some bytes").unwrap();
    assert!(data.backups.daily(&data.database(), today).is_err());
    assert!(data.backups.before(&data.database(), "start-over").is_err());
    assert_eq!(
        fs::read_dir(data.backups.dir()).unwrap().count(),
        0,
        "no copy, no temporary"
    );
}

fn folder_names(database: &Database) -> Vec<String> {
    database
        .read(|c| {
            c.prepare("SELECT name FROM folders ORDER BY id")?
                .query_map([], |row| row.get(0))?
                .collect::<Result<Vec<String>, _>>()
                .map_err(Error::from)
        })
        .unwrap()
}

fn add_folder(database: &Database, id: i64, name: &str) {
    database
        .write(|c| {
            c.execute("INSERT INTO folders (id, name) VALUES (?1, ?2)", (id, name))
                .map_err(Error::from)
        })
        .unwrap();
}

#[test]
fn a_backup_is_restored_keeping_the_database_it_replaces() {
    let data = DataDir::new();
    let database = data.open(&SHIPPED).unwrap();
    add_folder(&database, 1, "Karina");
    let daily = data
        .backups
        .daily(
            &data.database(),
            NaiveDate::from_ymd_opt(2026, 11, 2).unwrap(),
        )
        .unwrap()
        .unwrap();
    add_folder(&database, 2, "Winter");
    drop(database);

    let restored = data
        .backups
        .restore(daily.name(), &data.database())
        .unwrap();
    assert_eq!(restored, daily.path);
    let database = data.open(&SHIPPED).unwrap();
    assert_eq!(folder_names(&database), ["Karina"]);
    drop(database);

    /* The database it replaced is kept, Winter and all, and comes back the same way. */
    let replaced = data
        .backups
        .list()
        .unwrap()
        .into_iter()
        .find(|backup| backup.kind == BackupKind::Before("restore".into()))
        .unwrap();
    data.backups
        .restore(replaced.name(), &data.database())
        .unwrap();
    let database = data.open(&SHIPPED).unwrap();
    assert_eq!(folder_names(&database), ["Karina", "Winter"]);
}

#[test]
fn only_a_sound_backup_is_restored() {
    let data = DataDir::new();
    let database = data.open(&SHIPPED).unwrap();
    add_folder(&database, 1, "Karina");
    drop(database);
    for name in ["app.db", "../app.db", "settings.json", "app-20261031.db"] {
        assert!(
            matches!(
                data.backups.restore(name, &data.database()),
                Err(Error::NotABackup(_))
            ),
            "{name}"
        );
    }
    fs::create_dir_all(data.backups.dir()).unwrap();
    fs::write(
        data.backups.dir().join("app-20261031.db"),
        b"not a database",
    )
    .unwrap();
    assert!(
        data.backups
            .restore("app-20261031.db", &data.database())
            .is_err()
    );
    let database = data.open(&SHIPPED).unwrap();
    assert_eq!(folder_names(&database), ["Karina"]);
}

#[test]
fn a_wal_that_cant_be_removed_stops_the_restore() {
    let data = DataDir::new();
    drop(data.open(&SHIPPED).unwrap());
    let daily = data
        .backups
        .daily(
            &data.database(),
            NaiveDate::from_ymd_opt(2026, 11, 2).unwrap(),
        )
        .unwrap()
        .unwrap();
    fs::remove_file(data.database()).unwrap();
    /* A folder where the WAL goes can't be removed as a file. */
    fs::create_dir_all(sibling(&data.database(), "-wal").join("held")).unwrap();
    assert!(
        data.backups
            .restore(daily.name(), &data.database())
            .is_err()
    );
    assert!(!data.database().exists());
    assert_eq!(
        listing(data.backups.dir()),
        [daily.name().to_string()],
        "no temporary left"
    );
}
