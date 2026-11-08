//! A test app's schema, and its databases laid out exactly as sqlez leaves them, built with plain
//! SQL (docs/store.md "Migrations" has the table's format).

#![allow(dead_code, reason = "each test file uses its own share of these")]

use std::fs;
use std::path::{Path, PathBuf};

use toolbox_store::rusqlite::{Connection, params};
use toolbox_store::{Backups, Database, OpenOptions, Schema};

pub const DOMAIN: &str = "app";

/**
 * Step 0 as the app writes it, with what the apps' steps use: `STRICT`, `CHECK` word lists,
 * `ON DELETE SET NULL` (which sqlformat splits over three lines), an FTS5 trigram table.
 */
pub const STEP_0: &str = "CREATE TABLE folders (
  id INTEGER PRIMARY KEY,
  parent_id INTEGER REFERENCES folders(id) ON DELETE SET NULL CHECK (parent_id <> id),
  name TEXT NOT NULL CHECK (name <> ''),
  color TEXT NOT NULL DEFAULT 'sky' CHECK (color IN ('sky', 'mint', 'lemon'))
) STRICT;
CREATE TABLE items (
  id INTEGER PRIMARY KEY,
  folder_id INTEGER NOT NULL REFERENCES folders(id) ON DELETE CASCADE,
  path BLOB NOT NULL,
  hold_ms INTEGER NOT NULL CHECK (hold_ms BETWEEN 100 AND 3600000 AND hold_ms % 100 = 0),
  UNIQUE (folder_id, path)
) STRICT;
CREATE INDEX items_order ON items(folder_id, id);
CREATE VIRTUAL TABLE item_fts USING fts5(doc, tokenize = 'trigram');
CREATE TABLE kv (key TEXT PRIMARY KEY, value TEXT NOT NULL CHECK (json_valid(value)));
";

/// Step 0 as sqlez recorded it: `sqlformat` 0.2.6's output for [`STEP_0`], byte for byte.
pub const STEP_0_RECORDED: &str = "CREATE TABLE folders (
  id INTEGER PRIMARY KEY,
  parent_id INTEGER REFERENCES folders(id) ON DELETE
  SET
    NULL CHECK (parent_id <> id),
    name TEXT NOT NULL CHECK (name <> ''),
    color TEXT NOT NULL DEFAULT 'sky' CHECK (color IN ('sky', 'mint', 'lemon'))
) STRICT;
CREATE TABLE items (
  id INTEGER PRIMARY KEY,
  folder_id INTEGER NOT NULL REFERENCES folders(id) ON DELETE CASCADE,
  path BLOB NOT NULL,
  hold_ms INTEGER NOT NULL CHECK (
    hold_ms BETWEEN 100 AND 3600000
    AND hold_ms % 100 = 0
  ),
  UNIQUE (folder_id, path)
) STRICT;
CREATE INDEX items_order ON items(folder_id, id);
CREATE VIRTUAL TABLE item_fts USING fts5(doc, tokenize = 'trigram');
CREATE TABLE kv (
  key TEXT PRIMARY KEY,
  value TEXT NOT NULL CHECK (json_valid(value))
);";

pub const STEP_1: &str = "ALTER TABLE items ADD COLUMN rotation INTEGER NOT NULL DEFAULT 0 \
                          CHECK (rotation IN (0, 90, 180, 270));";

/// Step 1 as sqlez recorded it (`sqlformat` 0.2.6).
pub const STEP_1_RECORDED: &str = "ALTER TABLE
  items
ADD
  COLUMN rotation INTEGER NOT NULL DEFAULT 0 CHECK (rotation IN (0, 90, 180, 270));";

/// A step written after the move to this crate.
pub const STEP_2: &str = "
CREATE TABLE tags (id INTEGER PRIMARY KEY, name TEXT NOT NULL UNIQUE) STRICT;
CREATE TABLE item_tags (
  item_id INTEGER NOT NULL REFERENCES items(id) ON DELETE CASCADE,
  tag_id INTEGER NOT NULL REFERENCES tags(id) ON DELETE CASCADE,
  PRIMARY KEY (item_id, tag_id)
) STRICT;
";

