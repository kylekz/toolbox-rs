//! Every still through WIC (Windows; docs/stills.md "Decoding"): the twin of `imageio.rs`, with
//! the same output. ImageIO does it all in one call; in WIC the decoder builds the chain:
//!
//! 1. the decoder's first frame → `IWICBitmapScaler` (Fant). The scaler asks the frame's
//!    `IWICBitmapSourceTransform` for a reduced decode itself (JPEG's IDCT scaling, HEIF's
//!    scaled output), so a pre-scale gains nothing (measured);
//! 2. → `IWICBitmapFlipRotator` for the EXIF orientation, which WIC never applies itself. It
//!    flips first, then rotates clockwise. WIC's HEIF decoder applies `irot`/`imir` and then
//!    reports orientation 1, so nothing is rotated twice;
//! 3. → `IWICFormatConverter` to `32bppBGRA`, which is straight alpha (and takes 16-bit TIFF to
//!    8 bits);
//! 4. → `IWICColorTransform` from the file's profile to sRGB, skipped for sRGB sources. WIC's
//!    HEIF decoder exposes no colour context, so a file sniffed as HEIC or AVIF has its profile
//!    read from its `colr` box (`crate::heif`).
//!
//! `WINCODEC_ERR_COMPONENTNOTFOUND` from any step means a Store codec isn't installed.
//! Each thread that decodes joins COM's multithreaded apartment once and keeps a factory: the
//! `crate::Decoder`'s threads at their start, any other thread at its first decode.
//!
//! A file reaches WIC by name. A path of `MAX_PATH` (260) or more goes in its `\\?\` form
//! ([`wic_path`]): WIC's file stream opens a plain one only where the system's long-path setting
//! is on and the process's manifest says `longPathAware` (docs/stills.md "Long paths").
//!
//! On Windows the decode tests run through WIC, and its output meets `imageio.rs`'s
//! expectations, HEIF's profiles included, on the decode pool's MTA threads.

use std::borrow::Cow;
use std::cell::OnceCell;
use std::ffi::OsString;
use std::fs::File;
use std::io::Cursor;
use std::os::windows::ffi::{OsStrExt as _, OsStringExt as _};
use std::path::{Path, PathBuf};

use windows::Win32::Foundation::{E_UNEXPECTED, GENERIC_READ, WINCODEC_ERR_COMPONENTNOTFOUND};
use windows::Win32::Graphics::Imaging::{
    CLSID_WICImagingFactory2, GUID_WICPixelFormat32bppBGRA, IWICBitmapDecoder,
    IWICBitmapFrameDecode, IWICBitmapSource, IWICColorContext, IWICImagingFactory,
    WICBitmapCacheOnLoad, WICBitmapDitherTypeNone, WICBitmapInterpolationModeFant,
    WICBitmapPaletteTypeCustom, WICBitmapTransformFlipHorizontal, WICBitmapTransformFlipVertical,
    WICBitmapTransformOptions, WICBitmapTransformRotate0, WICBitmapTransformRotate90,
    WICBitmapTransformRotate180, WICBitmapTransformRotate270, WICColorContextExifColorSpace,
    WICDecodeMetadataCacheOnDemand,
};
use windows::Win32::System::Com::StructuredStorage::{PROPVARIANT, PropVariantClear};
use windows::Win32::System::Com::{
    CLSCTX_INPROC_SERVER, COINIT_MULTITHREADED, CoCreateInstance, CoInitializeEx,
};
use windows::Win32::System::Variant::VT_UI2;
use windows::core::{HSTRING, Interface as _, w};

use super::{Bgra, Fitted, OsError, Source, scale_down_to_fit};
use crate::heif;
use crate::sniff::ImageFormat;

thread_local! {
    /// One factory per thread, created after joining the MTA on that thread.
    static FACTORY: OnceCell<IWICImagingFactory> = const { OnceCell::new() };
}

pub(crate) fn decode(
    source: Source<'_>,
    format: ImageFormat,
    bounds: (u32, u32),
) -> Result<Fitted, OsError> {
    decode_wic(source, format, bounds).map_err(|error| {
        if error.code() == WINCODEC_ERR_COMPONENTNOTFOUND {
            OsError::NoCodec
        } else {
            OsError::Failed(format!("WIC: {error}"))
        }
    })
}

/// Joins this thread to COM's multithreaded apartment and makes its factory, ahead of a decode.
pub(crate) fn join_apartment() {
    if let Err(error) = factory() {
        log::warn!("WIC: {error}");
    }
}

