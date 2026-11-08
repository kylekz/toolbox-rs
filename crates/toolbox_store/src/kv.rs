//! The key-value table (docs/store.md "Key-value"): small app-wide settings and notes as text,
//! one row per key, in a table the app's own migration creates (slide's `kv`, fc's `app_state`).
//! The functions take a connection, so they run inside the caller's read or write.

use rusqlite::{Connection, OptionalExtension as _};

use crate::error::Result;

/**
 * A table of `key TEXT PRIMARY KEY` and `value TEXT` rows, named by the app. A value that should
 * be typed (a word, a number, JSON) is the app's to parse; [`Kv::get_parsed`] logs one that doesn't.
 */
#[derive(Clone, Copy, Debug)]
pub struct Kv {
    table: &'static str,
}

impl Kv {
    /**
     * The table named `table`: ASCII letters, digits and `_`, not starting with a digit, since
     * it is written into the SQL. Checked when the constant is built, so a bad name fails the
     * build: `const SETTINGS: Kv = Kv::new("kv");`.
     */
    pub const fn new(table: &'static str) -> Self {
        let bytes = table.as_bytes();
        assert!(
            !bytes.is_empty() && !bytes[0].is_ascii_digit(),
            "a kv table name starts with a letter or _"
        );
        let mut i = 0;
        while i < bytes.len() {
            assert!(
                bytes[i].is_ascii_alphanumeric() || bytes[i] == b'_',
                "a kv table name is ASCII letters, digits and _"
            );
            i += 1;
        }
        Self { table }
    }

    pub fn table(&self) -> &'static str {
        self.table
    }

    pub fn get(&self, connection: &Connection, key: &str) -> Result<Option<String>> {
        Ok(connection
            .prepare_cached(&format!("SELECT value FROM {} WHERE key = ?1", self.table))?
            .query_row([key], |row| row.get(0))
            .optional()?)
    }

    /// Sets `key` to `value`, replacing what was there.
    pub fn set(&self, connection: &Connection, key: &str, value: &str) -> Result<()> {
        connection
            .prepare_cached(&format!(
                "INSERT INTO {} (key, value) VALUES (?1, ?2)
                 ON CONFLICT (key) DO UPDATE SET value = excluded.value",
                self.table
            ))?
            .execute([key, value])?;
        Ok(())
    }

    /// Removes `key`; returns whether it was there.
    pub fn delete(&self, connection: &Connection, key: &str) -> Result<bool> {
        let removed = connection
            .prepare_cached(&format!("DELETE FROM {} WHERE key = ?1", self.table))?
            .execute([key])?;
        Ok(removed > 0)
    }

    /**
     * `key`'s value through `parse`: `None` when it is missing or `parse` can't read it, which is
     * logged, so one bad row reads as the app's default instead of failing what reads it.
     */
    pub fn get_parsed<T>(
        &self,
        connection: &Connection,
        key: &str,
        parse: impl FnOnce(&str) -> Option<T>,
    ) -> Result<Option<T>> {
        Ok(self.get(connection, key)?.and_then(|text| {
            let parsed = parse(&text);
            if parsed.is_none() {
                log::warn!(
                    "ignoring the unreadable {} value {key} = {text:?}",
                    self.table
                );
            }
            parsed
        }))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const SETTINGS: Kv = Kv::new("kv");

    fn connection() -> Connection {
        let connection = Connection::open_in_memory().unwrap();
        connection
            .execute_batch("CREATE TABLE kv (key TEXT PRIMARY KEY, value TEXT NOT NULL)")
            .unwrap();
        connection
    }

    #[test]
    fn values_are_set_replaced_and_deleted() {
        let c = connection();
        assert_eq!(SETTINGS.get(&c, "player.hardware").unwrap(), None);
        SETTINGS.set(&c, "player.hardware", "on").unwrap();
        SETTINGS.set(&c, "player.hardware", "off").unwrap();
        assert_eq!(
            SETTINGS.get(&c, "player.hardware").unwrap().as_deref(),
            Some("off")
        );
        assert!(SETTINGS.delete(&c, "player.hardware").unwrap());
        assert!(!SETTINGS.delete(&c, "player.hardware").unwrap());
        assert_eq!(SETTINGS.get(&c, "player.hardware").unwrap(), None);
    }

    #[test]
    fn an_unreadable_value_reads_as_missing() {
        let c = connection();
        SETTINGS.set(&c, "volume", "loud").unwrap();
        SETTINGS.set(&c, "hold_ms", "500").unwrap();
        let number = |text: &str| text.parse::<u32>().ok();
        assert_eq!(SETTINGS.get_parsed(&c, "volume", number).unwrap(), None);
        assert_eq!(
            SETTINGS.get_parsed(&c, "hold_ms", number).unwrap(),
            Some(500)
        );
        assert_eq!(SETTINGS.get_parsed(&c, "missing", number).unwrap(), None);
    }

    #[test]
    #[should_panic(expected = "ASCII letters")]
    fn a_table_name_that_isnt_a_plain_name_is_refused() {
        Kv::new("kv; DROP TABLE kv");
    }
}
