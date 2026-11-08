//! Every still through ImageIO (macOS; docs/stills.md "Decoding").
//!
//! `CGImageSourceCreateThumbnailAtIndex` decodes straight to the target size (JPEG with IDCT
//! scaling or the hardware decoder, HEIC by picking its tiles) and applies the EXIF or HEIF
//! orientation. Drawing the result into an sRGB bitmap context colour-matches it from the
//! file's profile. ImageIO is thread-safe, so decodes run on any background thread.
//!
//! The `unsafe` calls are ImageIO's and CoreGraphics' C APIs; the option dictionaries map
//! ImageIO's `CFString` keys to the `CFBoolean`/`CFNumber` values each key documents.

use std::ffi::c_void;

use objc2_core_foundation::{
    CFBoolean, CFData, CFDictionary, CFNumber, CFRetained, CFString, CFType, CFURL, CGPoint,
    CGRect, CGSize,
};
use objc2_core_graphics::{
    CGBitmapContextCreate, CGBlendMode, CGColorSpace, CGContext, CGImage, CGImageAlphaInfo,
    CGImageByteOrderInfo, kCGColorSpaceSRGB,
};
use objc2_image_io::{
    CGImageSource, CGImageSourceStatus, kCGImagePropertyOrientation, kCGImagePropertyPixelHeight,
    kCGImagePropertyPixelWidth, kCGImageSourceCreateThumbnailFromImageAlways,
    kCGImageSourceCreateThumbnailWithTransform, kCGImageSourceShouldCache,
    kCGImageSourceShouldCacheImmediately, kCGImageSourceThumbnailMaxPixelSize,
};

use super::{Bgra, Fitted, OsError, Source, scale_down_to_fit};
use crate::sniff::ImageFormat;

/**
 * ImageIO reads the format from the bytes itself; WIC's twin needs `_format` for HEIF colour.
 * Bytes in memory are copied into a `CFData`: ImageIO may hold its source past this call.
 */
pub(crate) fn decode(
    source: Source<'_>,
    _format: ImageFormat,
    bounds: (u32, u32),
) -> Result<Fitted, OsError> {
    /* The thumbnail is the only copy kept: the source itself caches nothing. */
    let no_cache = CFDictionary::<CFString, CFType>::from_slices(
        &[unsafe { kCGImageSourceShouldCache }],
        &[CFBoolean::new(false)],
    );
    let source = match source {
        Source::File(path) => {
            let url = CFURL::from_file_path(path)
                .ok_or_else(|| OsError::Failed(format!("{} isn't a file URL", path.display())))?;
            unsafe { CGImageSource::with_url(&url, Some(no_cache.as_opaque())) }
        }
        Source::Bytes(bytes) => {
            let data = CFData::from_bytes(bytes);
            unsafe { CGImageSource::with_data(&data, Some(no_cache.as_opaque())) }
        }
    }
    .ok_or_else(|| OsError::Failed("ImageIO can't open it".to_owned()))?;
    if unsafe { source.count() } == 0 {
        return Err(OsError::Failed(format!(
            "ImageIO finds no image in it ({})",
            status(unsafe { source.status() })
        )));
    }
    let index = unsafe { source.primary_image_index() };
    let (width, height) = displayed_size(&source, index)?;
    let (target_w, target_h) = scale_down_to_fit((width, height), bounds);

    let max_edge = CFNumber::new_i64(i64::from(target_w.max(target_h)));
    let yes = CFBoolean::new(true);
    let options = CFDictionary::<CFString, CFType>::from_slices(
        &unsafe {
            [
                /* Never an embedded EXIF thumbnail, which is tiny. */
                kCGImageSourceCreateThumbnailFromImageAlways,
                /* Orientation applied. */
                kCGImageSourceCreateThumbnailWithTransform,
                /* Decoded now, on this thread, not lazily at the first draw. */
                kCGImageSourceShouldCacheImmediately,
                /* ImageIO never enlarges, so this is the contain-fit. */
                kCGImageSourceThumbnailMaxPixelSize,
            ]
        },
        &[yes, yes, yes, &max_edge],
    );
    let image = unsafe { source.thumbnail_at_index(index, Some(options.as_opaque())) }.ok_or_else(
        || {
            OsError::Failed(format!(
                "ImageIO returned no image ({})",
                status(unsafe { source.status_at_index(index) })
            ))
        },
    )?;
    Ok(Fitted {
        image: draw_bgra(&image)?,
        source: (width, height),
    })
}

