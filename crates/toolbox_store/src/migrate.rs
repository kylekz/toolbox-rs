//! Migrations (docs/store.md "Migrations"): an app's append-only list of SQL steps, recorded in
//! the same `migrations` table sqlez kept, so a database sqlez migrated carries on from the step
//! it stopped at.
//!
//! sqlez's table, as the apps' databases have it: `migrations (domain TEXT, step INTEGER,
//! migration TEXT)`, no key, one row per step that ran, `step` counting from 0, `migration` the
//! step's SQL after sqlez ran it through `sqlformat` (which re-indents it and changes nothing
//! else). sqlez matched rows to steps by position (`ORDER BY step`), refused a database whose
//! recorded step differed from the app's, and ignored rows past the steps it knew.
//!
//! This runner keeps that table and that contract, with three differences, each recorded in
//! docs/store.md "Migrations":
//! - a recorded step is compared with whitespace and comments ignored ([`same_step`]), since
//!   only sqlformat's layout separates sqlez's text from the app's, and new rows hold the app's
//!   text as written;
//! - rows past the known steps are refused ([`Error::Newer`]) rather than ignored;
//! - foreign keys broken by a step fail the migration instead of being deleted.

use rusqlite::{Connection, OptionalExtension as _, TransactionBehavior};

use crate::error::{Error, Result};

/**
 * An app's schema: its sqlez domain name and its steps, oldest first. A shipped step is never
 * edited: every change is a new step at the end.
 *
 * ```ignore
 * pub const SCHEMA: Schema = Schema { domain: "slide", steps: &[MIGRATION_1, MIGRATION_2] };
 * ```
 */
#[derive(Clone, Copy, Debug)]
pub struct Schema {
    /// The `domain` column's value: `slide`, `fc`. Never changes once shipped.
    pub domain: &'static str,
    /// The steps, each one or more SQL statements; step `n` is recorded as `step = n`.
    pub steps: &'static [&'static str],
}

/// The table, as sqlez creates it; a new database gets exactly this.
const CREATE_TABLE: &str =
    "CREATE TABLE IF NOT EXISTS migrations (domain TEXT, step INTEGER, migration TEXT)";

/**
 * Brings the database on `connection` up to `schema`, in one `BEGIN IMMEDIATE` transaction with
 * foreign keys off (so a step may rebuild a table, as SQLite's ALTER TABLE guide does), checked
 * with `PRAGMA foreign_key_check` before it commits. Returns how many steps had run before.
 */
pub(crate) fn migrate(connection: &mut Connection, schema: &Schema) -> Result<usize> {
    /* `foreign_keys` can't change inside a transaction, so it is switched around one. */
    connection.pragma_update(None, "foreign_keys", false)?;
    let migrated = run(connection, schema);
    let restored = connection.pragma_update(None, "foreign_keys", true);
    let steps = migrated?;
    restored?;
    Ok(steps)
}

fn run(connection: &mut Connection, schema: &Schema) -> Result<usize> {
    let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
    transaction.execute_batch(CREATE_TABLE)?;
    let ran: Vec<String> = transaction
        .prepare("SELECT migration FROM migrations WHERE domain = ?1 ORDER BY step")?
        .query_map([schema.domain], |row| {
            Ok(row.get::<_, Option<String>>(0)?.unwrap_or_default())
        })?
        .collect::<rusqlite::Result<_>>()?;
    if ran.len() > schema.steps.len() {
        return Err(Error::Newer {
            path: transaction.path().map(Into::into).unwrap_or_default(),
            applied: ran.len(),
            known: schema.steps.len(),
        });
    }
    for (step, (ran, now)) in ran.iter().zip(schema.steps).enumerate() {
        if !same_step(ran, now) {
            return Err(Error::MigrationChanged {
                domain: schema.domain,
                step,
                ran: ran.clone(),
                now: (*now).to_string(),
            });
        }
    }
    for (step, sql) in schema.steps.iter().enumerate().skip(ran.len()) {
        transaction
            .execute_batch(sql)
            .map_err(|source| Error::MigrationFailed {
                domain: schema.domain,
                step,
                source,
            })?;
        transaction.execute(
            "INSERT INTO migrations (domain, step, migration) VALUES (?1, ?2, ?3)",
            (schema.domain, step, sql),
        )?;
    }
    if ran.len() < schema.steps.len() {
        let rows = broken_foreign_keys(&transaction)?;
        if !rows.is_empty() {
            return Err(Error::ForeignKeys {
                domain: schema.domain,
                rows,
            });
        }
    }
    transaction.commit()?;
    Ok(ran.len())
}

