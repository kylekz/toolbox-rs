//! The thumbnail disk cache (docs/stills.md "The thumbnail cache"): JPEGs in a folder of the
//! app's data dir, never beside the media.
//!
//! - **Key:** SHA-256 of the file's path bytes, size and modification time
//!   (`<path>|<size>|<mtime_ns>`), so a file that changes on disk gets a new key and its old
//!   thumbnail is never shown again; the stale one ages out.
//! - **Layout:** `<dir>/<first two hex digits>/<hex>-<side>.jpg`, one file per size asked for,
//!   written whole through a temporary file and a rename, so a reader never sees half a JPEG.
//! - **Pixels:** JPEG quality 85 has no alpha: a transparent picture is flattened onto black.
//!   Read back, it is opaque BGRA8 like every other thumbnail.
//! - **Pruning:** least recently used first, by file modification time, which [`ThumbCache::load`]
//!   moves forward on a hit (at most once an hour per file), down to 90 % of the budget so it
//!   doesn't prune again at the next write.
//!
//! Every call blocks on the disk: call them off the UI thread.

use std::fs::{self, File};
use std::io;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, SystemTime};

use image::codecs::jpeg::JpegEncoder;
use image::{ExtendedColorType, ImageEncoder as _};
use sha2::{Digest as _, Sha256};

use crate::decode::Bgra;

/// The JPEG quality thumbnails are saved at.
const QUALITY: u8 = 85;

/// A hit moves a file's modification time forward only when it is older than this.
const TOUCH_AFTER: Duration = Duration::from_secs(60 * 60);

/// A temporary file older than this was left by a write that never finished.
const STALE_TEMP: Duration = Duration::from_secs(60 * 60);

/// What a file's thumbnails are cached under: the hex SHA-256 of its path, size and time.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct ThumbKey(String);

impl ThumbKey {
    pub fn new(path: &Path, size_bytes: u64, mtime_ns: i64) -> Self {
        let mut hash = Sha256::new();
        hash.update(path_bytes(path));
        hash.update(format!("|{size_bytes}|{mtime_ns}"));
        Self(
            hash.finalize()
                .iter()
                .map(|byte| format!("{byte:02x}"))
                .collect(),
        )
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// The path's bytes as the OS gives them.
#[cfg(unix)]
fn path_bytes(path: &Path) -> Vec<u8> {
    use std::os::unix::ffi::OsStrExt as _;
    path.as_os_str().as_bytes().to_vec()
}

/// The path's UTF-16 units, little-endian: Windows' own form, which any path round-trips in.
#[cfg(windows)]
fn path_bytes(path: &Path) -> Vec<u8> {
    use std::os::windows::ffi::OsStrExt as _;
    path.as_os_str()
        .encode_wide()
        .flat_map(u16::to_le_bytes)
        .collect()
}

/// What [`ThumbCache::prune`] did.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Pruned {
    pub removed: usize,
    /// The thumbnails' bytes left on disk.
    pub kept_bytes: u64,
}

/// The cache in one folder, such as the data dir's `cache/thumbs`.
#[derive(Clone, Debug)]
pub struct ThumbCache {
    dir: PathBuf,
}

impl ThumbCache {
    /// The cache in `dir`, created on the first write.
    pub fn new(dir: PathBuf) -> Self {
        Self { dir }
    }

    /// Where a thumbnail of `side` for `key` is kept.
    pub fn file(&self, key: &ThumbKey, side: u32) -> PathBuf {
        self.dir
            .join(&key.0[..2])
            .join(format!("{}-{side}.jpg", key.0))
    }

    /**
     * The thumbnail of `side` for `key`, as opaque BGRA8, or `None` when there is none. A file
     * that doesn't decode is deleted and counts as none, so it is made again.
     */
    pub fn load(&self, key: &ThumbKey, side: u32) -> Option<Bgra> {
        let file = self.file(key, side);
        let bytes = match fs::read(&file) {
            Ok(bytes) => bytes,
            Err(error) if error.kind() == io::ErrorKind::NotFound => return None,
            Err(error) => {
                log::warn!("reading the thumbnail {}: {error}", file.display());
                return None;
            }
        };
        let Some(image) = decode_jpeg(&bytes) else {
            log::warn!("the thumbnail {} is damaged, removing it", file.display());
            let _ = fs::remove_file(&file);
            return None;
        };
        touch(&file);
        Some(image)
    }

    /**
     * Saves `image` as the thumbnail of `side` for `key`, replacing any it had, and returns it
     * as [`ThumbCache::load`] will read it back, so a tile shows the same pixels whether its
     * thumbnail was just made or read from the cache. On an error nothing is saved.
     */
    pub fn store(&self, key: &ThumbKey, side: u32, image: &Bgra) -> io::Result<Bgra> {
        static TEMP: AtomicU64 = AtomicU64::new(0);
        let mut jpeg = Vec::new();
        JpegEncoder::new_with_quality(&mut jpeg, QUALITY)
            .write_image(
                &flatten(image),
                image.width(),
                image.height(),
                ExtendedColorType::Rgb8,
            )
            .map_err(io::Error::other)?;
        let file = self.file(key, side);
        let folder = file
            .parent()
            .expect("a thumbnail's file is inside its folder");
        fs::create_dir_all(folder)?;
        let temp = folder.join(format!(
            "{}.{}-{}.tmp",
            key.0,
            std::process::id(),
            TEMP.fetch_add(1, Ordering::Relaxed)
        ));
        let written = fs::write(&temp, &jpeg).and_then(|()| fs::rename(&temp, &file));
        if let Err(error) = written {
            let _ = fs::remove_file(&temp);
            return Err(error);
        }
        decode_jpeg(&jpeg).ok_or_else(|| io::Error::other("the JPEG just written doesn't decode"))
    }

