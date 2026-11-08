use std::path::PathBuf;

use pretty_assertions::assert_eq;

use super::*;
use crate::sniff::AnimationFormat;

fn fixture(name: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures")
        .join(name)
}

/// No limit on an animation's frames.
const UNLIMITED: usize = usize::MAX;

/**
 * Whether this machine decodes `format`: HEIC and AVIF need the OS decoder and, on Windows, the
 * Store codecs, which GitHub's Windows runners don't have. Those tests run where the probe passes
 * (always on macOS, `probe::tests` makes sure) and say so where it doesn't.
 */
fn decodes(format: ImageFormat) -> bool {
    let supported = Codec::of(format).is_none_or(|codec| probe_codecs().supports(codec));
    if !supported {
        eprintln!("skipping {format:?}: this machine has no codec for it");
    }
    supported
}

fn still(name: &str, format: ImageFormat, bounds: (u32, u32)) -> Bgra {
    match decode(&fixture(name), Picture::Image(format), bounds, UNLIMITED).unwrap() {
        Decoded::Still(image) => image,
        Decoded::Animation(_) => panic!("{name} decoded as an animation"),
    }
}

/// `[b, g, r, a]` at (x, y).
fn pixel(image: &Bgra, x: u32, y: u32) -> [u8; 4] {
    let at = (y * image.width() + x) as usize * 4;
    image.pixels()[at..at + 4].try_into().unwrap()
}

fn close(actual: [u8; 4], expected: [u8; 4], tolerance: u8) -> bool {
    actual
        .iter()
        .zip(expected)
        .all(|(&a, e)| a.abs_diff(e) <= tolerance)
}

#[test]
fn contain_fit_never_enlarges() {
    assert_eq!(scale_down_to_fit((800, 600), (1568, 1568)), (800, 600));
    assert_eq!(scale_down_to_fit((4000, 3000), (1568, 1568)), (1568, 1176));
    assert_eq!(scale_down_to_fit((1000, 5000), (1568, 1568)), (314, 1568));
    assert_eq!(scale_down_to_fit((10_000, 1), (100, 100)), (100, 1));
    assert_eq!(scale_down_to_fit((0, 0), (10, 10)), (1, 1));
}

#[test]
fn jpegs_fit_the_bounds() {
    let image = still("still-1920x1080.jpg", ImageFormat::Jpeg, (640, 640));
    assert_eq!((image.width(), image.height()), (640, 360));
    assert_eq!(image.byte_size(), 640 * 360 * 4);
    let image = still("still-1920x1080.jpg", ImageFormat::Jpeg, (4000, 4000));
    assert_eq!(
        (image.width(), image.height()),
        (1920, 1080),
        "never enlarged"
    );
    /* 562.5 rows: ImageIO's own fit can differ by a pixel from the computed one. */
    let image = still("still-1920x1080.jpg", ImageFormat::Jpeg, (1000, 1000));
    assert!(
        image.width() == 1000 && image.height().abs_diff(563) <= 1,
        "{image:?}"
    );
}

#[test]
fn orientation_6_comes_out_portrait() {
    /* Stored 160x90, red left and blue right; turned 90° clockwise, red is on top. */
    let check = |image: &Bgra| {
        assert_eq!((image.width(), image.height()), (90, 160));
        let (top, bottom) = (pixel(image, 45, 20), pixel(image, 45, 140));
        assert!(
            close(top, [0, 0, 255, 255], 40),
            "top should be red: {top:?}"
        );
        assert!(
            close(bottom, [255, 0, 0, 255], 40),
            "bottom should be blue: {bottom:?}"
        );
    };
    check(&still(
        "orient6-160x90.jpg",
        ImageFormat::Jpeg,
        (1000, 1000),
    ));
    /* The `image` crate fallback orients the same way, fitting in stored orientation. */
    let path = fixture("orient6-160x90.jpg");
    let fitted =
        decode_with_crate(Source::File(&path), image::ImageFormat::Jpeg, (1000, 1000)).unwrap();
    check(&fitted.image);
    /* The upright size it was fitted from: stored 160×90, shown 90×160. */
    assert_eq!(fitted.source, (90, 160));
    let small = decode_with_crate(Source::File(&path), image::ImageFormat::Jpeg, (45, 80))
        .unwrap()
        .image;
    assert_eq!((small.width(), small.height()), (45, 80));
}

