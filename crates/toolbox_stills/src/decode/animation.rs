//! Animations frame by frame through the `image` crate (docs/stills.md "Animations"): each frame
//! composited onto the full canvas, resized as it comes so only one full-size frame is held.
//!
//! - **Budget:** the frames together stay within the caller's byte budget; when the next frame
//!   would pass it, the target size halves and the frames so far are resized to it, so a long
//!   animation plays smaller rather than not at all.
//! - **Cut-off files:** the frames decoded before the error are kept when there are two or
//!   more; a single one is shown as a still. image-webp fails outright when the cut lands in
//!   a frame's header: [`DecodeError::Truncated`], like a file cut before its first frame.
//! - **Delays** are whole milliseconds rounded so they add up as the file's do: APNG's 1/15 s
//!   is 66 or 67 ms, and 45 such frames last 3,000 ms.

use std::io::{self, BufRead, Seek};

use image::codecs::gif::GifDecoder;
use image::codecs::png::PngDecoder;
use image::codecs::webp::WebPDecoder;
use image::{AnimationDecoder as _, Delay, DynamicImage, Frames, ImageDecoder as _};

use super::{
    AnimFrame, Bgra, DecodeError, Decoded, Reader, Source, Thumbnail, decode_image, read_error,
    resize, scale_down_to_fit, to_bgra,
};
use crate::sniff::AnimationFormat;

pub(super) fn decode(
    source: Source<'_>,
    format: AnimationFormat,
    bounds: (u32, u32),
    budget: usize,
) -> Result<Decoded, DecodeError> {
    let reader = Reader::open(source).map_err(|error| read_error(source, error))?;
    let Some((canvas, frames)) = frames(reader, format).map_err(failed)? else {
        return decode_image(source, format.still(), bounds)
            .map(|fitted| Decoded::Still(fitted.image));
    };
    let mut target = scale_down_to_fit(canvas, bounds);
    let mut clock = Clock::default();
    let mut decoded: Vec<AnimFrame> = Vec::new();
    let mut bytes = 0;
    let mut cut_off = None;
    for frame in frames {
        let frame = match frame {
            Ok(frame) => frame,
            Err(error) => {
                cut_off = Some(error);
                break;
            }
        };
        let delay_ms = clock.delay_ms(frame.delay());
        let image = to_bgra(resize(
            DynamicImage::ImageRgba8(frame.into_buffer()),
            target,
        ));
        bytes += image.byte_size();
        decoded.push(AnimFrame { image, delay_ms });
        while bytes > budget {
            if target == (1, 1) {
                return Err(DecodeError::Decode(format!(
                    "its frames take more than {budget} bytes even at 1 px"
                )));
            }
            target = scale_down_to_fit(canvas, ((target.0 / 2).max(1), (target.1 / 2).max(1)));
            decoded = decoded
                .into_iter()
                .map(|frame| AnimFrame {
                    image: resize_bgra(frame.image, target),
                    ..frame
                })
                .collect();
            bytes = decoded.iter().map(|frame| frame.image.byte_size()).sum();
        }
    }
    match (decoded.len(), cut_off) {
        (0, Some(error)) => Err(failed(error)),
        (1, Some(error)) => {
            log::warn!("{source}: {error}; showing its first frame");
            Ok(Decoded::Still(decoded.swap_remove(0).image))
        }
        /* Not animated after all, or no frames: the still path decodes it. */
        (0 | 1, None) => {
            decode_image(source, format.still(), bounds).map(|fitted| Decoded::Still(fitted.image))
        }
        (count, cut_off) => {
            if let Some(error) = cut_off {
                log::warn!("{source}: {error}; keeping its first {count} frames");
            }
            Ok(Decoded::Animation(decoded))
        }
    }
}

/**
 * The thumbnail decode: the first frame fitted inside `bounds`, the canvas size, and the
 * length of one play, the sum of every frame's delay. The later frames are decoded only for
 * their delays and dropped. One that turns out to have a single frame is a still; a cut-off
 * file's whole frames count.
 */
