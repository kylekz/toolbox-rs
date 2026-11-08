# toolbox_stills: still images

`crates/toolbox_stills` decodes still images (and GIF/WebP/APNG animations) through the
operating system's own decoders, and caches thumbnails. It has no gpui dependency
(`script/check` step 3). PLAN §5a explains why it exists; this page is its reference, and says
how slide and fc adopt it.

Its seed is slide's `slide_media` at slide `0560027`, without `scan.rs`, which stays in slide.
`slide_media` used no gpui types, so nothing gpui-shaped had to stay behind.

## What it does

| | macOS | Windows | Linux (CI only today) |
|---|---|---|---|
| Still decoder | ImageIO | WIC | the `image` crate |
| Colour management to sRGB | yes, from the file's profile | yes, from the file's profile (a HEIC's from its `colr` box) | no |
| EXIF / HEIF orientation | yes | yes | EXIF only |
| JPEG, PNG, GIF, BMP, WebP | yes | yes | yes |
| TIFF | yes | yes | no |
| HEIC, AVIF | yes, always | when the Store codecs are installed | no |
| Animations (GIF, WebP, APNG) | the `image` crate everywhere | | |

The OS decoders are 1.5–6× faster than the `image` crate for JPEG at display size (slide's
measurement, its ARCHITECTURE D4), because they decode at reduced scale. When the OS decoder
returns no image, the `image` crate decodes JPEG, PNG, GIF, BMP and WebP instead, without colour
management.

## API

Everything is re-exported at the crate root. Every function blocks except the `Decoder`'s
futures; call them off the UI thread.

### Sniffing

```rust
pub fn sniff_file(path: &Path) -> io::Result<Option<Media>>;
pub fn sniff_bytes(bytes: &[u8], extension: Option<&str>) -> Option<Media>;

pub enum Media { Picture(Picture), Video(VideoFormat) }
pub enum Picture { Image(ImageFormat), Animation(AnimationFormat) }
pub enum ImageFormat { Jpeg, Png, Gif, Bmp, Webp, Tiff, Heif, Avif }
pub enum AnimationFormat { Gif, Webp, Apng }
pub enum VideoFormat { Mp4, Mov, Mkv, Webm }
pub enum MediaKind { Image, Animation, Video }

Media::kind(self) -> MediaKind;
Media::picture(self) -> Option<Picture>;
Picture::still_format(self) -> ImageFormat;   // an animation's first frame's format
ImageFormat::extension(self) -> &'static str; // "jpg", "png", "heic", …
AnimationFormat::still(self) -> ImageFormat;
```

- The leading bytes decide; the extension only breaks a tie the bytes leave open (a HEIF whose
  brands say neither HEIC nor AVIF, Matroska without its DocType, an MP4-branded `.mov`).
- A GIF, WebP or PNG is an `Animation` only with a second frame: `sniff_file` reads past the
  first 4 KB only until it has seen one or ruled one out. Bytes cut before a second frame sniff
  as a still.
- `None`: anything else, camera raw (TIFF underneath: CR2, NEF, ARW, DNG, …), AVI, audio-only
  MP4, JPEG 2000.

### Decoding

```rust
pub fn decode<'a>(source: impl Into<Source<'a>>, picture: Picture, bounds: (u32, u32),
                  animation_budget: usize) -> Result<Decoded, DecodeError>;
pub fn thumbnail<'a>(source: impl Into<Source<'a>>, picture: Picture, max_side: u32)
                  -> Result<Thumbnail, DecodeError>;

pub enum Source<'a> { File(&'a Path), Bytes(&'a [u8]) }   // From &Path, &PathBuf, &[u8], &Vec<u8>
pub enum SourceBuf { File(PathBuf), Bytes(Arc<[u8]>) }     // From PathBuf, Vec<u8>, Arc<[u8]>

pub enum Decoded { Still(Bgra), Animation(Vec<AnimFrame>) }
pub struct AnimFrame { pub image: Bgra, pub delay_ms: u32 }
pub struct Thumbnail { pub image: Bgra, pub source_size: (u32, u32), pub duration_ms: Option<u64> }

Bgra::{new, width, height, pixels, into_pixels, into_rgba_image, byte_size};
pub fn scale_down_to_fit(size: (u32, u32), bounds: (u32, u32)) -> (u32, u32);
pub fn resize_to_fit(image: DynamicImage, bounds: (u32, u32)) -> DynamicImage;
```

- **Output:** `Bgra`, straight-alpha BGRA8 in sRGB, upright, contain-fit inside `bounds` and
  never enlarged: what gpui's `RenderImage` holds. `into_pixels` wraps uncopied in an
  `image::RgbaImage` for a `RenderImage`; `into_rgba_image` swaps to real RGBA for the `image`
  crate's crops, resizes and encoders. Pass `(u32::MAX, u32::MAX)` for the full size.
- **`thumbnail`** fits inside a `max_side` square and also reports the picture's upright size
  and an animation's length (the sum of its delays); an animation gives its first frame.
- **`animation_budget`:** an animation's frames together stay within this many bytes; past it,
  every frame is decoded smaller, halving until they fit. A still ignores it.
- **`resize_to_fit`** is the fallback's own resize (a box filter to within 2×, then a triangle
  filter), for an app's own copies.