#[cfg(any(target_os = "macos", windows))]
#[test]
fn heic_and_avif_decode_upright() {
    if decodes(ImageFormat::Avif) {
        let avif = still("still.avif", ImageFormat::Avif, (640, 640));
        assert_eq!((avif.width(), avif.height()), (640, 360));
    }
    if !decodes(ImageFormat::Heif) {
        return;
    }
    let heic = still("still.heic", ImageFormat::Heif, (540, 540));
    assert_eq!((heic.width(), heic.height()), (432, 540));
    let p3 = still("p3-64.heic", ImageFormat::Heif, (64, 64));
    assert_eq!((p3.width(), p3.height()), (64, 64));
    assert!(
        heic.pixels()
            .as_chunks::<4>()
            .0
            .iter()
            .all(|px| px[3] == 255),
        "opaque"
    );
}

#[test]
fn single_frame_gif_and_webp_are_stills() {
    for (name, format) in [
        ("still-160x90.gif", ImageFormat::Gif),
        ("still-160x90.webp", ImageFormat::Webp),
    ] {
        let image = still(name, format, (80, 80));
        assert_eq!((image.width(), image.height()), (80, 45), "{name}");
    }
    /* Sniffed as an animation but one frame after all: decoded as a still. */
    let decoded = decode(
        &fixture("still-160x90.gif"),
        Picture::Animation(AnimationFormat::Gif),
        (80, 80),
        UNLIMITED,
    )
    .unwrap();
    assert!(matches!(decoded, Decoded::Still(image) if image.width() == 80));
}

#[test]
fn animations_keep_every_frame_and_delay() {
    for (name, format, frames) in [
        ("anim-320x180.gif", AnimationFormat::Gif, 10),
        ("anim-160x90.webp", AnimationFormat::Webp, 5),
        ("anim-160x90.png", AnimationFormat::Apng, 5),
    ] {
        let Decoded::Animation(decoded) = decode(
            &fixture(name),
            Picture::Animation(format),
            (80, 80),
            UNLIMITED,
        )
        .unwrap() else {
            panic!("{name} decoded as a still");
        };
        assert_eq!(decoded.len(), frames, "{name}");
        for frame in &decoded {
            assert_eq!((frame.image.width, frame.image.height), (80, 45), "{name}");
            assert_eq!(frame.image.byte_size(), 80 * 45 * 4, "{name}");
            assert_eq!(frame.delay_ms, 100, "{name}: 10 fps");
        }
        assert_ne!(decoded[0].image, decoded[1].image, "{name}: frames differ");
    }
}

#[test]
fn alpha_stays_straight() {
    /* Rows at alpha 255, 128 and 16 of one colour; straight alpha keeps the colour in each. */
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("alpha.png");
    let image = image::RgbaImage::from_fn(8, 3, |_, y| {
        image::Rgba([200, 100, 50, [255, 128, 16][y as usize]])
    });
    image.save(&path).unwrap();

    let os = still_at(&path);
    assert_eq!(pixel(&os, 4, 0), [50, 100, 200, 255]);
    assert!(
        close(pixel(&os, 4, 1), [50, 100, 200, 128], 1),
        "{:?}",
        pixel(&os, 4, 1)
    );
    /* Premultiplied quantisation at low alpha costs a few levels (at most 7 for a ≥ 16). */
    assert!(
        close(pixel(&os, 4, 2), [50, 100, 200, 16], 7),
        "{:?}",
        pixel(&os, 4, 2)
    );

    let fallback = decode_with_crate(Source::File(&path), image::ImageFormat::Png, (8, 8))
        .unwrap()
        .image;
    assert_eq!(
        pixel(&fallback, 4, 2),
        [50, 100, 200, 16],
        "exact through the image crate"
    );
}

fn still_at(path: &Path) -> Bgra {
    match decode(
        path,
        Picture::Image(ImageFormat::Png),
        (100, 100),
        UNLIMITED,
    )
    .unwrap()
    {
        Decoded::Still(image) => image,
        Decoded::Animation(_) => panic!("a still"),
    }
}