    /**
     * The bytes the thumbnails take on disk (a settings page's "used"). It walks the cache as
     * [`ThumbCache::prune`] does, under a budget nothing reaches, so it deletes only the
     * temporary files writes left.
     */
    pub fn usage(&self) -> io::Result<u64> {
        self.prune(u64::MAX).map(|pruned| pruned.kept_bytes)
    }

    /**
     * Deletes every thumbnail (a settings page's Clear): a prune to nothing. Tiles on screen
     * keep the pictures they have in memory, and a thumbnail asked for again is made again.
     */
    pub fn clear(&self) -> io::Result<Pruned> {
        self.prune(0)
    }

    /**
     * Deletes the least recently used thumbnails until the rest take at most 90 % of
     * `budget` bytes, when they take more than `budget`, and any temporary file a write left.
     * A file that goes meanwhile (another prune, a write's rename) is passed over, and one that
     * can't be read is logged and left: only an unreadable cache folder fails the prune.
     */
    pub fn prune(&self, budget: u64) -> io::Result<Pruned> {
        let mut files = Vec::new();
        let mut removed = 0;
        let now = SystemTime::now();
        let folders = match fs::read_dir(&self.dir) {
            Ok(folders) => folders,
            Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(Pruned::default()),
            Err(error) => return Err(error),
        };
        for folder in folders.filter_map(|folder| passed_over(folder, &self.dir)) {
            if !folder.file_type().is_ok_and(|kind| kind.is_dir()) {
                continue;
            }
            let entries = match fs::read_dir(folder.path()) {
                Ok(entries) => entries,
                Err(error) => {
                    skip(&error, &folder.path());
                    continue;
                }
            };
            for entry in entries.filter_map(|entry| passed_over(entry, &folder.path())) {
                let path = entry.path();
                let (modified, len) = match entry
                    .metadata()
                    .and_then(|meta| Ok((meta.modified()?, meta.len())))
                {
                    Ok(found) => found,
                    Err(error) => {
                        skip(&error, &path);
                        continue;
                    }
                };
                match path.extension().and_then(|extension| extension.to_str()) {
                    Some("jpg") => files.push((modified, len, path)),
                    Some("tmp")
                        if now.duration_since(modified).unwrap_or_default() > STALE_TEMP =>
                    {
                        removed += usize::from(fs::remove_file(&path).is_ok());
                    }
                    _ => {}
                }
            }
        }
        let mut kept_bytes: u64 = files.iter().map(|(_, len, _)| len).sum();
        if kept_bytes > budget {
            let target = budget / 10 * 9;
            files.sort_by_key(|(modified, _, _)| *modified);
            for (_, len, path) in files {
                if kept_bytes <= target {
                    break;
                }
                match fs::remove_file(&path) {
                    Ok(()) => {
                        kept_bytes -= len;
                        removed += 1;
                    }
                    /* Gone already: it no longer takes room. */
                    Err(error) if error.kind() == io::ErrorKind::NotFound => kept_bytes -= len,
                    Err(error) => log::warn!("pruning {}: {error}", path.display()),
                }
            }
        }
        Ok(Pruned {
            removed,
            kept_bytes,
        })
    }
}

/// A directory entry, or `None` (logged unless it just went) when it can't be read.
fn passed_over(entry: io::Result<fs::DirEntry>, folder: &Path) -> Option<fs::DirEntry> {
    entry.inspect_err(|error| skip(error, folder)).ok()
}

/// Logs a cache file or folder the prune passes over, unless it went meanwhile.
fn skip(error: &io::Error, path: &Path) {
    if error.kind() != io::ErrorKind::NotFound {
        log::warn!(
            "pruning the thumbnail cache, skipping {}: {error}",
            path.display()
        );
    }
}

/// A thumbnail's JPEG as opaque BGRA8, or `None` when it doesn't decode.
fn decode_jpeg(jpeg: &[u8]) -> Option<Bgra> {
    let rgb = image::load_from_memory_with_format(jpeg, image::ImageFormat::Jpeg)
        .ok()?
        .into_rgb8();
    let (width, height) = rgb.dimensions();
    let pixels = rgb
        .as_chunks::<3>()
        .0
        .iter()
        .flat_map(|&[r, g, b]| [b, g, r, 255])
        .collect();
    Bgra::new(width, height, pixels)
}

/// Straight-alpha BGRA8 as RGB8 over black.
fn flatten(image: &Bgra) -> Vec<u8> {
    image
        .pixels()
        .as_chunks::<4>()
        .0
        .iter()
        .flat_map(|&[b, g, r, a]| {
            let over_black = |channel: u8| ((u16::from(channel) * u16::from(a) + 127) / 255) as u8;
            [over_black(r), over_black(g), over_black(b)]
        })
        .collect()
}

/// Marks a hit as recently used for pruning, unless it already is.
fn touch(file: &Path) {
    let now = SystemTime::now();
    let stale = fs::metadata(file)
        .and_then(|meta| meta.modified())
        .is_ok_and(|modified| now.duration_since(modified).unwrap_or_default() > TOUCH_AFTER);
    if stale
        && let Err(error) = File::options()
            .write(true)
            .open(file)
            .and_then(|file| file.set_modified(now))
    {
        log::warn!("marking {} used: {error}", file.display());
    }
}

#[cfg(test)]
mod tests;