/// `PRAGMA foreign_key_check`'s rows, as `table rowid → parent`.
fn broken_foreign_keys(connection: &Connection) -> Result<Vec<String>> {
    Ok(connection
        .prepare("PRAGMA foreign_key_check")?
        .query_map([], |row| {
            let table: String = row.get(0)?;
            let rowid: Option<i64> = row.get(1)?;
            let parent: String = row.get(2)?;
            Ok(match rowid {
                Some(rowid) => format!("{table} {rowid} → {parent}"),
                None => format!("{table} → {parent}"),
            })
        })?
        .collect::<rusqlite::Result<_>>()?)
}

/**
 * How many of `domain`'s steps the database on `connection` records: 0 without a `migrations`
 * table. The guard reads this before anything is written.
 */
pub(crate) fn recorded_steps(connection: &Connection, domain: &str) -> rusqlite::Result<usize> {
    let has_table = connection
        .query_row(
            "SELECT 1 FROM sqlite_schema WHERE type = 'table' AND name = 'migrations' COLLATE NOCASE",
            [],
            |_| Ok(()),
        )
        .optional()?
        .is_some();
    if !has_table {
        return Ok(0);
    }
    connection.query_row(
        "SELECT count(*) FROM migrations WHERE domain = ?1",
        [domain],
        |row| row.get(0),
    )
}

/**
 * Whether the recorded step `ran` is the step `now`: the same once whitespace and comments
 * outside string literals and quoted names are dropped. sqlformat, which wrote every step sqlez
 * recorded, only changes layout, so this holds for each of them and for no real edit.
 */
pub(crate) fn same_step(ran: &str, now: &str) -> bool {
    significant(ran).eq(significant(now))
}

/// The characters of `sql` that matter: everything but whitespace and comments outside quotes.
fn significant(sql: &str) -> impl Iterator<Item = char> + '_ {
    let mut chars = sql.chars().peekable();
    let mut quote: Option<char> = None;
    std::iter::from_fn(move || {
        loop {
            let c = chars.next()?;
            if let Some(close) = quote {
                if c == close {
                    quote = None;
                }
                return Some(c);
            }
            match c {
                '\'' | '"' | '`' => {
                    quote = Some(c);
                    return Some(c);
                }
                '[' => {
                    quote = Some(']');
                    return Some(c);
                }
                '-' if chars.peek() == Some(&'-') => {
                    for c in chars.by_ref() {
                        if c == '\n' {
                            break;
                        }
                    }
                }
                '/' if chars.peek() == Some(&'*') => {
                    chars.next();
                    let mut previous = ' ';
                    for c in chars.by_ref() {
                        if previous == '*' && c == '/' {
                            break;
                        }
                        previous = c;
                    }
                }
                c if c.is_whitespace() => {}
                c => return Some(c),
            }
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn layout_and_comments_dont_count_but_everything_else_does() {
        let written = "CREATE TABLE groups (
  id INTEGER PRIMARY KEY, -- the group
  parent_id INTEGER REFERENCES groups(id) ON DELETE SET NULL, /* a tree */
  name TEXT NOT NULL CHECK (name <> '' AND name <> 'a  b')
);";
        let formatted = "CREATE TABLE groups (
  id INTEGER PRIMARY KEY,
  -- the group
  parent_id INTEGER REFERENCES groups(id) ON DELETE
  SET
    NULL,
    /* a tree */
    name TEXT NOT NULL CHECK (
      name <> ''
      AND name <> 'a  b'
    )
);";
        assert!(same_step(written, formatted));
        for edit in [
            "CREATE TABLE groups (id INTEGER PRIMARY KEY, parent_id INTEGER REFERENCES groups(id) \
             ON DELETE CASCADE, name TEXT NOT NULL CHECK (name <> '' AND name <> 'a  b'));",
            "CREATE TABLE groups (id INTEGER PRIMARY KEY, parent_id INTEGER REFERENCES groups(id) \
             ON DELETE SET NULL, name TEXT NOT NULL CHECK (name <> '' AND name <> 'a b'));",
            "create table groups (id INTEGER PRIMARY KEY, parent_id INTEGER REFERENCES groups(id) \
             ON DELETE SET NULL, name TEXT NOT NULL CHECK (name <> '' AND name <> 'a  b'));",
        ] {
            assert!(!same_step(formatted, edit), "{edit}");
        }
    }

    #[test]
    fn quotes_hide_comment_and_space_lookalikes() {
        assert!(!same_step("SELECT '-- x'", "SELECT ''"));
        assert!(!same_step("SELECT \"a b\"", "SELECT \"ab\""));
        assert!(!same_step("SELECT [a b]", "SELECT [ab]"));
        assert!(same_step("SELECT 1 -- it's fine\n", "SELECT\n  1"));
        assert!(same_step("SELECT 1 /* it's * fine */ + 2", "SELECT 1 + 2"));
    }
}
