//! Decoding stills and animations for painting (docs/stills.md "Decoding"). The output is what
//! GPUI's `RenderImage` holds: straight-alpha BGRA8 in sRGB, upright, contain-fit inside the
//! caller's bounds and never enlarged.
//!
//! - **Stills** go through the OS decoder: ImageIO on macOS (`imageio.rs`), WIC on Windows
//!   (`wic.rs`). Both decode at reduced scale, apply EXIF/HEIF orientation and colour-manage to
//!   sRGB. When the OS returns no image, the `image` crate decodes JPEG, PNG, GIF, BMP and WebP
//!   instead, without colour management; TIFF, HEIC and AVIF have no fallback. Other systems
//!   have no OS decoder here (`none.rs`): the `image` crate is all they get.
//! - **Cut-off files** (still copying, or damaged) are [`DecodeError::Truncated`], never a
//!   picture with its missing part grey, black or transparent: the container is walked first
//!   (`container.rs`), and a fully transparent result from a picture that can't be transparent
//!   is refused too.
//! - **Animations** (GIF, WebP, APNG) are decoded frame by frame by the `image` crate, each
//!   frame composited onto the full canvas, within a byte budget (`animation.rs`). One that
//!   turns out to have a single frame is decoded as a still.
//! - **Missing codecs** (Windows): a HEIC or AVIF that WIC has no decoder for, or that fails
//!   while the startup probe failed for its codec ([`crate::probe_codecs`]), is
//!   [`DecodeError::MissingCodec`].
//!
//! A picture comes from a file or from bytes in memory ([`Source`]). [`decode`] and
//! [`thumbnail`] run on the calling thread; an app runs them on a [`crate::Decoder`]'s threads.

mod animation;
mod container;

use std::fmt;
use std::fs::File;
use std::io::{self, BufRead, BufReader, Cursor, Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};
use std::sync::Arc;

use image::imageops::FilterType;
use image::metadata::Orientation;
use image::{DynamicImage, ImageDecoder as _, ImageReader};

use crate::probe::probe_codecs;
use crate::sniff::{ImageFormat, Picture};
use container::Container;

#[cfg(target_os = "macos")]
pub(crate) mod imageio;
#[cfg(target_os = "macos")]
pub(crate) use imageio as os;
#[cfg(windows)]
pub(crate) mod wic;
#[cfg(windows)]
pub(crate) use wic as os;
#[cfg(not(any(target_os = "macos", windows)))]
pub(crate) mod none;
#[cfg(not(any(target_os = "macos", windows)))]
pub(crate) use none as os;

/// Upright BGRA8 in sRGB with straight (not premultiplied) alpha; the stride is `width * 4`.
#[derive(Clone, PartialEq, Eq)]
pub struct Bgra {
    width: u32,
    height: u32,
    /// A `Vec`, so an app wraps it in an `image::RgbaImage` for its `RenderImage` uncopied.
    pixels: Vec<u8>,
}

impl Bgra {
    /// `None` unless `pixels` holds exactly `width * height` pixels of 4 bytes.
    pub fn new(width: u32, height: u32, pixels: Vec<u8>) -> Option<Self> {
        let len = (width as usize)
            .checked_mul(height as usize)?
            .checked_mul(4)?;
        (pixels.len() == len).then_some(Self {
            width,
            height,
            pixels,
        })
    }

    pub fn width(&self) -> u32 {
        self.width
    }

    pub fn height(&self) -> u32 {
        self.height
    }

    pub fn pixels(&self) -> &[u8] {
        &self.pixels
    }

    pub fn into_pixels(self) -> Vec<u8> {
        self.pixels
    }

    /**
     * The same picture as RGBA8, for the `image` crate's crops, resizes and encoders. GPUI
     * draws BGRA: an app that paints wraps [`Bgra::into_pixels`] instead.
     */
    pub fn into_rgba_image(self) -> image::RgbaImage {
        let mut pixels = self.pixels;
        for px in pixels.as_chunks_mut::<4>().0 {
            px.swap(0, 2);
        }
        image::RgbaImage::from_raw(self.width, self.height, pixels)
            .expect("a Bgra holds 4 bytes per pixel")
    }