pub(super) fn thumbnail(
    source: Source<'_>,
    format: AnimationFormat,
    bounds: (u32, u32),
) -> Result<Thumbnail, DecodeError> {
    let still = || {
        decode_image(source, format.still(), bounds).map(|fitted| Thumbnail {
            image: fitted.image,
            source_size: fitted.source,
            duration_ms: None,
        })
    };
    let reader = Reader::open(source).map_err(|error| read_error(source, error))?;
    let Some((canvas, mut frames)) = frames(reader, format).map_err(failed)? else {
        return still();
    };
    let first = match frames.next() {
        Some(first) => first.map_err(failed)?,
        None => return still(),
    };
    let mut clock = Clock::default();
    let mut duration_ms = u64::from(clock.delay_ms(first.delay()));
    let mut count = 1;
    let mut cut_off = false;
    for frame in frames {
        match frame {
            Ok(frame) => {
                duration_ms += u64::from(clock.delay_ms(frame.delay()));
                count += 1;
            }
            Err(error) => {
                log::warn!("{source}: {error}; counting its first {count} frames");
                cut_off = true;
                break;
            }
        }
    }
    if count == 1 && !cut_off {
        return still();
    }
    let image = to_bgra(resize(
        DynamicImage::ImageRgba8(first.into_buffer()),
        scale_down_to_fit(canvas, bounds),
    ));
    Ok(Thumbnail {
        image,
        source_size: canvas,
        duration_ms: (count > 1).then_some(duration_ms),
    })
}

/// An `image` error while reading frames: a file that ends early is cut off.
fn failed(error: image::ImageError) -> DecodeError {
    match error {
        image::ImageError::IoError(error) if error.kind() == io::ErrorKind::UnexpectedEof => {
            DecodeError::Truncated
        }
        error => DecodeError::Decode(error.to_string()),
    }
}

/**
 * The canvas size and frames of an animation, or `None` for a PNG or WebP that isn't animated
 * after all.
 */
fn frames<'a>(
    reader: impl BufRead + Seek + 'a,
    format: AnimationFormat,
) -> image::ImageResult<Option<((u32, u32), Frames<'a>)>> {
    Ok(match format {
        AnimationFormat::Gif => {
            let decoder = GifDecoder::new(reader)?;
            Some((decoder.dimensions(), decoder.into_frames()))
        }
        AnimationFormat::Apng => {
            let decoder = PngDecoder::new(reader)?;
            if !decoder.is_apng()? {
                return Ok(None);
            }
            Some((decoder.dimensions(), decoder.apng()?.into_frames()))
        }
        AnimationFormat::Webp => {
            let mut decoder = WebPDecoder::new(reader)?;
            if !decoder.has_animation() {
                return Ok(None);
            }
            /* Match gpui: a transparent canvas, not the file's background colour. */
            decoder.set_background_color(image::Rgba([0, 0, 0, 0]))?;
            Some((decoder.dimensions(), decoder.into_frames()))
        }
    })
}

/// A frame resized to `target`; the filter treats BGRA's channels alike, so it reads as RGBA.
fn resize_bgra(image: Bgra, target: (u32, u32)) -> Bgra {
    let (width, height) = (image.width(), image.height());
    let buffer = image::RgbaImage::from_raw(width, height, image.into_pixels())
        .expect("a Bgra holds 4 bytes per pixel");
    let resized = resize(DynamicImage::ImageRgba8(buffer), target).into_rgba8();
    let (width, height) = resized.dimensions();
    Bgra::new(width, height, resized.into_raw()).expect("an RgbaImage holds 4 bytes per pixel")
}

/**
 * Frame delays in whole milliseconds: each frame ends at its exact end time rounded, so the
 * delays add up as the exact ones do.
 */
#[derive(Default)]
struct Clock {
    exact_ms: f64,
    shown_ms: u64,
}

impl Clock {
    fn delay_ms(&mut self, delay: Delay) -> u32 {
        let (numer, denom) = delay.numer_denom_ms();
        if denom != 0 {
            self.exact_ms += f64::from(numer) / f64::from(denom);
        }
        let end = self.exact_ms.round() as u64;
        let delay = end.saturating_sub(self.shown_ms);
        self.shown_ms = end;
        u32::try_from(delay).unwrap_or(u32::MAX)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn delays_add_up_as_the_files_do() {
        let mut clock = Clock::default();
        let delays: Vec<u32> = (0..45)
            .map(|_| clock.delay_ms(Delay::from_numer_denom_ms(1_000, 15)))
            .collect();
        assert_eq!(delays.iter().sum::<u32>(), 3_000);
        assert!(
            delays.iter().all(|delay| matches!(delay, 66 | 67)),
            "{delays:?}"
        );

        let mut clock = Clock::default();
        assert_eq!(clock.delay_ms(Delay::from_numer_denom_ms(100, 1)), 100);
        assert_eq!(clock.delay_ms(Delay::from_numer_denom_ms(0, 1)), 0);
        assert_eq!(clock.delay_ms(Delay::from_numer_denom_ms(5, 2)), 3);
        assert_eq!(clock.delay_ms(Delay::from_numer_denom_ms(5, 2)), 2);
    }
}