#[test]
fn failures_say_why() {
    let dir = tempfile::tempdir().unwrap();
    let missing = decode(
        &dir.path().join("missing.jpg"),
        Picture::Image(ImageFormat::Jpeg),
        (10, 10),
        UNLIMITED,
    );
    assert!(
        matches!(missing, Err(DecodeError::Io { .. })),
        "{missing:?}"
    );

    /* Whole as far as its markers go (SOI, a segment, EOI), but no picture. */
    let broken = dir.path().join("broken.jpg");
    std::fs::write(
        &broken,
        [0xFF, 0xD8, 0xFF, 0xE0, 0, 4, b'J', b'F', 0xFF, 0xD9],
    )
    .unwrap();
    let error = decode(
        &broken,
        Picture::Image(ImageFormat::Jpeg),
        (10, 10),
        UNLIMITED,
    )
    .unwrap_err();
    assert!(
        matches!(&error, DecodeError::Decode(message) if message.contains("the image crate")),
        "the OS failed, then the fallback: {error}"
    );

    /*
     * With the codec present (the probe passed), a broken HEIC is broken, not missing; without
     * it (no OS decoder, or no Store codec on Windows), the codec is missing.
     */
    let heic = dir.path().join("broken.heic");
    std::fs::write(&heic, b"\0\0\0\x18ftypheic\0\0\0\0mif1heic").unwrap();
    let error = decode(
        &heic,
        Picture::Image(ImageFormat::Heif),
        (10, 10),
        UNLIMITED,
    )
    .unwrap_err();
    if probe_codecs().heic {
        assert!(matches!(error, DecodeError::Decode(_)), "{error:?}");
    } else {
        assert!(
            matches!(error, DecodeError::MissingCodec(Codec::Heic)),
            "{error:?}"
        );
    }

    let missing = DecodeError::MissingCodec(Codec::Heic).to_string();
    if cfg!(windows) {
        assert_eq!(
            missing,
            "HEIC images need a codec that isn't installed: HEIF Image Extensions and HEVC Video \
             Extensions from the Microsoft Store"
        );
    } else {
        assert_eq!(missing, "HEIC images need a codec this system doesn't have");
    }
}

/// `name` cut to `percent` % of its length, in `dir`.
fn cut(dir: &Path, name: &str, percent: usize) -> PathBuf {
    let bytes = std::fs::read(fixture(name)).unwrap();
    let path = dir.join(format!("{percent}-{name}"));
    std::fs::write(&path, &bytes[..bytes.len() * percent / 100]).unwrap();
    path
}

#[test]
fn cut_off_stills_are_truncated_never_blank() {
    let dir = tempfile::tempdir().unwrap();
    let bmp = dir.path().join("whole.bmp");
    image::RgbImage::from_pixel(160, 90, image::Rgb([10, 200, 30]))
        .save(&bmp)
        .unwrap();
    for (name, format) in [
        ("still-1920x1080.jpg", ImageFormat::Jpeg),
        ("still-1080x1350.png", ImageFormat::Png),
        ("still-160x90.gif", ImageFormat::Gif),
        ("still-160x90.webp", ImageFormat::Webp),
        ("still.heic", ImageFormat::Heif),
        ("p3-64.heic", ImageFormat::Heif),
        ("still.avif", ImageFormat::Avif),
    ] {
        for percent in [10, 50, 90] {
            let path = cut(dir.path(), name, percent);
            let result = decode(&path, Picture::Image(format), (640, 640), UNLIMITED);
            assert!(
                matches!(result, Err(DecodeError::Truncated)),
                "{name} at {percent} %: {result:?}"
            );
        }
    }
    let whole = std::fs::read(&bmp).unwrap();
    for percent in [10, 50, 90] {
        let path = dir.path().join(format!("{percent}.bmp"));
        std::fs::write(&path, &whole[..whole.len() * percent / 100]).unwrap();
        let result = decode(
            &path,
            Picture::Image(ImageFormat::Bmp),
            (640, 640),
            UNLIMITED,
        );
        assert!(
            matches!(result, Err(DecodeError::Truncated)),
            "BMP at {percent} %: {result:?}"
        );
    }
}

/**
 * Where each frame of an animation fixture ends: a GIF's image data, a WebP's `ANMF` chunk,
 * an APNG's frame (at the next `fcTL`, or `IEND`).
 */