fn decode_wic(
    source: Source<'_>,
    format: ImageFormat,
    bounds: (u32, u32),
) -> windows::core::Result<Fitted> {
    let factory = factory()?;
    let decoder = open(&factory, source)?;
    let frame = unsafe { decoder.GetFrame(0) }?;
    let (mut width, mut height) = (0, 0);
    unsafe { frame.GetSize(&mut width, &mut height) }?;
    let orientation = orientation(&frame);

    /* Fit the displayed size, then scale in stored orientation and rotate the small result. */
    let swaps = (5..=8).contains(&orientation);
    let displayed = if swaps {
        (height, width)
    } else {
        (width, height)
    };
    let (target_w, target_h) = scale_down_to_fit(displayed, bounds);
    let stored = if swaps {
        (target_h, target_w)
    } else {
        (target_w, target_h)
    };

    let mut image: IWICBitmapSource = frame.cast()?;
    if stored != (width, height) {
        let scaler = unsafe { factory.CreateBitmapScaler() }?;
        unsafe { scaler.Initialize(&image, stored.0, stored.1, WICBitmapInterpolationModeFant) }?;
        image = scaler.cast()?;
    }
    let transform = transform_for(orientation);
    if transform != WICBitmapTransformRotate0 {
        /*
         * The flip/rotator reads its source out of order; caching the scaled image first keeps
         * it from pulling the scaler, and the decoder, more than once.
         */
        let cached = unsafe { factory.CreateBitmapFromSource(&image, WICBitmapCacheOnLoad) }?;
        let rotator = unsafe { factory.CreateBitmapFlipRotator() }?;
        unsafe { rotator.Initialize(&cached, transform) }?;
        image = rotator.cast()?;
    }
    let converter = unsafe { factory.CreateFormatConverter() }?;
    unsafe {
        converter.Initialize(
            &image,
            &GUID_WICPixelFormat32bppBGRA,
            WICBitmapDitherTypeNone,
            None,
            0.0,
            WICBitmapPaletteTypeCustom,
        )
    }?;
    image = converter.cast()?;
    if let Some(profile) = colour_context(&factory, &frame, source, format)? {
        let srgb = unsafe { factory.CreateColorContext() }?;
        /* EXIF colour space 1 is sRGB. */
        unsafe { srgb.InitializeFromExifColorSpace(1) }?;
        let transform = unsafe { factory.CreateColorTransformer() }?;
        unsafe { transform.Initialize(&image, &profile, &srgb, &GUID_WICPixelFormat32bppBGRA) }?;
        image = transform.cast()?;
    }

    /*
     * The chain makes exactly the fitted size. The buffer is sized from that, never from what
     * the chain reports: a size WIC gets wrong fails the decode instead of being allocated.
     */
    let (mut out_w, mut out_h) = (0, 0);
    unsafe { image.GetSize(&mut out_w, &mut out_h) }?;
    if (out_w, out_h) != (target_w, target_h) {
        return Err(windows::core::Error::new(
            E_UNEXPECTED,
            format!("{out_w}×{out_h} pixels where {target_w}×{target_h} were asked for"),
        ));
    }
    let mut pixels = vec![0u8; target_w as usize * target_h as usize * 4];
    unsafe { image.CopyPixels(std::ptr::null(), target_w * 4, &mut pixels) }?;
    Ok(Fitted {
        image: Bgra::new(target_w, target_h, pixels).expect("the buffer is sized from the fit"),
        source: displayed,
    })
}

fn factory() -> windows::core::Result<IWICImagingFactory> {
    FACTORY.with(|cell| {
        if let Some(factory) = cell.get() {
            return Ok(factory.clone());
        }
        /* S_FALSE (already in the MTA) is success; a thread already in an STA is an error. */
        unsafe { CoInitializeEx(None, COINIT_MULTITHREADED) }.ok()?;
        let factory: IWICImagingFactory =
            unsafe { CoCreateInstance(&CLSID_WICImagingFactory2, None, CLSCTX_INPROC_SERVER) }?;
        Ok(cell.get_or_init(|| factory).clone())
    })
}

fn open(
    factory: &IWICImagingFactory,
    source: Source<'_>,
) -> windows::core::Result<IWICBitmapDecoder> {
    match source {
        Source::File(path) => unsafe {
            factory.CreateDecoderFromFilename(
                &HSTRING::from(wic_path(path).as_os_str()),
                None,
                GENERIC_READ,
                WICDecodeMetadataCacheOnDemand,
            )
        },
        Source::Bytes(bytes) => {
            let stream = unsafe { factory.CreateStream() }?;
            /*
             * The stream reads `bytes` in place, unowned: every object built on it is released
             * before `decode_wic` returns, while the caller still holds them.
             */
            unsafe { stream.InitializeFromMemory(bytes) }?;
            unsafe {
                factory.CreateDecoderFromStream(
                    &stream,
                    std::ptr::null(),
                    WICDecodeMetadataCacheOnDemand,
                )
            }
        }
    }
}

/**
 * `path` as WIC's file stream opens it at any length: as it is below `MAX_PATH` (260 UTF-16
 * units), else absolute in the `\\?\` form (`\\?\UNC\` for a share), which `CreateFileW` takes
 * whatever the long-path setting. `std::path::absolute` also gives it the shape that form needs:
 * backslashes, and no `.` or `..`.
 */
