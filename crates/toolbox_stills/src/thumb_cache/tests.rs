use std::fs;
use std::path::Path;
use std::time::{Duration, SystemTime};

use super::*;

/// Two thumbnail sizes, as an app's grid and small cards ask for.
const GRID: u32 = 320;
const SMALL: u32 = 160;

/// A `side`×`side` picture of solid `bgra`.
fn solid(side: u32, bgra: [u8; 4]) -> Bgra {
    Bgra::new(side, side, bgra.repeat((side * side) as usize)).unwrap()
}

fn age(file: &Path, by: Duration) {
    File::options()
        .write(true)
        .open(file)
        .unwrap()
        .set_modified(SystemTime::now() - by)
        .unwrap();
}

#[test]
fn keys_follow_the_path_size_and_time() {
    let key = ThumbKey::new(Path::new("/photos/a.jpg"), 100, 7);
    assert_eq!(key.as_str().len(), 64);
    assert!(key.as_str().bytes().all(|byte| byte.is_ascii_hexdigit()));
    assert_eq!(key, ThumbKey::new(Path::new("/photos/a.jpg"), 100, 7));
    for other in [
        ThumbKey::new(Path::new("/photos/b.jpg"), 100, 7),
        ThumbKey::new(Path::new("/photos/a.jpg"), 101, 7),
        ThumbKey::new(Path::new("/photos/a.jpg"), 100, 8),
    ] {
        assert_ne!(key, other);
    }
    /* `<path>|<size>|<mtime_ns>`, with the path in the OS's own form (UTF-16 on Windows). */
    let mut text = path_bytes(Path::new("/a"));
    text.extend_from_slice(b"|1|2");
    assert_eq!(
        ThumbKey::new(Path::new("/a"), 1, 2).as_str(),
        Sha256::digest(&text)
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect::<String>()
    );
}

#[test]
fn a_stored_thumbnail_loads_back_at_its_size() {
    let dir = tempfile::tempdir().unwrap();
    let cache = ThumbCache::new(dir.path().join("thumbs"));
    let key = ThumbKey::new(Path::new("/photos/a.jpg"), 100, 7);
    assert_eq!(cache.load(&key, GRID), None);

    let image = solid(32, [40, 120, 200, 255]);
    let stored = cache.store(&key, GRID, &image).unwrap();
    let file = cache.file(&key, GRID);
    assert_eq!(
        file,
        dir.path()
            .join("thumbs")
            .join(&key.as_str()[..2])
            .join(format!("{}-320.jpg", key.as_str()))
    );
    let loaded = cache.load(&key, GRID).unwrap();
    assert_eq!(loaded, stored, "what was shown is what loads back");
    assert_eq!((loaded.width(), loaded.height()), (32, 32));
    /* JPEG is lossy, but a flat colour comes back within a level or two. */
    for px in loaded.pixels().as_chunks::<4>().0 {
        for (got, want) in px.iter().zip([40u8, 120, 200, 255]) {
            assert!(got.abs_diff(want) <= 2, "{px:?}");
        }
    }
    /* Each size is its own file. */
    assert_eq!(cache.load(&key, SMALL), None);
    /* No temporary file is left. */
    let names: Vec<_> = fs::read_dir(file.parent().unwrap())
        .unwrap()
        .map(|entry| entry.unwrap().file_name())
        .collect();
    assert_eq!(names.len(), 1);
}

#[test]
fn transparency_is_flattened_onto_black() {
    let dir = tempfile::tempdir().unwrap();
    let cache = ThumbCache::new(dir.path().to_owned());
    let key = ThumbKey::new(Path::new("/a.png"), 1, 1);
    cache
        .store(&key, 160, &solid(16, [200, 200, 200, 128]))
        .unwrap();
    let px = cache.load(&key, 160).unwrap().pixels()[..4].to_vec();
    assert!(
        px[..3].iter().all(|channel| channel.abs_diff(100) <= 2),
        "{px:?}"
    );
    assert_eq!(px[3], 255);
}

#[test]
fn a_damaged_thumbnail_is_removed_and_missed() {
    let dir = tempfile::tempdir().unwrap();
    let cache = ThumbCache::new(dir.path().to_owned());
    let key = ThumbKey::new(Path::new("/a.jpg"), 1, 1);
    cache.store(&key, 320, &solid(8, [0, 0, 0, 255])).unwrap();
    let file = cache.file(&key, 320);
    fs::write(&file, b"not a jpeg").unwrap();
    assert_eq!(cache.load(&key, 320), None);
    assert!(!file.exists());
}