fn frame_ends(bytes: &[u8], format: AnimationFormat) -> Vec<usize> {
    let mut ends = Vec::new();
    match format {
        AnimationFormat::Gif => {
            let table = |packed: u8| {
                if packed & 0x80 != 0 {
                    3 << ((packed & 7) + 1)
                } else {
                    0
                }
            };
            let skip_blocks = |mut at: usize| {
                while bytes[at] != 0 {
                    at += 1 + bytes[at] as usize;
                }
                at + 1
            };
            let mut at = 13 + table(bytes[10]);
            loop {
                match bytes[at] {
                    0x21 => at = skip_blocks(at + 2),
                    0x2C => {
                        at = skip_blocks(at + 10 + table(bytes[at + 9]) + 1);
                        ends.push(at);
                    }
                    _ => break,
                }
            }
        }
        AnimationFormat::Webp => {
            let mut at = 12;
            while at + 8 <= bytes.len() {
                let size = u32::from_le_bytes(bytes[at + 4..at + 8].try_into().unwrap()) as usize;
                let end = at + 8 + size + (size & 1);
                if &bytes[at..at + 4] == b"ANMF" {
                    ends.push(end);
                }
                at = end;
            }
        }
        AnimationFormat::Apng => {
            let mut at = 8;
            let mut seen_frame = false;
            while at + 8 <= bytes.len() {
                let len = u32::from_be_bytes(bytes[at..at + 4].try_into().unwrap()) as usize;
                let kind = &bytes[at + 4..at + 8];
                if kind == b"fcTL" || kind == b"IEND" {
                    if seen_frame {
                        ends.push(at);
                    }
                    seen_frame = true;
                }
                at += 12 + len;
            }
        }
    }
    ends
}

#[test]
fn cut_off_animations_keep_their_whole_frames() {
    let dir = tempfile::tempdir().unwrap();
    for (name, format, frames) in [
        ("anim-320x180.gif", AnimationFormat::Gif, 10),
        ("anim-160x90.webp", AnimationFormat::Webp, 5),
        ("anim-160x90.png", AnimationFormat::Apng, 5),
    ] {
        let bytes = std::fs::read(fixture(name)).unwrap();
        let ends = frame_ends(&bytes, format);
        assert_eq!(ends.len(), frames, "{name}");
        let decode_cut = |len: usize| {
            let path = dir.path().join(format!("{len}-{name}"));
            std::fs::write(&path, &bytes[..len]).unwrap();
            decode(&path, Picture::Animation(format), (80, 80), UNLIMITED)
        };
        assert!(ends[0] + 100 < ends[1] && ends[2] + 100 < ends[3], "{name}");
        /* Partway into the second frame: the first, as a still. */
        match decode_cut(ends[0] + 100) {
            Ok(Decoded::Still(image)) => assert_eq!((image.width(), image.height()), (80, 45)),
            other => panic!("{name}: {other:?}"),
        }
        /* Partway into the fourth: three frames. */
        match decode_cut(ends[2] + 100) {
            Ok(Decoded::Animation(kept)) => {
                assert_eq!(kept.len(), 3, "{name}");
                assert!(kept.iter().all(|frame| frame.delay_ms == 100), "{name}");
            }
            other => panic!("{name}: {other:?}"),
        }
        /* Before the first frame is whole: nothing to show. */
        assert!(decode_cut(ends[0] / 2).is_err(), "{name}");
    }
}

#[test]
fn animations_shrink_to_their_budget() {
    let path = fixture("anim-320x180.gif");
    let picture = Picture::Animation(AnimationFormat::Gif);
    let whole = decode(&path, picture, (320, 320), UNLIMITED).unwrap();
    assert_eq!(whole.byte_size(), 10 * 320 * 180 * 4);

    let budget = 1_000_000;
    let Decoded::Animation(frames) = decode(&path, picture, (320, 320), budget).unwrap() else {
        panic!("an animation");
    };
    assert_eq!(frames.len(), 10);
    assert!(
        frames
            .iter()
            .all(|frame| (frame.image.width(), frame.image.height()) == (160, 90))
    );
    assert!(Decoded::Animation(frames).byte_size() <= budget);

    assert!(matches!(
        decode(&path, picture, (320, 320), 10),
        Err(DecodeError::Decode(_))
    ));
}