    /// The memory its pixels take.
    pub fn byte_size(&self) -> usize {
        self.pixels.len()
    }

    fn is_fully_transparent(&self) -> bool {
        self.pixels.as_chunks::<4>().0.iter().all(|px| px[3] == 0)
    }
}

impl fmt::Debug for Bgra {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Bgra")
            .field("width", &self.width)
            .field("height", &self.height)
            .field("bytes", &self.pixels.len())
            .finish()
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AnimFrame {
    pub image: Bgra,
    /**
     * How long the frame shows, as the file says, in whole milliseconds that add up to the
     * file's total; 0 when it says nothing.
     */
    pub delay_ms: u32,
}

/// A still the OS decoder fitted, and the upright size it was fitted from.
pub(crate) struct Fitted {
    pub(crate) image: Bgra,
    pub(crate) source: (u32, u32),
}

/**
 * What a thumbnail decode finds: the picture fitted, its upright size, and an animation's
 * length (the sum of its frames' delays; `None` for a still).
 */
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Thumbnail {
    pub image: Bgra,
    pub source_size: (u32, u32),
    pub duration_ms: Option<u64>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Decoded {
    Still(Bgra),
    /// At least two frames, each the full canvas at the same size.
    Animation(Vec<AnimFrame>),
}

impl Decoded {
    /// The memory its pixels take, every frame's together.
    pub fn byte_size(&self) -> usize {
        match self {
            Self::Still(image) => image.byte_size(),
            Self::Animation(frames) => frames.iter().map(|frame| frame.image.byte_size()).sum(),
        }
    }
}

/// A codec Windows installs from the Microsoft Store (docs/stills.md "Missing codecs").
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Codec {
    Heic,
    Avif,
}

impl Codec {
    /// The Store extensions that provide it, named in [`DecodeError::MissingCodec`]'s message.
    pub fn store_extensions(self) -> &'static str {
        match self {
            Self::Heic => "HEIF Image Extensions and HEVC Video Extensions",
            Self::Avif => "HEIF Image Extensions and AV1 Video Extension",
        }
    }

    fn of(format: ImageFormat) -> Option<Self> {
        match format {
            ImageFormat::Heif => Some(Self::Heic),
            ImageFormat::Avif => Some(Self::Avif),
            _ => None,
        }
    }

    /// What [`DecodeError::MissingCodec`] says: on Windows, which Store extensions to install.
    fn missing_message(self) -> String {
        if cfg!(windows) {
            format!(
                "{self} images need a codec that isn't installed: {} from the Microsoft Store",
                self.store_extensions()
            )
        } else {
            format!("{self} images need a codec this system doesn't have")
        }
    }
}

impl fmt::Display for Codec {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::Heic => "HEIC",
            Self::Avif => "AVIF",
        })
    }
}

#[derive(Debug, thiserror::Error)]
pub enum DecodeError {
    /// The file is missing or unreadable.
    #[error("reading {}: {source}", path.display())]
    Io {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error("{}", .0.missing_message())]
    MissingCodec(Codec),
    /// The file ends before its picture does: still being copied, or damaged.
    #[error("the file ends before its picture does")]
    Truncated,
    #[error("{0}")]
    Decode(String),
    /// The [`crate::Decoder`] was dropped before it got to this request.
    #[error("the decoder has shut down")]
    Closed,
}

/// Where a picture is read from: a file, or its bytes in memory.
#[derive(Clone, Copy, Debug)]
pub enum Source<'a> {
    File(&'a Path),
    Bytes(&'a [u8]),
}

impl<'a> From<&'a Path> for Source<'a> {
    fn from(path: &'a Path) -> Self {
        Self::File(path)
    }
}

impl<'a> From<&'a PathBuf> for Source<'a> {
    fn from(path: &'a PathBuf) -> Self {
        Self::File(path)
    }
}