fn status(status: CGImageSourceStatus) -> &'static str {
    match status {
        CGImageSourceStatus::StatusUnexpectedEOF => "unexpected end of file",
        CGImageSourceStatus::StatusInvalidData => "invalid data",
        CGImageSourceStatus::StatusUnknownType => "unknown type",
        CGImageSourceStatus::StatusReadingHeader => "reading the header",
        CGImageSourceStatus::StatusIncomplete => "incomplete",
        CGImageSourceStatus::StatusComplete => "complete",
        _ => "unknown status",
    }
}

/// The size as displayed: the stored size, its axes swapped for orientations 5–8.
fn displayed_size(source: &CGImageSource, index: usize) -> Result<(u32, u32), OsError> {
    let props = unsafe { source.properties_at_index(index, None) }
        .ok_or_else(|| OsError::Failed("ImageIO reads no properties".to_owned()))?;
    /* ImageIO documents the properties dictionary as `CFString` keys to `CFType` values. */
    let props: CFRetained<CFDictionary<CFString, CFType>> =
        unsafe { CFRetained::cast_unchecked(props) };
    let number =
        |key: &CFString| -> Option<i64> { props.get(key)?.downcast_ref::<CFNumber>()?.as_i64() };
    let side = |key: &CFString| {
        number(key)
            .and_then(|value| u32::try_from(value).ok())
            .ok_or_else(|| OsError::Failed("ImageIO reads no pixel size".to_owned()))
    };
    let width = side(unsafe { kCGImagePropertyPixelWidth })?;
    let height = side(unsafe { kCGImagePropertyPixelHeight })?;
    let orientation = number(unsafe { kCGImagePropertyOrientation }).unwrap_or(1);
    Ok(if (5..=8).contains(&orientation) {
        (height, width)
    } else {
        (width, height)
    })
}

/**
 * Draws `image` 1:1 into a premultiplied BGRA8 bitmap in sRGB (little-endian premultiplied
 * ARGB is the only 8-bit layout with alpha CGBitmapContext supports), then un-premultiplies,
 * since gpui's sprite shader takes straight alpha. Sized from `image`, which can differ from
 * the computed fit by a pixel.
 */
fn draw_bgra(image: &CGImage) -> Result<Bgra, OsError> {
    let (width, height) = (CGImage::width(Some(image)), CGImage::height(Some(image)));
    let mut pixels = vec![0u8; width * height * 4];
    let srgb = CGColorSpace::with_name(Some(unsafe { kCGColorSpaceSRGB }))
        .ok_or_else(|| OsError::Failed("no sRGB colour space".to_owned()))?;
    let context = unsafe {
        CGBitmapContextCreate(
            pixels.as_mut_ptr().cast::<c_void>(),
            width,
            height,
            8,
            width * 4,
            Some(&srgb),
            CGImageAlphaInfo::PremultipliedFirst.0 | CGImageByteOrderInfo::Order32Little.0,
        )
    }
    .ok_or_else(|| OsError::Failed("CGBitmapContextCreate failed".to_owned()))?;
    CGContext::set_blend_mode(Some(&context), CGBlendMode::Copy);
    CGContext::draw_image(
        Some(&context),
        CGRect::new(
            CGPoint::new(0.0, 0.0),
            CGSize::new(width as f64, height as f64),
        ),
        Some(image),
    );
    /* The context writes into `pixels` until it is released. */
    drop(context);
    if has_alpha(CGImage::alpha_info(Some(image))) {
        unpremultiply(&mut pixels);
    }
    let side = |value: usize| {
        u32::try_from(value).map_err(|_| OsError::Failed("an image wider than u32".to_owned()))
    };
    Bgra::new(side(width)?, side(height)?, pixels)
        .ok_or_else(|| OsError::Failed("the bitmap's size doesn't match its pixels".to_owned()))
}

fn has_alpha(info: CGImageAlphaInfo) -> bool {
    !matches!(
        info,
        CGImageAlphaInfo::None | CGImageAlphaInfo::NoneSkipFirst | CGImageAlphaInfo::NoneSkipLast
    )
}

/// Premultiplied to straight alpha, rounding to nearest; exact at alpha 255 and 0.
fn unpremultiply(pixels: &mut [u8]) {
    for px in pixels.as_chunks_mut::<4>().0 {
        let alpha = u16::from(px[3]);
        if alpha != 0 && alpha != 255 {
            for channel in &mut px[..3] {
                /* A premultiplied channel never exceeds alpha, so this stays within u8. */
                *channel = ((u16::from(*channel) * 255 + alpha / 2) / alpha).min(255) as u8;
            }
        }
    }
}
