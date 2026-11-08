//! Still images for the apps, gpui-free (docs/stills.md).
//!
//! - [`sniff_file`] and [`sniff_bytes`] tell a file's format from its leading bytes: a still, an
//!   animation (GIF, WebP, APNG with more than one frame) or a video.
//! - [`decode`] and [`thumbnail`] turn a still or an animation, from a file or from bytes in
//!   memory, into upright, straight-alpha BGRA8 in sRGB, fitted inside the caller's bounds:
//!   through ImageIO on macOS and WIC on Windows, the `image` crate elsewhere.
//! - [`Decoder`] runs the same decodes on a pool of its own threads, which on Windows are the
//!   only ones joined to COM's multithreaded apartment.
//! - [`ThumbCache`] keeps thumbnails as JPEGs in a folder, keyed by path, size and modification
//!   time, pruned to a byte budget.
//! - [`probe_codecs`] warms the HEIC and AVIF decoders once and finds missing Windows codecs.
//!
//! Every function here but the [`Decoder`]'s blocks: call them off the UI thread.

mod decode;
mod decoder;
mod heif;
mod probe;
mod sniff;
mod thumb_cache;

pub use decode::{
    AnimFrame, Bgra, Codec, DecodeError, Decoded, Source, SourceBuf, Thumbnail, decode,
    resize_to_fit, scale_down_to_fit, thumbnail,
};
pub use decoder::{Decoder, Priority};
pub use probe::{Codecs, probe_codecs};
pub use sniff::{
    AnimationFormat, ImageFormat, Media, MediaKind, Picture, VideoFormat, sniff_bytes, sniff_file,
};
pub use thumb_cache::{Pruned, ThumbCache, ThumbKey};