impl<'a> From<&'a [u8]> for Source<'a> {
    fn from(bytes: &'a [u8]) -> Self {
        Self::Bytes(bytes)
    }
}

impl<'a> From<&'a Vec<u8>> for Source<'a> {
    fn from(bytes: &'a Vec<u8>) -> Self {
        Self::Bytes(bytes)
    }
}

impl fmt::Display for Source<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::File(path) => path.display().fmt(f),
            Self::Bytes(bytes) => write!(f, "{} bytes in memory", bytes.len()),
        }
    }
}

/// A [`Source`] that owns its path or bytes, for a [`crate::Decoder`] request.
#[derive(Clone, Debug)]
pub enum SourceBuf {
    File(PathBuf),
    Bytes(Arc<[u8]>),
}

impl SourceBuf {
    pub fn as_source(&self) -> Source<'_> {
        match self {
            Self::File(path) => Source::File(path),
            Self::Bytes(bytes) => Source::Bytes(bytes),
        }
    }
}

impl From<PathBuf> for SourceBuf {
    fn from(path: PathBuf) -> Self {
        Self::File(path)
    }
}

impl From<Arc<[u8]>> for SourceBuf {
    fn from(bytes: Arc<[u8]>) -> Self {
        Self::Bytes(bytes)
    }
}

impl From<Vec<u8>> for SourceBuf {
    fn from(bytes: Vec<u8>) -> Self {
        Self::Bytes(bytes.into())
    }
}

/// A [`Source`] opened for reading: a buffered file, or a cursor over the bytes.
pub(crate) enum Reader<'a> {
    File(BufReader<File>),
    Bytes(Cursor<&'a [u8]>),
}

impl<'a> Reader<'a> {
    /**
     * Opens `source`. The OS decoders can't tell a missing or unreadable file from a bad one,
     * so the container walk's open is where a file's absence shows. Large reads keep the walks,
     * which read most files through, to a few system calls.
     */
    pub(crate) fn open(source: Source<'a>) -> io::Result<Self> {
        Ok(match source {
            Source::File(path) => Self::File(BufReader::with_capacity(1 << 16, File::open(path)?)),
            Source::Bytes(bytes) => Self::Bytes(Cursor::new(bytes)),
        })
    }
}

impl Read for Reader<'_> {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        match self {
            Self::File(file) => file.read(buf),
            Self::Bytes(bytes) => bytes.read(buf),
        }
    }
}

impl BufRead for Reader<'_> {
    fn fill_buf(&mut self) -> io::Result<&[u8]> {
        match self {
            Self::File(file) => file.fill_buf(),
            Self::Bytes(bytes) => bytes.fill_buf(),
        }
    }

    fn consume(&mut self, amount: usize) {
        match self {
            Self::File(file) => file.consume(amount),
            Self::Bytes(bytes) => bytes.consume(amount),
        }
    }
}

impl Seek for Reader<'_> {
    fn seek(&mut self, position: SeekFrom) -> io::Result<u64> {
        match self {
            Self::File(file) => file.seek(position),
            Self::Bytes(bytes) => bytes.seek(position),
        }
    }
}

/// Why the OS decoder returned no image.
#[derive(Debug)]
pub(crate) enum OsError {
    #[cfg(any(target_os = "macos", windows))]
    Failed(String),
    /// WIC has no decoder for the format: `WINCODEC_ERR_COMPONENTNOTFOUND` at any step.
    #[cfg(windows)]
    NoCodec,
    /// This system has no OS decoder here: the `image` crate decodes what it can.
    #[cfg(not(any(target_os = "macos", windows)))]
    Unavailable,
}

impl fmt::Display for OsError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            #[cfg(any(target_os = "macos", windows))]
            Self::Failed(message) => f.write_str(message),
            #[cfg(windows)]
            Self::NoCodec => f.write_str("WIC has no decoder for this format"),
            #[cfg(not(any(target_os = "macos", windows)))]
            Self::Unavailable => f.write_str("this system has no OS image decoder"),
        }
    }
}