### Cut-off files

A file still being copied, or damaged, is `DecodeError::Truncated`, never a picture with its
missing part grey, black or transparent. The container is walked before decoding: JPEG to its EOI,
PNG to `IEND`, GIF to its trailer, BMP and WebP to their declared size, HEIF/AVIF's boxes and
`iloc` extents, TIFF's strips or tiles. A fully transparent result from a format, or a file, that
can't be transparent is refused too.

### Animations

GIF, WebP and APNG are decoded frame by frame by the `image` crate, each frame composited onto
the full canvas (a transparent one, as gpui does), resized as it comes. Delays are whole
milliseconds that add up as the file's do. A cut-off animation keeps its whole frames (one alone
is a still); one that turns out to have a single frame is decoded as a still.

### Missing codecs

```rust
pub fn probe_codecs() -> Codecs;          // once per process; later calls return the first answer
pub struct Codecs { pub heic: bool, pub avif: bool }
pub enum Codec { Heic, Avif }             // Codec::store_extensions()
pub enum DecodeError { Io { path, source }, MissingCodec(Codec), Truncated, Decode(String), Closed }
```

`probe_codecs` decodes a 64 px embedded HEIC and AVIF (`assets/`) on a thread of its own. Call it
once at startup on a background thread: it warms the decoders (the first OS HEIC decode costs
50–200 ms more) and, on Windows, finds the missing Store codecs. A HEIC or AVIF that WIC has no
decoder for, or that fails while the probe failed for its codec, is `MissingCodec`, whose message
names the Store extensions: HEIC needs **HEIF Image Extensions and HEVC Video Extensions**, AVIF
**HEIF Image Extensions and AV1 Video Extension**.

### The decode pool

```rust
impl Decoder {
    pub const WORKERS: usize = 4;
    pub fn new(workers: usize) -> io::Result<Decoder>;
    pub fn decode(&self, source: impl Into<SourceBuf>, picture: Picture, bounds: (u32, u32),
                  animation_budget: usize, priority: Priority)
        -> impl Future<Output = Result<Decoded, DecodeError>> + Send + 'static;
    pub fn thumbnail(&self, source: impl Into<SourceBuf>, picture: Picture, max_side: u32)
        -> impl Future<Output = Result<Thumbnail, DecodeError>> + Send + 'static;
}
pub type Priority = u64;
```

- **An app decodes through one `Decoder`, never on its executor's threads.** On Windows, the
  first decode on a thread joins it to COM's multithreaded apartment for good, for WIC. gpui's
  Windows background executor runs on the system thread pool (`TrySubmitThreadpoolCallback`),
  whose threads must not be left that way. The pool's threads are its own: each joins the
  apartment once, at its start. The free `decode` and `thumbnail` are for tests, tools and
  threads an app owns.
