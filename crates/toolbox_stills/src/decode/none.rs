//! Systems without an OS decoder here (Linux, docs/stills.md "What it does"): every still goes to
//! the `image` crate, without colour management, and TIFF, HEIC and AVIF don't decode.

use super::{Fitted, OsError, Source};
use crate::sniff::ImageFormat;

pub(crate) fn decode(
    _source: Source<'_>,
    _format: ImageFormat,
    _bounds: (u32, u32),
) -> Result<Fitted, OsError> {
    Err(OsError::Unavailable)
}