/**
 * Decodes the picture in `source` (sniffed as `picture`) to fit inside `bounds`, in device
 * pixels, on this thread: blocking, for tens of milliseconds per still. On Windows the first
 * decode on a thread joins it to COM's multithreaded apartment for good; an app decodes through
 * a [`crate::Decoder`] instead, whose threads are its own.
 *
 * An animation's frames together take at most `animation_budget` bytes: past it, all of them
 * are decoded smaller, halving until they fit. A still ignores it.
 */
pub fn decode<'a>(
    source: impl Into<Source<'a>>,
    picture: Picture,
    bounds: (u32, u32),
    animation_budget: usize,
) -> Result<Decoded, DecodeError> {
    let source = source.into();
    match picture {
        Picture::Image(format) => {
            decode_image(source, format, bounds).map(|fitted| Decoded::Still(fitted.image))
        }
        Picture::Animation(format) => animation::decode(source, format, bounds, animation_budget),
    }
}

/**
 * The picture in `source` fitted inside `max_side` square, with its upright size, and for an
 * animation its first frame and length. Blocking, like [`decode`]; an app runs it through
 * [`crate::Decoder::thumbnail`].
 */
pub fn thumbnail<'a>(
    source: impl Into<Source<'a>>,
    picture: Picture,
    max_side: u32,
) -> Result<Thumbnail, DecodeError> {
    let (source, bounds) = (source.into(), (max_side, max_side));
    match picture {
        Picture::Image(format) => decode_image(source, format, bounds).map(|fitted| Thumbnail {
            image: fitted.image,
            source_size: fitted.source,
            duration_ms: None,
        }),
        Picture::Animation(format) => animation::thumbnail(source, format, bounds),
    }
}

/// Contain-fit inside `bounds`, never enlarged, each side ≥ 1.
pub fn scale_down_to_fit(size: (u32, u32), bounds: (u32, u32)) -> (u32, u32) {
    if size.0 <= bounds.0 && size.1 <= bounds.1 {
        return (size.0.max(1), size.1.max(1));
    }
    let (width, height) = (f64::from(size.0.max(1)), f64::from(size.1.max(1)));
    let scale = (f64::from(bounds.0.max(1)) / width).min(f64::from(bounds.1.max(1)) / height);
    let side = |value: f64| ((value * scale).round() as u32).max(1);
    (side(width), side(height))
}

/**
 * `image` scaled down to fit inside `bounds` ([`scale_down_to_fit`]), never up: a box filter
 * to within 2× for large ratios, then a triangle filter, which a box filter alone would alias.
 */
pub fn resize_to_fit(image: DynamicImage, bounds: (u32, u32)) -> DynamicImage {
    let target = scale_down_to_fit((image.width(), image.height()), bounds);
    resize(image, target)
}

fn decode_image(
    source: Source<'_>,
    format: ImageFormat,
    bounds: (u32, u32),
) -> Result<Fitted, DecodeError> {
    let walked =
        Reader::open(source).and_then(|mut reader| container::inspect(&mut reader, format));
    let alpha = match walked.map_err(|error| read_error(source, error))? {
        Container::Truncated => return Err(DecodeError::Truncated),
        Container::Complete { alpha } => alpha,
    };
    let fitted = match os::decode(source, format, bounds) {
        Ok(fitted) => fitted,
        Err(os_error) => match crate_format(format) {
            Some(fallback) => {
                /* Without an OS decoder, the `image` crate is the decoder, not a fallback. */
                let quiet = cfg!(not(any(target_os = "macos", windows)));
                if !quiet {
                    log::warn!("{source}: {os_error}; trying the image crate");
                }
                decode_with_crate(source, fallback, bounds).map_err(|error| {
                    DecodeError::Decode(if quiet {
                        format!("the image crate: {error}")
                    } else {
                        format!("{os_error}; the image crate: {error}")
                    })
                })?
            }
            None => return Err(os_failure(format, os_error)),
        },
    };
    /* A decoder that ran out of data can leave the rest of an opaque picture transparent. */
    if !alpha && fitted.image.is_fully_transparent() {
        return Err(DecodeError::Truncated);
    }
    Ok(fitted)
}