#[test]
fn thumbnails_of_every_fixture_still_and_animation() {
    let image = |format| Picture::Image(format);
    let animation = |format| Picture::Animation(format);
    for (name, picture, source_size, fitted, duration_ms) in [
        (
            "still-1920x1080.jpg",
            image(ImageFormat::Jpeg),
            (1920, 1080),
            (320, 180),
            None,
        ),
        (
            "orient6-160x90.jpg",
            image(ImageFormat::Jpeg),
            (90, 160),
            (90, 160),
            None,
        ),
        (
            "still-1080x1350.png",
            image(ImageFormat::Png),
            (1080, 1350),
            (256, 320),
            None,
        ),
        (
            "still-160x90.gif",
            image(ImageFormat::Gif),
            (160, 90),
            (160, 90),
            None,
        ),
        (
            "still-160x90.webp",
            image(ImageFormat::Webp),
            (160, 90),
            (160, 90),
            None,
        ),
        (
            "still.heic",
            image(ImageFormat::Heif),
            (1080, 1350),
            (256, 320),
            None,
        ),
        (
            "p3-64.heic",
            image(ImageFormat::Heif),
            (64, 64),
            (64, 64),
            None,
        ),
        (
            "still.avif",
            image(ImageFormat::Avif),
            (1280, 720),
            (320, 180),
            None,
        ),
        (
            "anim-320x180.gif",
            animation(AnimationFormat::Gif),
            (320, 180),
            (320, 180),
            Some(1000),
        ),
        (
            "anim-160x90.webp",
            animation(AnimationFormat::Webp),
            (160, 90),
            (160, 90),
            Some(500),
        ),
        (
            "anim-160x90.png",
            animation(AnimationFormat::Apng),
            (160, 90),
            (160, 90),
            Some(500),
        ),
    ] {
        if !decodes(picture.still_format()) {
            continue;
        }
        let thumbnail = thumbnail(&fixture(name), picture, 320)
            .unwrap_or_else(|error| panic!("{name}: {error}"));
        assert_eq!(thumbnail.source_size, source_size, "{name}");
        let size = (thumbnail.image.width(), thumbnail.image.height());
        /* ImageIO's fit can differ from the computed one by a pixel. */
        assert!(
            size.0.abs_diff(fitted.0) <= 1 && size.1.abs_diff(fitted.1) <= 1,
            "{name}: {size:?}"
        );
        assert_eq!(thumbnail.duration_ms, duration_ms, "{name}");
        /* The same pixels each time: apps' screenshot scenarios depend on it. */
        assert_eq!(
            thumbnail.image,
            super::thumbnail(&fixture(name), picture, 320)
                .unwrap()
                .image,
            "{name}"
        );
        /* The same from the file's bytes in memory. */
        let bytes = std::fs::read(fixture(name)).unwrap();
        assert_eq!(
            super::thumbnail(&bytes, picture, 320).unwrap(),
            thumbnail,
            "{name} from bytes"
        );
    }
}

#[test]
fn bytes_decode_as_their_files_do() {
    let animation = Picture::Animation(AnimationFormat::Gif);
    let path = fixture("anim-320x180.gif");
    let bytes = std::fs::read(&path).unwrap();
    assert_eq!(
        decode(&bytes, animation, (80, 80), UNLIMITED).unwrap(),
        decode(&path, animation, (80, 80), UNLIMITED).unwrap()
    );
    let jpeg = std::fs::read(fixture("still-1920x1080.jpg")).unwrap();
    let cut = &jpeg[..jpeg.len() / 2];
    assert!(matches!(
        decode(cut, Picture::Image(ImageFormat::Jpeg), (64, 64), UNLIMITED),
        Err(DecodeError::Truncated)
    ));
    let broken = decode(
        b"\xFF\xD8\xFF\xE0\0\x04JF\xFF\xD9".as_slice(),
        Picture::Image(ImageFormat::Jpeg),
        (10, 10),
        UNLIMITED,
    );
    assert!(matches!(broken, Err(DecodeError::Decode(_))), "{broken:?}");
    /* An RGBA copy for the `image` crate: the channels swapped back. */
    let Decoded::Still(still) = decode(
        &jpeg,
        Picture::Image(ImageFormat::Jpeg),
        (64, 64),
        UNLIMITED,
    )
    .unwrap() else {
        panic!("a still");
    };
    let [b, g, r, a] = pixel(&still, 10, 10);
    let rgba = still.into_rgba_image();
    assert_eq!((rgba.width(), rgba.height()), (64, 36));
    assert_eq!(rgba.get_pixel(10, 10).0, [r, g, b, a]);
}

#[test]
fn resize_to_fit_fits_and_never_enlarges() {
    let image = DynamicImage::ImageRgb8(image::RgbImage::from_pixel(
        2000,
        1000,
        image::Rgb([10, 200, 30]),
    ));
    let resized = resize_to_fit(image.clone(), (160, 160));
    assert_eq!((resized.width(), resized.height()), (160, 80));
    assert_eq!(resized.to_rgb8().get_pixel(80, 40).0, [10, 200, 30]);
    assert_eq!(resize_to_fit(image, (4000, 4000)).width(), 2000);
}