pub(crate) fn wic_path(path: &Path) -> Cow<'_, Path> {
    const MAX_PATH: usize = 260;
    if path.as_os_str().encode_wide().count() < MAX_PATH {
        return Cow::Borrowed(path);
    }
    let Ok(absolute) = std::path::absolute(path) else {
        return Cow::Borrowed(path);
    };
    /* In UTF-16 units, so any path Windows allows survives (`OsStr` isn't always UTF-8). */
    let wide: Vec<u16> = absolute.as_os_str().encode_wide().collect();
    let units = |text: &str| text.encode_utf16().collect::<Vec<u16>>();
    if wide.starts_with(&units(r"\\?\")) {
        return Cow::Owned(absolute);
    }
    let verbatim = if wide.starts_with(&units(r"\\")) {
        /* `\\server\share\…` is `\\?\UNC\server\share\…`. */
        [units(r"\\?\UNC"), wide[1..].to_vec()].concat()
    } else {
        [units(r"\\?\"), wide].concat()
    };
    Cow::Owned(PathBuf::from(OsString::from_wide(&verbatim)))
}

/// EXIF orientation 1–8 from `System.Photo.Orientation` (JPEG, TIFF and HEIF), else 1.
fn orientation(frame: &IWICBitmapFrameDecode) -> u16 {
    let Ok(reader) = (unsafe { frame.GetMetadataQueryReader() }) else {
        return 1;
    };
    let mut value = PROPVARIANT::default();
    let mut orientation = 1;
    if unsafe { reader.GetMetadataByName(w!("System.Photo.Orientation"), &mut value) }.is_ok() {
        let inner = unsafe { &value.Anonymous.Anonymous };
        if inner.vt == VT_UI2 {
            orientation = unsafe { inner.Anonymous.uiVal };
        }
    }
    /* Clearing a VT_UI2 frees nothing; its result says nothing worth acting on. */
    let _ = unsafe { PropVariantClear(&mut value) };
    if (1..=8).contains(&orientation) {
        orientation
    } else {
        1
    }
}

/**
 * The flip/rotation that makes an EXIF-oriented image upright. WIC's flip/rotator flips first
 * and then rotates clockwise (all eight match `image`'s `apply_orientation`), so
 * transpose (5) is flip + 270° and transverse (7) is flip + 90°.
 */
fn transform_for(orientation: u16) -> WICBitmapTransformOptions {
    let flipped = |rotation: WICBitmapTransformOptions| {
        WICBitmapTransformOptions(rotation.0 | WICBitmapTransformFlipHorizontal.0)
    };
    match orientation {
        2 => WICBitmapTransformFlipHorizontal,
        3 => WICBitmapTransformRotate180,
        4 => WICBitmapTransformFlipVertical,
        5 => flipped(WICBitmapTransformRotate270),
        6 => WICBitmapTransformRotate90,
        7 => flipped(WICBitmapTransformRotate90),
        8 => WICBitmapTransformRotate270,
        _ => WICBitmapTransformRotate0,
    }
}

/**
 * The colour context to convert from: the frame's first, or for a file sniffed as HEIC or AVIF
 * (WIC's HEIF decoder exposes none) the ICC profile in its `colr` box. `None` when there is
 * none, or when it says sRGB, in which case the pixels are already what GPUI wants.
 */
fn colour_context(
    factory: &IWICImagingFactory,
    frame: &IWICBitmapFrameDecode,
    source: Source<'_>,
    format: ImageFormat,
) -> windows::core::Result<Option<IWICColorContext>> {
    let mut count = 0;
    let context = if unsafe { frame.GetColorContexts(&mut [], &mut count) }.is_ok() && count > 0 {
        let mut contexts = (0..count)
            .map(|_| unsafe { factory.CreateColorContext() }.map(Some))
            .collect::<windows::core::Result<Vec<_>>>()?;
        unsafe { frame.GetColorContexts(&mut contexts, &mut count) }?;
        contexts.into_iter().flatten().next()
    } else if matches!(format, ImageFormat::Heif | ImageFormat::Avif) {
        let profile = match source {
            Source::File(path) => File::open(path).and_then(heif::Boxes::read),
            Source::Bytes(bytes) => heif::Boxes::read(Cursor::new(bytes)),
        }
        .map(|boxes| boxes.icc_profile());
        match profile {
            Ok(Some(profile)) => {
                let context = unsafe { factory.CreateColorContext() }?;
                unsafe { context.InitializeFromMemory(&profile) }?;
                Some(context)
            }
            Ok(None) => None,
            Err(error) => {
                log::warn!("reading a HEIF colr box: {error}; decoding it as sRGB");
                None
            }
        }
    } else {
        None
    };
    let Some(context) = context else {
        return Ok(None);
    };
    let is_srgb = unsafe { context.GetType() }? == WICColorContextExifColorSpace
        && unsafe { context.GetExifColorSpace() }? == 1;
    Ok((!is_srgb).then_some(context))
}