/// The error for a still the OS decoder failed on and the `image` crate can't read.
fn os_failure(format: ImageFormat, os_error: OsError) -> DecodeError {
    #[cfg(windows)]
    let no_codec = matches!(os_error, OsError::NoCodec);
    #[cfg(not(windows))]
    let no_codec = false;
    match Codec::of(format) {
        Some(codec) if no_codec || !probe_codecs().supports(codec) => {
            DecodeError::MissingCodec(codec)
        }
        _ => DecodeError::Decode(os_error.to_string()),
    }
}

/// The `image` crate's format for a still it can decode as the fallback.
fn crate_format(format: ImageFormat) -> Option<image::ImageFormat> {
    match format {
        ImageFormat::Jpeg => Some(image::ImageFormat::Jpeg),
        ImageFormat::Png => Some(image::ImageFormat::Png),
        ImageFormat::Gif => Some(image::ImageFormat::Gif),
        ImageFormat::Bmp => Some(image::ImageFormat::Bmp),
        ImageFormat::Webp => Some(image::ImageFormat::WebP),
        ImageFormat::Tiff | ImageFormat::Heif | ImageFormat::Avif => None,
    }
}

/**
 * The `image` crate's still: decoded at full size, resized in stored orientation (the bounds'
 * axes swapped when the EXIF orientation swaps them), then the small result oriented.
 */
fn decode_with_crate(
    source: Source<'_>,
    format: image::ImageFormat,
    bounds: (u32, u32),
) -> image::ImageResult<Fitted> {
    let mut decoder = ImageReader::with_format(Reader::open(source)?, format).into_decoder()?;
    let orientation = decoder.orientation()?;
    let image = DynamicImage::from_decoder(decoder)?;
    let (width, height) = (image.width(), image.height());
    let swaps = matches!(
        orientation,
        Orientation::Rotate90
            | Orientation::Rotate270
            | Orientation::Rotate90FlipH
            | Orientation::Rotate270FlipH
    );
    let (stored_bounds, source) = if swaps {
        ((bounds.1, bounds.0), (height, width))
    } else {
        (bounds, (width, height))
    };
    let target = scale_down_to_fit((width, height), stored_bounds);
    let mut image = resize(image, target);
    image.apply_orientation(orientation);
    let image = to_bgra(image);
    Ok(Fitted { image, source })
}

/// `image` resized to exactly `target`: [`resize_to_fit`]'s filters.
fn resize(image: DynamicImage, target: (u32, u32)) -> DynamicImage {
    let (width, height) = (image.width(), image.height());
    if target == (width, height) {
        return image;
    }
    if width >= target.0 * 4 && height >= target.1 * 4 {
        let coarse = image.thumbnail_exact(target.0 * 2, target.1 * 2);
        return coarse.resize_exact(target.0, target.1, FilterType::Triangle);
    }
    image.resize_exact(target.0, target.1, FilterType::Triangle)
}

/// Straight-alpha RGBA8 to BGRA8, as gpui's own `decode_static_image` does.
fn to_bgra(image: DynamicImage) -> Bgra {
    let mut rgba = image.into_rgba8();
    for px in rgba.as_chunks_mut::<4>().0 {
        px.swap(0, 2);
    }
    let (width, height) = rgba.dimensions();
    Bgra::new(width, height, rgba.into_raw()).expect("an RgbaImage holds 4 bytes per pixel")
}

/**
 * An error opening or reading `source`: the file's [`DecodeError::Io`], or for bytes in
 * memory, which only a walk past their end fails to read, a decode error.
 */
fn read_error(source: Source<'_>, error: io::Error) -> DecodeError {
    match source {
        Source::File(path) => DecodeError::Io {
            path: path.to_owned(),
            source: error,
        },
        Source::Bytes(_) => DecodeError::Decode(error.to_string()),
    }
}

#[cfg(test)]
mod tests;