#[test]
fn a_hit_marks_an_old_thumbnail_used() {
    let dir = tempfile::tempdir().unwrap();
    let cache = ThumbCache::new(dir.path().to_owned());
    let key = ThumbKey::new(Path::new("/a.jpg"), 1, 1);
    cache.store(&key, 320, &solid(8, [0, 0, 0, 255])).unwrap();
    let file = cache.file(&key, 320);
    age(&file, Duration::from_secs(3 * 60 * 60));
    cache.load(&key, 320).unwrap();
    let modified = fs::metadata(&file).unwrap().modified().unwrap();
    assert!(SystemTime::now().duration_since(modified).unwrap() < Duration::from_secs(60));
}

#[test]
fn pruning_removes_the_least_recently_used_first() {
    let dir = tempfile::tempdir().unwrap();
    let cache = ThumbCache::new(dir.path().to_owned());
    assert_eq!(cache.prune(0).unwrap(), Pruned::default(), "no folder yet");
    let keys: Vec<_> = (0..10)
        .map(|index| ThumbKey::new(Path::new(&format!("/{index}.jpg")), 1, 1))
        .collect();
    for (index, key) in keys.iter().enumerate() {
        cache
            .store(key, 320, &solid(64, [index as u8 * 20, 0, 0, 255]))
            .unwrap();
        /* Key 0 is the oldest, key 9 the newest. */
        age(
            &cache.file(key, 320),
            Duration::from_secs(1000 - index as u64 * 10),
        );
    }
    let sizes: Vec<u64> = keys
        .iter()
        .map(|key| fs::metadata(cache.file(key, 320)).unwrap().len())
        .collect();
    let total: u64 = sizes.iter().sum();
    /* Under budget: nothing goes. */
    assert_eq!(
        cache.prune(total).unwrap(),
        Pruned {
            removed: 0,
            kept_bytes: total
        }
    );
    /* A stale temporary file goes whatever the budget. */
    let temp = cache.file(&keys[0], 320).with_extension("1-1.tmp");
    fs::write(&temp, b"half").unwrap();
    age(&temp, Duration::from_secs(2 * 60 * 60));
    /* Over budget: the oldest go until 90 % of the budget is left. */
    let budget = total - 1;
    let pruned = cache.prune(budget).unwrap();
    assert!(!temp.exists());
    assert!(pruned.kept_bytes <= budget / 10 * 9);
    let left: Vec<bool> = keys
        .iter()
        .map(|key| cache.file(key, 320).exists())
        .collect();
    let gone = left.iter().take_while(|exists| !**exists).count();
    assert!(gone >= 1, "{left:?}");
    assert!(left[gone..].iter().all(|exists| *exists), "{left:?}");
    assert_eq!(pruned.removed, gone + 1);
    assert_eq!(pruned.kept_bytes, sizes[gone..].iter().sum::<u64>());
    /* What is left is the usage; clearing deletes all of it. */
    assert_eq!(cache.usage().unwrap(), pruned.kept_bytes);
    assert_eq!(cache.clear().unwrap().kept_bytes, 0);
    assert_eq!(cache.usage().unwrap(), 0);
    assert!(keys.iter().all(|key| !cache.file(key, 320).exists()));
}

/// A folder of the cache that can't be read is passed over: the rest is still pruned.
#[cfg(unix)]
#[test]
fn pruning_passes_over_what_it_cannot_read() {
    use std::os::unix::fs::PermissionsExt as _;

    let dir = tempfile::tempdir().unwrap();
    let cache = ThumbCache::new(dir.path().to_owned());
    let keys: Vec<_> = (0..40)
        .map(|index| ThumbKey::new(Path::new(&format!("/{index}.jpg")), 1, 1))
        .collect();
    for key in &keys {
        cache.store(key, 320, &solid(64, [90, 0, 0, 255])).unwrap();
    }
    let locked = cache.file(&keys[0], 320).parent().unwrap().to_owned();
    fs::set_permissions(&locked, fs::Permissions::from_mode(0o000)).unwrap();
    let pruned = cache.prune(0);
    fs::set_permissions(&locked, fs::Permissions::from_mode(0o755)).unwrap();
    let pruned = pruned.expect("an unreadable folder doesn't stop the prune");
    let left = keys
        .iter()
        .filter(|key| cache.file(key, 320).exists())
        .count();
    let in_locked = fs::read_dir(&locked).unwrap().count();
    assert_eq!(left, in_locked, "everything readable went");
    assert_eq!(pruned.removed, keys.len() - in_locked);
}