/// The app as it shipped on sqlez: steps 0 and 1.
pub const SHIPPED: Schema = Schema {
    domain: DOMAIN,
    steps: &[STEP_0, STEP_1],
};

/// The app after the move, with a new step.
pub const NEXT: Schema = Schema {
    domain: DOMAIN,
    steps: &[STEP_0, STEP_1, STEP_2],
};

/// The table as sqlez's `migrate` creates it, text and all.
pub const SQLEZ_MIGRATIONS_TABLE: &str = "CREATE TABLE IF NOT EXISTS migrations (
    domain TEXT,
    step INTEGER,
    migration TEXT
)";

/**
 * A database at `path` as sqlez leaves an app's after it ran `steps` of [`SHIPPED`]: WAL mode,
 * sqlez's `migrations` table, each step run as recorded (sqlez ran the formatted text) and
 * recorded with its 0-based index, then a folder and an item. `close` checkpoints and removes the
 * WAL, as a clean quit does; the connection is returned otherwise, so the caller can copy the
 * files mid-session.
 */
pub fn sqlez_database(path: &Path, steps: usize) -> Connection {
    let connection = Connection::open(path).unwrap();
    connection
        .pragma_update(None, "journal_mode", "WAL")
        .unwrap();
    connection.execute_batch(SQLEZ_MIGRATIONS_TABLE).unwrap();
    for (step, recorded) in [STEP_0_RECORDED, STEP_1_RECORDED]
        .into_iter()
        .enumerate()
        .take(steps)
    {
        connection.execute_batch(recorded).unwrap();
        connection
            .execute(
                "INSERT INTO migrations (domain, step, migration) VALUES (?, ?, ?)",
                params![DOMAIN, step as i64, recorded],
            )
            .unwrap();
    }
    if steps > 0 {
        connection
            .execute_batch(
                "INSERT INTO folders (id, name) VALUES (1, 'Trips');
                 INSERT INTO items (id, folder_id, path, hold_ms) VALUES (1, 1, x'2f612e6a7067', 500);
                 INSERT INTO kv (key, value) VALUES ('volume', '80');",
            )
            .unwrap();
    }
    connection
}

/// [`sqlez_database`], closed as a clean quit leaves it: no WAL beside it.
pub fn closed_sqlez_database(path: &Path, steps: usize) {
    drop(sqlez_database(path, steps));
    assert!(!sibling(path, "-wal").exists());
}

/// A database at `path` after `statements`, each run on its own, closed.
pub fn database_with(path: &Path, statements: &[&str]) {
    let connection = Connection::open(path).unwrap();
    for sql in statements {
        connection.execute_batch(sql).unwrap();
    }
}

pub fn sibling(path: &Path, suffix: &str) -> PathBuf {
    let mut name = path.as_os_str().to_owned();
    name.push(suffix);
    PathBuf::from(name)
}

/// The names in `dir`, sorted.
pub fn listing(dir: &Path) -> Vec<String> {
    let mut names: Vec<String> = fs::read_dir(dir)
        .unwrap()
        .map(|entry| entry.unwrap().file_name().into_string().unwrap())
        .collect();
    names.sort();
    names
}

/// The `(domain, step, migration)` rows, in order.
pub fn recorded(database: &Database) -> Vec<(String, i64, String)> {
    database
        .read(|c| {
            c.prepare("SELECT domain, step, migration FROM migrations ORDER BY rowid")?
                .query_map([], |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)))?
                .collect::<Result<Vec<_>, _>>()
                .map_err(toolbox_store::Error::from)
        })
        .unwrap()
}

/// A test's data dir: the database and its backups folder.
pub struct DataDir {
    pub dir: tempfile::TempDir,
    pub backups: Backups,
}

impl DataDir {
    pub fn new() -> Self {
        let dir = tempfile::tempdir().unwrap();
        let backups = Backups::new(dir.path().join("backups"), DOMAIN);
        Self { dir, backups }
    }

    pub fn database(&self) -> PathBuf {
        self.dir.path().join("app.db")
    }

    pub fn open(&self, schema: &Schema) -> toolbox_store::Result<Database> {
        Database::open(
            &self.database(),
            &OpenOptions::new(schema).backups(&self.backups),
        )
    }
}