- Two lanes: `decode` requests (urgent: a slideshow's next stills) run before any queued
  `thumbnail`, lowest `Priority` first; thumbnails in the order asked. A decode already running
  finishes first. Count `Priority` so it doesn't change as the app moves on (slide: the place in
  play order), so requests queued at different times compare.
- Dropping a future before its turn skips the decode. Dropping the pool never waits: queued
  requests resolve to `Closed`, running decodes finish on their threads. A decoder that panics
  on an untrusted file is a `Decode` error; the thread survives.
- The futures need no particular executor. A thread an app owns can wait on one with
  `futures::executor::block_on`.
- `WORKERS = 4`: slide's size, with which a still decodes well within a 0.1 s hold.

### The thumbnail cache

```rust
impl ThumbKey { pub fn new(path: &Path, size_bytes: u64, mtime_ns: i64) -> ThumbKey; pub fn as_str(&self) -> &str; }
impl ThumbCache {
    pub fn new(dir: PathBuf) -> ThumbCache;
    pub fn file(&self, key: &ThumbKey, side: u32) -> PathBuf;
    pub fn load(&self, key: &ThumbKey, side: u32) -> Option<Bgra>;
    pub fn store(&self, key: &ThumbKey, side: u32, image: &Bgra) -> io::Result<Bgra>;
    pub fn usage(&self) -> io::Result<u64>;
    pub fn clear(&self) -> io::Result<Pruned>;
    pub fn prune(&self, budget: u64) -> io::Result<Pruned>;
}
pub struct Pruned { pub removed: usize, pub kept_bytes: u64 }
```

- **Key:** hex SHA-256 of the path's OS bytes (UTF-16LE on Windows), then `|<size>|<mtime_ns>`.
  A file that changes gets a new key; its old thumbnail ages out.
- **Files:** `<dir>/<hash[0..2]>/<hash>-<side>.jpg`, JPEG quality 85, one per side asked for,
  written through a temporary file and a rename. JPEG has no alpha: transparency is flattened
  onto black. `store` returns the pixels `load` will read back, so a picture looks the same
  whether it was just made or read from the cache.
- **Pruning:** least recently used first, by modification time, which a hit moves forward (at
  most once an hour), down to 90 % of the budget. Stale temporary files go too. A file that
  vanishes meanwhile is passed over; only an unreadable cache folder fails.
- The sides and the budget are the app's (slide: 320 and 160 px, 2 GiB).

### Long paths

