//! Paths stored as the OS's own bytes (docs/store.md "Paths"), as sqlez bound a `Path`: a blob of
//! `OsStr::as_encoded_bytes`, which is the raw bytes on Unix and WTF-8 on Windows. Any path the OS
//! gives round-trips exactly, including ones that aren't valid Unicode.

use std::path::{Path, PathBuf};

use rusqlite::types::{FromSql, FromSqlError, FromSqlResult, ToSql, ToSqlOutput, ValueRef};

/// Binds a borrowed path as its OS bytes: `params![OsPath(&item.path)]`.
#[derive(Clone, Copy, Debug)]
pub struct OsPath<'a>(pub &'a Path);

impl ToSql for OsPath<'_> {
    fn to_sql(&self) -> rusqlite::Result<ToSqlOutput<'_>> {
        Ok(ToSqlOutput::Borrowed(ValueRef::Blob(
            self.0.as_os_str().as_encoded_bytes(),
        )))
    }
}

/**
 * Reads (and binds) a path stored as its OS bytes: `row.get::<_, OsPathBuf>(0)?.0`. A text value
 * is read as its UTF-8 bytes; bytes that aren't a path on this OS (invalid WTF-8 on Windows) are
 * an error.
 */
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct OsPathBuf(pub PathBuf);

impl ToSql for OsPathBuf {
    fn to_sql(&self) -> rusqlite::Result<ToSqlOutput<'_>> {
        Ok(ToSqlOutput::Borrowed(ValueRef::Blob(
            self.0.as_os_str().as_encoded_bytes(),
        )))
    }
}

impl FromSql for OsPathBuf {
    fn column_result(value: ValueRef<'_>) -> FromSqlResult<Self> {
        let bytes = match value {
            ValueRef::Blob(bytes) | ValueRef::Text(bytes) => bytes,
            _ => return Err(FromSqlError::InvalidType),
        };
        path_from_bytes(bytes)
            .map(Self)
            .ok_or_else(|| FromSqlError::Other("the stored bytes are not a path on this OS".into()))
    }
}

#[cfg(unix)]
fn path_from_bytes(bytes: &[u8]) -> Option<PathBuf> {
    use std::os::unix::ffi::OsStrExt as _;
    Some(PathBuf::from(std::ffi::OsStr::from_bytes(bytes)))
}

#[cfg(windows)]
fn path_from_bytes(bytes: &[u8]) -> Option<PathBuf> {
    use std::os::windows::ffi::OsStringExt as _;
    wtf8_to_wide(bytes).map(|wide| PathBuf::from(std::ffi::OsString::from_wide(&wide)))
}

/**
 * Decodes WTF-8 (UTF-8 that may also hold unpaired surrogates, each as its own three bytes) into
 * the UTF-16 code units Windows names paths in. `None` for anything else: bad or overlong
 * sequences, code points past U+10FFFF, or a surrogate pair written as two halves.
 */
#[cfg(any(windows, test))]
fn wtf8_to_wide(bytes: &[u8]) -> Option<Vec<u16>> {
    fn continuation(byte: Option<&u8>) -> Option<u32> {
        byte.filter(|byte| (0x80..=0xBF).contains(*byte))
            .map(|byte| u32::from(byte & 0x3F))
    }
    let mut wide = Vec::with_capacity(bytes.len());
    let mut after_lead_surrogate = false;
    let mut at = 0;
    while at < bytes.len() {
        let first = u32::from(bytes[at]);
        let (code, length, least) = match first {
            0x00..=0x7F => (first, 1, 0),
            0xC0..=0xDF => (
                (first & 0x1F) << 6 | continuation(bytes.get(at + 1))?,
                2,
                0x80,
            ),
            0xE0..=0xEF => (
                (first & 0x0F) << 12
                    | continuation(bytes.get(at + 1))? << 6
                    | continuation(bytes.get(at + 2))?,
                3,
                0x800,
            ),
            0xF0..=0xF7 => (
                (first & 0x07) << 18
                    | continuation(bytes.get(at + 1))? << 12
                    | continuation(bytes.get(at + 2))? << 6
                    | continuation(bytes.get(at + 3))?,
                4,
                0x1_0000,
            ),
            _ => return None,
        };
        if code < least || code > 0x10_FFFF {
            return None;
        }
        let trail_surrogate = (0xDC00..=0xDFFF).contains(&code);
        if trail_surrogate && after_lead_surrogate {
            return None;
        }
        after_lead_surrogate = (0xD800..=0xDBFF).contains(&code);
        match u16::try_from(code) {
            Ok(unit) => wide.push(unit),
            Err(_) => {
                let code = code - 0x1_0000;
                wide.extend([0xD800 | (code >> 10) as u16, 0xDC00 | (code & 0x3FF) as u16]);
            }
        }
        at += length;
    }
    Some(wide)
}

#[cfg(test)]
mod tests {
    use rusqlite::Connection;

    use super::*;

    #[test]
    fn a_path_round_trips_as_a_blob() {
        let connection = Connection::open_in_memory().unwrap();
        let path = Path::new("/photos/Été 2026/IMG_0001.HEIC");
        let (kind, back): (String, OsPathBuf) = connection
            .query_row("SELECT typeof(?1), ?1", [OsPath(path)], |row| {
                Ok((row.get(0)?, row.get(1)?))
            })
            .unwrap();
        assert_eq!(kind, "blob");
        assert_eq!(back.0, path);
        let from_text: OsPathBuf = connection
            .query_row("SELECT '/a/b.jpg'", [], |row| row.get(0))
            .unwrap();
        assert_eq!(from_text.0, Path::new("/a/b.jpg"));
    }

    #[cfg(unix)]
    #[test]
    fn bytes_that_arent_unicode_round_trip_on_unix() {
        use std::os::unix::ffi::OsStrExt as _;
        let path = Path::new(std::ffi::OsStr::from_bytes(b"/data/\xFFname"));
        let connection = Connection::open_in_memory().unwrap();
        let back: OsPathBuf = connection
            .query_row("SELECT ?1", [OsPath(path)], |row| row.get(0))
            .unwrap();
        assert_eq!(back.0, path);
    }

    #[test]
    fn wtf8_decodes_to_the_utf16_windows_uses() {
        let text = "C:\\a€😀";
        assert_eq!(
            wtf8_to_wide(text.as_bytes()),
            Some(text.encode_utf16().collect())
        );
        /* An unpaired surrogate, as Windows allows in names. */
        assert_eq!(
            wtf8_to_wide(b"a\xED\xA0\x80b"),
            Some(vec![0x61, 0xD800, 0x62])
        );
        assert_eq!(wtf8_to_wide(b"\xED\xB0\x80"), Some(vec![0xDC00]));
        for bad in [
            &b"\xED\xA0\x80\xED\xB0\x80"[..],
            b"\xC0\x80",
            b"\xE0\x80\x80",
            b"\xF4\x90\x80\x80",
            b"\xFF",
            b"\xE2\x82",
            b"\x80",
        ] {
            assert_eq!(wtf8_to_wide(bad), None, "{bad:?}");
        }
    }
}