#[test]
fn an_animation_thumbnail_is_its_first_frame() {
    let path = fixture("anim-160x90.webp");
    let Decoded::Animation(frames) = decode(
        &path,
        Picture::Animation(AnimationFormat::Webp),
        (160, 160),
        UNLIMITED,
    )
    .unwrap() else {
        panic!("decoded as a still");
    };
    let thumbnail = thumbnail(&path, Picture::Animation(AnimationFormat::Webp), 160).unwrap();
    assert_eq!(thumbnail.image, frames[0].image);
}

/// WIC gets a long path in its `\\?\` form, a share's as `\\?\UNC\`, and a short one as it is.
#[cfg(windows)]
#[test]
fn wic_takes_long_paths_in_their_verbatim_form() {
    use super::wic::wic_path;
    let short = Path::new(r"C:\photos\a.jpg");
    assert_eq!(wic_path(short), short);
    let folder = "f".repeat(200);
    let long = PathBuf::from(format!(r"C:\{folder}\{folder}\a.jpg"));
    assert_eq!(
        wic_path(&long),
        PathBuf::from(format!(r"\\?\C:\{folder}\{folder}\a.jpg"))
    );
    let share = PathBuf::from(format!(r"\\nas\photos\{folder}\{folder}\a.jpg"));
    assert_eq!(
        wic_path(&share),
        PathBuf::from(format!(r"\\?\UNC\nas\photos\{folder}\{folder}\a.jpg"))
    );
    let verbatim = PathBuf::from(format!(r"\\?\C:\{folder}\{folder}\a.jpg"));
    assert_eq!(wic_path(&verbatim), verbatim);
}

/**
 * A TIFF (WIC's alone: no fallback, and no Store codec) deep enough that its path passes
 * `MAX_PATH` decodes, whatever the system's long-path setting (docs/stills.md "Long paths").
 */
#[cfg(windows)]
#[test]
fn a_still_past_max_path_decodes() {
    let dir = tempfile::tempdir().unwrap();
    let mut deep = dir.path().to_path_buf();
    while deep.as_os_str().len() < 300 {
        deep.push("a_folder_name_long_enough_to_pass_max_path_soon");
    }
    std::fs::create_dir_all(&deep).unwrap();
    let path = deep.join("still.tiff");
    std::fs::write(
        &path,
        super::container::tests::directory_first_tiff(64, 32, 32, false),
    )
    .unwrap();
    match decode(
        &path,
        Picture::Image(ImageFormat::Tiff),
        (640, 640),
        UNLIMITED,
    )
    .unwrap()
    {
        Decoded::Still(image) => assert_eq!((image.width(), image.height()), (64, 32)),
        Decoded::Animation(_) => panic!("decoded as an animation"),
    }
}

/**
 * A cut-off TIFF is refused, never half drawn: in a strip per row, which ImageIO and WIC refuse
 * themselves, and in one strip, which WIC hands back with its missing rows black and the
 * container walk refuses (`container::tiff`). Its directory comes first, so the cut loses only
 * pixels.
 */
#[cfg(any(target_os = "macos", windows))]
#[test]
fn a_cut_off_tiff_is_refused() {
    let dir = tempfile::tempdir().unwrap();
    let decode_tiff =
        |path: &Path| decode(path, Picture::Image(ImageFormat::Tiff), (64, 64), UNLIMITED);
    for rows in [1, 32] {
        let tiff = super::container::tests::directory_first_tiff(64, 32, rows, false);
        let whole = dir.path().join(format!("whole-{rows}.tiff"));
        std::fs::write(&whole, &tiff).unwrap();
        match decode_tiff(&whole).unwrap() {
            Decoded::Still(image) => assert_eq!((image.width(), image.height()), (64, 32)),
            Decoded::Animation(_) => panic!("decoded as an animation"),
        }
        let cut = dir.path().join(format!("cut-{rows}.tiff"));
        std::fs::write(&cut, &tiff[..tiff.len() * 9 / 10]).unwrap();
        assert!(
            decode_tiff(&cut).is_err(),
            "a cut-off TIFF in {rows}-row strips decoded"
        );
    }
}