WIC opens a file by name, which fails from `MAX_PATH` (260 UTF-16 units) unless the system's
long-path setting is on and the manifest says `longPathAware`. The crate hands WIC such a path
in its `\\?\` form (`\\?\UNC\` for a share), so any length decodes.

## Tests

`cargo test -p toolbox_stills` runs slide's tests with the crate's own fixtures
(`tests/fixtures/`, about 400 KB; their README says where they come from). The OS-specific tests
are `cfg`'d: TIFF decoding on macOS and Windows, the `MAX_PATH` tests on Windows. HEIC and AVIF
decodes run where `probe_codecs` finds the codec and print a skip where it doesn't. They always
run on macOS. GitHub's Windows runners have no Store codecs, so there CI checks WIC's JPEG, PNG,
GIF, BMP, WebP and TIFF decodes, bytes in memory and long paths, and only the "missing codec"
side of HEIC and AVIF. WIC's HEIC decode (its `colr` profile included) is checked on a Windows PC
with the codecs installed.

## Dependencies and licences

`async-channel`, `image` (BMP, GIF, JPEG, PNG and WebP only), `log`, `sha2`, `thiserror`; on
macOS `objc2-core-foundation`, `objc2-core-graphics` and `objc2-image-io`; on Windows `windows`.
Every package in its tree was already in toolbox-rs's lock file except `objc2-image-io` (Zlib OR
Apache-2.0 OR MIT, as the other objc2 crates), so it adds no licence the apps' notices don't
already list. The `image` crate is part of the API (`resize_to_fit`, `Bgra::into_rgba_image`):
an app on `image` 0.25 shares the crate's copy.

## Adopting it

Both apps take it at the same tag as the automation crates:

```toml
# Cargo.toml, [workspace.dependencies]
toolbox_stills = { git = "https://github.com/kylekz/toolbox-rs", tag = "v0.3.0" }
```

and add `toolbox_stills=info` to their logging's default filter (`crates/slide/src/logging.rs`,
`crates/fc/src/logging.rs`), since the decoders' warnings are logged under that target.

### slide

The crate is `slide_media` without `scan.rs`, so slide changes names, not behaviour: the same
pixels, the same cache files (same keys and layout, so existing thumbnails stay valid), the same
pool.

**What moves out of `slide_media`:** `decode.rs`, `decode/` (`animation.rs`, `container.rs`,
`imageio.rs`, `wic.rs`, `tests.rs`), `decoder.rs`, `heif.rs`, `probe.rs`, `sniff.rs`,
`thumb_cache.rs`, `thumb_cache/tests.rs` and `assets/probe.{heic,avif}`. Delete them.

**What stays:** `slide_media` keeps `scan.rs` alone, still gpui-free (it stays in
`script/check`'s list):
- `src/lib.rs`: `mod scan;` and `pub use scan::{MAX_ITEMS, Scan, ScanError, ScannedFile, scan};`,
  with the crate doc cut to the scan.
- `src/scan.rs`: `use crate::sniff::{Media, sniff_file};` becomes
  `use toolbox_stills::{Media, sniff_file};`, and its tests' `crate::sniff::{…}` becomes
  `toolbox_stills::{…}`. Its fixtures stay in `tests/fixtures/media`.
- `Cargo.toml`: dependencies `log`, `thiserror`, `toolbox_stills`; dev-dependencies
  `pretty_assertions`, `tempfile`, and `libc` on Unix (the FIFO test). Drop the rest.

**Root `Cargo.toml`:** add `toolbox_stills` (above). Remove `objc2-core-foundation`,
`objc2-core-graphics`, `objc2-image-io`, `windows` and `sha2`, which only `slide_media` used.
Reword the comments that name `slide_media` for `async-channel`, `image`, `libc`, `pollster` and
`thiserror`.

**`crates/slide_ui`:** add `toolbox_stills.workspace = true` to its `Cargo.toml`, then:

| File | Change |
|---|---|
| `src/services/mod.rs` | `use slide_media::Decoder;` → `use toolbox_stills::Decoder;`; `slide_media::probe_codecs()` → `toolbox_stills::probe_codecs()`; the module doc's `slide_media::Decoder` |
| `src/services/thumbs.rs` | `use slide_media::{Bgra, DecodeError, Decoder, Media, Pruned, ThumbCache, ThumbKey};` → `toolbox_stills::`. `ThumbCache::GRID_SIDE` and `ThumbCache::DEFAULT_BUDGET` are slide's own values now: define `const SIDE: u32 = 320;` (the grid's) and `const DEFAULT_DISK_BUDGET: u64 = 2 * 1024 * 1024 * 1024;` here, and point `slide_core::app_settings`'s comment at the latter. The module doc's `slide_media::` names |
| `src/services/thumbs/tests.rs` | `use slide_media::Media;`, `slide_media::sniff_file`, `slide_media::Picture` → `toolbox_stills::` |
| `src/services/playback/mod.rs` | `use slide_media::{DecodeError, Decoded, Decoder};` → `toolbox_stills::` |
| `src/services/playback/stills.rs` | `use slide_media::{Bgra, DecodeError, Decoded, Decoder, Picture, Priority};` and `slide_media::ImageFormat::Jpeg` → `toolbox_stills::`; the module doc |
| `src/services/playback/plan.rs` | `use slide_media::{Media, Priority};` → `toolbox_stills::` |
| `src/services/settings.rs` | the test's `use slide_media::Decoder;` → `toolbox_stills::Decoder` |
| `src/services/intake.rs` | `use slide_media::{AnimationFormat, ImageFormat, MAX_ITEMS, Media, MediaKind, Picture, Scan, ScanError, ScannedFile, VideoFormat};` splits: `MAX_ITEMS`, `Scan`, `ScanError`, `ScannedFile` stay `slide_media::`, the rest are `toolbox_stills::`. `slide_media::scan` stays |
| `src/empty_state.rs` | `slide_media::MAX_ITEMS` stays |

Nothing else in `slide_ui` changes: `ThumbCache::SMALL_SIDE` had no user outside the cache's
tests, and `Decoder::WORKERS`, `Decoder::new`, `decode` and `thumbnail` (which take a `PathBuf` as
before) keep their names and shapes.

**Elsewhere in slide:**
- `crates/slide/src/logging.rs`: add `toolbox_stills=info`.
- `crates/slide/src/automation.rs:506`, `crates/slide_player/src/prober.rs:2` and
  `crates/slide_core/src/model.rs:94-97`: comments that name `slide_media`'s sniffing or decoder
  now name `toolbox_stills`.
- `script/make-fixtures`: the probe samples now live in toolbox-rs
  (`crates/toolbox_stills/assets/`); stop writing `crates/slide_media/assets/`, or keep it to
  remake them and copy them over. The fixtures toolbox-rs copied are listed in its
  `crates/toolbox_stills/tests/fixtures/README.md`.
- `CLAUDE.md`'s crate table and `docs/ARCHITECTURE.md` (D4, §3, §5.2, §5.3, §6.6, §8.5, §9 "Long
  paths", and §11's folio row): `slide_media` is the scan; decoding, the pool, the probe and the
  cache are `toolbox_stills`, documented here.
- `script/third-party-notices`: toolbox_stills is MIT and carries its `LICENSE`.

**What slide gains:** one copy instead of its own, shared with fc, and checked by CI on Linux,
Windows and macOS. Its decoding is unchanged; only the pool's thread names differ
(`stills-decode-N`, was `slide-decode-N`).

### fc

fc keeps `fc_media`, its YouTube thumbnail chain (`thumbs.rs`), its covers (`covers.rs`) and its
display and turned copies (`raster.rs`'s `display_copy`, `turned_copy`, `DISPLAY_SIZE`). Its
sniffing and decoding move to toolbox_stills.

**What fc gains:**
- **HEIC and AVIF** covers, dropped or picked or pasted as a URL, and TIFF, BMP and GIF ones
  (macOS always; Windows with the Store codecs, else "Missing codec" copy).
- **Colour management:** a Display P3 iPhone photo or an Adobe RGB file becomes sRGB instead of
  looking washed out; the `image` crate ignores ICC profiles.
- **Orientation from HEIF** (`irot`/`imir`); EXIF orientation, as before.
- **Cut-off downloads** are `Truncated` instead of a half-grey thumbnail; the fetch chain moves
  on to the next source.
- Faster JPEG decodes of YouTube thumbnails and ffmpeg frames.

**`crates/fc_media`:**
- `Cargo.toml`: add `toolbox_stills.workspace = true`. `image` stays (the display copies,
  covers and their PNG/JPEG encoders), as do `sha2`, `futures` and `http_client`.
- `src/sniff.rs`: delete it. `sniff(bytes)` becomes `toolbox_stills::sniff_bytes(bytes, None)`
  with `Media::picture`, and `ImageFormat::extension()` comes with toolbox_stills' `ImageFormat`
  (`Picture::still_format()` for an animation's).
- `src/raster.rs`: `decode` becomes a front for one pool of fc_media's own, so that no decode
  runs on gpui's threads (see "The decode pool"):

  ```rust
  fn decoder() -> &'static Decoder {
      static DECODER: OnceLock<Decoder> = OnceLock::new();
      DECODER.get_or_init(|| Decoder::new(2).expect("fc_media starts its decode threads"))
  }

  /** Decodes a picture (by its bytes, not its name) upright, in sRGB, at full size. */
  pub fn decode(bytes: &[u8]) -> Result<DynamicImage, MediaError> {
      let picture = toolbox_stills::sniff_bytes(bytes, None)
          .and_then(Media::picture)
          .ok_or(MediaError::UnknownFormat)?;
      let thumbnail = futures::executor::block_on(decoder().thumbnail(
          bytes.to_vec(),
          picture,
          u32::MAX,
      ))?;
      Ok(DynamicImage::ImageRgba8(thumbnail.image.into_rgba_image()))
  }
  ```

  It blocks on the pool as today's `decode` blocks on the CPU, so every caller stays as it is
  (`fc_ui`'s `services/thumbs/mod.rs` grab, fetch, `display_copy_of`; `services/library/edits.rs`
  `load_cover`, `set_cover`; `editors/cover.rs`'s `render_square`; `covers.rs`'s `fetch_image`;
  `thumbs.rs`'s `fetch`). `Decoder::new` fails only when the OS refuses a thread, so the
  `expect` states that invariant. An animated GIF or WebP gives its first frame, as today.
  `turned_copy` decodes fc's own display copy, so it can keep `image::load_from_memory`.
  `scale_down_to_fit` and `resize_to_fit` are toolbox_stills' (`resize_to_fit` takes the image by
  value: `resize_to_fit(cropped, DISPLAY_SIZE)`); delete fc's copies and their test.
- `src/lib.rs`: re-export `toolbox_stills::{ImageFormat, sniff_bytes}` where `fc_ui` used
  `fc_media::{ImageFormat, sniff}`, or have `fc_ui` name toolbox_stills itself. `MediaError`
  gains `Decode(#[from] toolbox_stills::DecodeError)` beside `Image`, and `UnknownFormat`'s
  message becomes "not a picture fc can read".
- `src/thumbs.rs`: `fetch`'s `(sniff(&bytes), decode(&bytes))` sniffs with
  `sniff_bytes(&bytes, None).and_then(Media::picture).map(Picture::still_format)`.
  `Fetched::format` is toolbox_stills' `ImageFormat`; `format.extension()` is unchanged for JPEG
  and WebP. Its tests keep `tests/fixtures/thumbs`.
- `raster.rs`'s `decodes_the_fixture_thumbnails`: a JPEG cut at 200 bytes is now
  `MediaError::Decode(DecodeError::Truncated)`, not `MediaError::Image`.

**`crates/fc_ui`:**
- `src/editors/cover.rs`, `Picked::new`: today it draws the picked bytes with
  `Image::from_bytes`, which gpui can't do for HEIC or AVIF and which shows a JPEG unrotated and
  without colour management. Draw the decoded picture instead: have `LibraryService::load_cover`
  (`services/library/edits.rs`) return the decoded `Bgra` (or `RgbaImage`) beside the bytes, and
  build an `Arc<RenderImage>` from it as slide does (`render_image` in slide's
  `crates/slide_ui/src/services/thumbs.rs`: the BGRA `into_pixels()` in an `RgbaImage`, one
  `image::Frame`), drawn through `img(ImageSource::Render(…))`. `render_square` and Save keep
  working on the bytes.
- `src/editors/cover.rs`, `dropped_image`, and the cover file picker (`system.rs`'s
  `pick_files("Choose a cover", …)`): accept `heic`, `heif`, `avif`, `tif`, `tiff`, `gif` and
  `bmp` besides `jpg`, `jpeg`, `png` and `webp`. Sniffing still decides what a file is.
- `src/editors/cover.rs`'s error copy: a `MissingCodec` error reads its own message (which names
  the Store extensions) rather than "That file isn't an image."
- Optionally, call `toolbox_stills::probe_codecs()` once at startup on the background executor
  (it runs on a thread of its own), so the first HEIC cover doesn't pay the decoder's warm-up.

**Elsewhere in fc:** `crates/fc/src/logging.rs` adds `toolbox_stills=info`; `docs/PLAN.md` §3.2
and `CLAUDE.md` say where sniffing and decoding live. fc's `ThumbCache` (`fc_ui`'s LRU of gpui
images) is unrelated to toolbox_stills' disk cache and stays; fc's thumbnails are keyed by video
id, not by path, so it has no use for `ThumbCache` or `ThumbKey`.

**Check after the move:** fc's thumbnail and cover tests compare colours within tolerances;
ImageIO's and WIC's JPEG decoders differ from `zune-jpeg` by a level or two, so a tolerance that
was exact may need a little room.
