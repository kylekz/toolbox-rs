//! Content sniffing (docs/stills.md "Sniffing"): a file's leading bytes decide its format and
//! kind. The extension only breaks a tie the bytes leave open (a HEIF file whose brands don't
//! say HEIC or AVIF, Matroska without its DocType, an MP4-branded `.mov`). Animated GIF, WebP and
//! APNG are told apart from single-frame ones here.

use std::fs::File;
use std::io::{self, Read as _};
use std::path::Path;

/// What a file is: a picture this crate decodes, or a video for the app's player.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Media {
    Picture(Picture),
    /// Only sniffed here: the app's player reads the codec itself.
    Video(VideoFormat),
}

/// A file [`crate::decode`] paints: a still image or an animation.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Picture {
    /// A still, or the only frame of a GIF, WebP or PNG with one frame.
    Image(ImageFormat),
    /// A GIF, WebP or PNG with more than one frame.
    Animation(AnimationFormat),
}

/// The coarse kind of a [`Media`]: what an app stores or filters by.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum MediaKind {
    Image,
    Animation,
    Video,
}

/// A still image's format. Every one goes through the OS decoder.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum ImageFormat {
    Jpeg,
    Png,
    Gif,
    Bmp,
    Webp,
    Tiff,
    /// HEIC and other HEIF stills.
    Heif,
    Avif,
}

/// An animation's format, decoded frame by frame by the `image` crate.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum AnimationFormat {
    Gif,
    Webp,
    Apng,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum VideoFormat {
    /// ISO base media with an MP4-family brand (`.mp4`, `.m4v`).
    Mp4,
    /// QuickTime: `ftyp qt  `, or a legacy file that starts with a `moov`/`mdat`/`wide` atom.
    Mov,
    Mkv,
    Webm,
}

impl Media {
    pub fn kind(self) -> MediaKind {
        match self {
            Self::Picture(Picture::Image(_)) => MediaKind::Image,
            Self::Picture(Picture::Animation(_)) => MediaKind::Animation,
            Self::Video(_) => MediaKind::Video,
        }
    }

    /// The picture, unless it is a video.
    pub fn picture(self) -> Option<Picture> {
        match self {
            Self::Picture(picture) => Some(picture),
            Self::Video(_) => None,
        }
    }
}

impl Picture {
    /// The still's format, or for an animation its first frame's.
    pub fn still_format(self) -> ImageFormat {
        match self {
            Self::Image(format) => format,
            Self::Animation(format) => format.still(),
        }
    }
}

impl ImageFormat {
    /// The usual file extension, lowercase and without its dot: what a copy is saved under.
    pub fn extension(self) -> &'static str {
        match self {
            Self::Jpeg => "jpg",
            Self::Png => "png",
            Self::Gif => "gif",
            Self::Bmp => "bmp",
            Self::Webp => "webp",
            Self::Tiff => "tiff",
            Self::Heif => "heic",
            Self::Avif => "avif",
        }
    }
}

impl AnimationFormat {
    /// The format its first frame is decoded as when it turns out to have only one.
    pub fn still(self) -> ImageFormat {
        match self {
            Self::Gif => ImageFormat::Gif,
            Self::Webp => ImageFormat::Webp,
            Self::Apng => ImageFormat::Png,
        }
    }
}

/// Bytes read first; enough for every signature and a typical APNG's `acTL`.
const HEAD_LEN: u64 = 4096;

/**
 * Sniffs the file at `path`: `None` for anything that is neither a picture this crate decodes
 * nor a video format it knows. Reads the first 4 KB, and more of a GIF, WebP or PNG only until
 * it has seen a second frame or ruled one out. Blocking: call it off the UI thread.
 */
pub fn sniff_file(path: &Path) -> io::Result<Option<Media>> {
    let mut file = File::open(path)?;
    let mut bytes = Vec::new();
    let mut chunk = HEAD_LEN;
    let mut at_end = (&mut file).take(chunk).read_to_end(&mut bytes)? < chunk as usize;
    let Some(signature) = signature(
        &bytes,
        path.extension().and_then(|extension| extension.to_str()),
    ) else {
        return Ok(None);
    };
    loop {
        if let Some(media) = classify(signature, &bytes, at_end) {
            return Ok(Some(media));
        }
        /* Doubling keeps a large single-frame GIF to a few reads. */
        at_end = (&mut file).take(chunk).read_to_end(&mut bytes)? < chunk as usize;
        chunk *= 2;
    }
}

/**
 * [`sniff_file`] on bytes already in memory: the whole file, or a prefix of it, which ends an
 * animation's frame count early (a GIF cut before its second frame sniffs as a still).
 * `extension` is the file name's, when there is one, without its dot.
 */
pub fn sniff_bytes(bytes: &[u8], extension: Option<&str>) -> Option<Media> {
    classify(signature(bytes, extension)?, bytes, true)
}

/// What the leading bytes say, before an animation's frames are counted.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Signature {
    Image(ImageFormat),
    Video(VideoFormat),
}

/**
 * The media for `signature`, or `None` when `bytes` ends before an animation's second frame
 * or its absence is known and more of the file can be read (`at_end` false).
 */
fn classify(signature: Signature, bytes: &[u8], at_end: bool) -> Option<Media> {
    let animation = match signature {
        Signature::Video(format) => return Some(Media::Video(format)),
        Signature::Image(ImageFormat::Gif) => AnimationFormat::Gif,
        Signature::Image(ImageFormat::Webp) => AnimationFormat::Webp,
        Signature::Image(ImageFormat::Png) => AnimationFormat::Apng,
        Signature::Image(format) => return Some(Media::Picture(Picture::Image(format))),
    };
    let animated = match animation {
        AnimationFormat::Gif => gif_is_animated(bytes),
        AnimationFormat::Webp => webp_is_animated(bytes),
        AnimationFormat::Apng => png_is_animated(bytes),
    };
    let picture = match animated {
        Some(true) => Picture::Animation(animation),
        /* A file cut short before a second frame is a still; decoding shows what is there. */
        Some(false) => Picture::Image(animation.still()),
        None if at_end => Picture::Image(animation.still()),
        None => return None,
    };
    Some(Media::Picture(picture))
}

fn signature(bytes: &[u8], extension: Option<&str>) -> Option<Signature> {
    let extension = extension.map(str::to_ascii_lowercase);
    let extension = extension.as_deref();
    if bytes.starts_with(b"\x89PNG\r\n\x1a\n") {
        return Some(Signature::Image(ImageFormat::Png));
    }
    if bytes.starts_with(&[0xFF, 0xD8, 0xFF]) {
        return Some(Signature::Image(ImageFormat::Jpeg));
    }
    if bytes.starts_with(b"GIF87a") || bytes.starts_with(b"GIF89a") {
        return Some(Signature::Image(ImageFormat::Gif));
    }
    if bytes.starts_with(b"II*\0") || bytes.starts_with(b"MM\0*") {
        /* Camera raw is TIFF underneath, and unsupported. */
        return extension
            .is_none_or(|extension| !RAW_EXTENSIONS.contains(&extension))
            .then_some(Signature::Image(ImageFormat::Tiff));
    }
    if bytes.len() >= 12 && &bytes[0..4] == b"RIFF" {
        return match &bytes[8..12] {
            b"WEBP" => Some(Signature::Image(ImageFormat::Webp)),
            /* AVI is RIFF too, and no app's player opens it: unsupported. */
            _ => None,
        };
    }
    if is_bmp(bytes) {
        return Some(Signature::Image(ImageFormat::Bmp));
    }
    if bytes.starts_with(&[0x1A, 0x45, 0xDF, 0xA3]) {
        return Some(Signature::Video(sniff_matroska(bytes, extension)));
    }
    sniff_iso_bmff(bytes, extension)
}

fn is_bmp(bytes: &[u8]) -> bool {
    if bytes.len() < 18 || !bytes.starts_with(b"BM") {
        return false;
    }
    /* "BM" alone is too weak (plain text can start with it); require a known DIB header size. */
    let dib_header_len = u32::from_le_bytes([bytes[14], bytes[15], bytes[16], bytes[17]]);
    matches!(dib_header_len, 12 | 16 | 40 | 52 | 56 | 64 | 108 | 124)
}

/// The EBML header carries its DocType ("webm" or "matroska") in its first few dozen bytes.
fn sniff_matroska(bytes: &[u8], extension: Option<&str>) -> VideoFormat {
    let header = &bytes[..bytes.len().min(64)];
    if header.windows(4).any(|window| window == b"webm") {
        VideoFormat::Webm
    } else if header.windows(8).any(|window| window == b"matroska") {
        VideoFormat::Mkv
    } else if extension == Some("webm") {
        VideoFormat::Webm
    } else {
        VideoFormat::Mkv
    }
}

/**
 * Camera raw formats built on TIFF, lowercase. Their bytes sniff as TIFF, and the OS decoders
 * would show the small embedded preview or a raw development of their own; raw is unsupported.
 */
const RAW_EXTENSIONS: [&str; 18] = [
    "3fr", "arw", "cr2", "crw", "dng", "erf", "iiq", "mos", "mrw", "nef", "nrw", "orf", "pef",
    "raf", "rw2", "sr2", "srf", "x3f",
];

const HEIF_BRANDS: [&[u8; 4]; 6] = [b"heic", b"heix", b"heim", b"heis", b"hevc", b"hevx"];
const AVIF_BRANDS: [&[u8; 4]; 2] = [b"avif", b"avis"];
/// Generic HEIF brands: the compatible brands, or the extension, say HEIC or AVIF.
const GENERIC_HEIF_BRANDS: [&[u8; 4]; 3] = [b"mif1", b"msf1", b"miaf"];
/**
 * ISO base media brands that aren't video: audio-only MP4 and audiobooks, Canon's CR3 raw and
 * JPEG 2000. They share MP4's container, so without this they would sniff as video.
 */
const NOT_VIDEO_BRANDS: [&[u8; 4]; 7] = [
    b"M4A ", b"M4B ", b"M4P ", b"crx ", b"jp2 ", b"jpm ", b"jpx ",
];

fn sniff_iso_bmff(bytes: &[u8], extension: Option<&str>) -> Option<Signature> {
    if bytes.len() < 12 {
        return None;
    }
    let atom_len = u32::from_be_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]);
    let atom_type = &bytes[4..8];
    if atom_type == b"ftyp" {
        let brand = &bytes[8..12];
        let is = |brands: &[&[u8; 4]]| brands.iter().any(|known| known.as_slice() == brand);
        if is(&HEIF_BRANDS) {
            return Some(Signature::Image(ImageFormat::Heif));
        }
        if is(&AVIF_BRANDS) {
            return Some(Signature::Image(ImageFormat::Avif));
        }
        if is(&GENERIC_HEIF_BRANDS) {
            return Some(Signature::Image(generic_heif(bytes, atom_len, extension)));
        }
        if is(&NOT_VIDEO_BRANDS) {
            return None;
        }
        let mov = brand == b"qt  " || matches!(extension, Some("mov" | "qt"));
        return Some(Signature::Video(if mov {
            VideoFormat::Mov
        } else {
            VideoFormat::Mp4
        }));
    }
    /* Pre-2001 QuickTime files have no ftyp; accept the usual first atoms with a sane length. */
    let plausible_len = atom_len == 0 || atom_len == 1 || atom_len >= 8;
    let legacy_first_atom = matches!(atom_type, b"moov" | b"mdat" | b"wide" | b"pnot");
    (plausible_len && legacy_first_atom).then_some(Signature::Video(VideoFormat::Mov))
}

/// A `mif1`/`msf1`/`miaf` file: AVIF or HEIC by its compatible brands, else by its extension.
fn generic_heif(bytes: &[u8], ftyp_len: u32, extension: Option<&str>) -> ImageFormat {
    /* The compatible brands follow the major brand (8..12) and minor version (12..16). */
    let end = bytes.len().min(ftyp_len as usize);
    let (compatible, _) = bytes.get(16..end).unwrap_or_default().as_chunks::<4>();
    for brand in compatible {
        if AVIF_BRANDS.contains(&brand) {
            return ImageFormat::Avif;
        }
        if HEIF_BRANDS.contains(&brand) {
            return ImageFormat::Heif;
        }
    }
    match extension {
        Some("avif" | "avifs") => ImageFormat::Avif,
        _ => ImageFormat::Heif,
    }
}

/**
 * Whether a GIF has a second image descriptor, walking its block structure. `None` when the
 * bytes end before the answer; a malformed block ends the walk with the frames seen so far.
 */
fn gif_is_animated(bytes: &[u8]) -> Option<bool> {
    /* The end of a run of sub-blocks starting at `pos`, or `None` past the end of `bytes`. */
    fn skip_sub_blocks(bytes: &[u8], mut pos: usize) -> Option<usize> {
        loop {
            let len = *bytes.get(pos)? as usize;
            pos += 1;
            if len == 0 {
                return Some(pos);
            }
            pos += len;
        }
    }
    const HEADER_AND_SCREEN_DESCRIPTOR: usize = 13;
    let screen_packed = *bytes.get(10)?;
    let mut pos = HEADER_AND_SCREEN_DESCRIPTOR + gif_color_table_len(screen_packed);
    let mut frames = 0;
    loop {
        match *bytes.get(pos)? {
            0x21 => pos = skip_sub_blocks(bytes, pos + 2)?,
            0x2C => {
                frames += 1;
                if frames == 2 {
                    return Some(true);
                }
                let packed = *bytes.get(pos + 9)?;
                /* Descriptor (10 bytes with its introducer), local table, LZW code size byte. */
                let data_start = pos + 10 + gif_color_table_len(packed) + 1;
                pos = skip_sub_blocks(bytes, data_start)?;
            }
            /* 0x3B is the trailer; anything else is garbage. */
            _ => return Some(false),
        }
    }
}

/// The size of the colour table a GIF descriptor's packed byte announces.
pub(crate) fn gif_color_table_len(packed: u8) -> usize {
    if packed & 0x80 != 0 {
        3 * (1 << ((packed & 0x07) + 1))
    } else {
        0
    }
}

/**
 * Whether a WebP has a second `ANMF` frame. Only an extended file (`VP8X`) with its animation
 * flag set can; `None` when the bytes end before the answer.
 */
fn webp_is_animated(bytes: &[u8]) -> Option<bool> {
    /* RIFF header (12), chunk fourcc "VP8X", chunk size (4), flags byte; bit 1 = animation. */
    if bytes.get(12..16)? != b"VP8X" || bytes.get(20)? & 0x02 == 0 {
        return Some(false);
    }
    let mut pos = 12;
    let mut frames = 0;
    loop {
        let fourcc = bytes.get(pos..pos + 4)?;
        let size = u32::from_le_bytes(bytes.get(pos + 4..pos + 8)?.try_into().ok()?) as usize;
        if fourcc == b"ANMF" {
            frames += 1;
            if frames == 2 {
                return Some(true);
            }
        }
        /* Chunks are padded to an even size. */
        pos += 8 + size + (size & 1);
        if pos >= bytes.len() {
            return None;
        }
    }
}

/**
 * Whether a PNG is an APNG of more than one frame: its `acTL` chunk, which comes before the
 * first `IDAT`, counts them. `None` when the bytes end before either chunk.
 */
fn png_is_animated(bytes: &[u8]) -> Option<bool> {
    let mut pos = 8;
    loop {
        let len = u32::from_be_bytes(bytes.get(pos..pos + 4)?.try_into().ok()?) as usize;
        match bytes.get(pos + 4..pos + 8)? {
            b"acTL" => {
                let frames = bytes.get(pos + 8..pos + 12)?;
                return Some(u32::from_be_bytes(frames.try_into().ok()?) > 1);
            }
            b"IDAT" | b"IEND" => return Some(false),
            /* Length, type, data and CRC. */
            _ => pos += 12 + len,
        }
    }
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use pretty_assertions::assert_eq;

    use super::*;

    fn fixture(name: &str) -> PathBuf {
        Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("tests/fixtures")
            .join(name)
    }

    fn image(format: ImageFormat) -> Option<Media> {
        Some(Media::Picture(Picture::Image(format)))
    }

    fn animation(format: AnimationFormat) -> Option<Media> {
        Some(Media::Picture(Picture::Animation(format)))
    }

    fn video(format: VideoFormat) -> Option<Media> {
        Some(Media::Video(format))
    }

    #[test]
    fn sniffs_every_fixture() {
        let cases = [
            ("still-1920x1080.jpg", image(ImageFormat::Jpeg)),
            ("orient6-160x90.jpg", image(ImageFormat::Jpeg)),
            ("still-1080x1350.png", image(ImageFormat::Png)),
            ("still-160x90.gif", image(ImageFormat::Gif)),
            ("still-160x90.webp", image(ImageFormat::Webp)),
            ("still.heic", image(ImageFormat::Heif)),
            ("p3-64.heic", image(ImageFormat::Heif)),
            ("still.avif", image(ImageFormat::Avif)),
            ("anim-320x180.gif", animation(AnimationFormat::Gif)),
            ("anim-160x90.webp", animation(AnimationFormat::Webp)),
            ("anim-160x90.png", animation(AnimationFormat::Apng)),
            /* A video's first 64 bytes, which is all sniffing reads of one. */
            ("head-h264-1080p30.mp4", video(VideoFormat::Mp4)),
            ("head-h264-portrait.mp4", video(VideoFormat::Mp4)),
            ("head-h264-audio-720p.mp4", video(VideoFormat::Mp4)),
            ("head-hevc-1080p.mov", video(VideoFormat::Mov)),
            ("head-vp9-720p.webm", video(VideoFormat::Webm)),
            ("head-av1-720p.mkv", video(VideoFormat::Mkv)),
            ("README.md", None),
        ];
        for (name, expected) in cases {
            assert_eq!(sniff_file(&fixture(name)).unwrap(), expected, "{name}");
        }
    }

    #[test]
    fn kinds_follow_the_store() {
        assert_eq!(image(ImageFormat::Heif).unwrap().kind(), MediaKind::Image);
        assert_eq!(
            animation(AnimationFormat::Apng).unwrap().kind(),
            MediaKind::Animation
        );
        assert_eq!(video(VideoFormat::Mkv).unwrap().kind(), MediaKind::Video);
        assert_eq!(video(VideoFormat::Mkv).unwrap().picture(), None);
        let apng = Picture::Animation(AnimationFormat::Apng);
        assert_eq!(Media::Picture(apng).picture(), Some(apng));
        assert_eq!(apng.still_format(), ImageFormat::Png);
        assert_eq!(apng.still_format().extension(), "png");
        assert_eq!(ImageFormat::Heif.extension(), "heic");
    }

    #[test]
    fn a_large_still_gif_is_read_past_its_head() {
        /* One frame whose image data runs well past the first read. */
        let mut gif = b"GIF89a\x01\x00\x01\x00\x00\x00\x00".to_vec();
        gif.extend_from_slice(&[0x2C, 0, 0, 0, 0, 1, 0, 1, 0, 0, 0x08]);
        for _ in 0..200 {
            gif.push(255);
            gif.extend_from_slice(&[0; 255]);
        }
        gif.extend_from_slice(&[0, 0x3B]);
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("big.gif");
        std::fs::write(&path, &gif).unwrap();
        assert_eq!(sniff_file(&path).unwrap(), image(ImageFormat::Gif));

        /* The same with a second frame after it is an animation. */
        gif.pop();
        gif.extend_from_slice(&[0x2C, 0, 0, 0, 0, 1, 0, 1, 0, 0, 0x08, 1, 0, 0, 0x3B]);
        std::fs::write(&path, &gif).unwrap();
        assert_eq!(sniff_file(&path).unwrap(), animation(AnimationFormat::Gif));
    }

    #[test]
    fn truncated_animations_are_stills() {
        let bytes = std::fs::read(fixture("anim-320x180.gif")).unwrap();
        assert_eq!(gif_is_animated(&bytes), Some(true));
        assert_eq!(gif_is_animated(&bytes[..20]), None);
        assert_eq!(sniff_bytes(&bytes[..20], None), image(ImageFormat::Gif));

        let bytes = std::fs::read(fixture("anim-160x90.webp")).unwrap();
        assert_eq!(webp_is_animated(&bytes), Some(true));
        assert_eq!(webp_is_animated(&bytes[..200]), None);

        let bytes = std::fs::read(fixture("still-160x90.webp")).unwrap();
        assert_eq!(webp_is_animated(&bytes), Some(false));
    }

    #[test]
    fn heif_brands_and_the_extension_tiebreak() {
        let ftyp = |major: &[u8; 4], compatible: &[&[u8; 4]]| {
            let len = 16 + 4 * compatible.len();
            let mut bytes = (len as u32).to_be_bytes().to_vec();
            bytes.extend_from_slice(b"ftyp");
            bytes.extend_from_slice(major);
            bytes.extend_from_slice(&[0; 4]);
            for brand in compatible {
                bytes.extend_from_slice(*brand);
            }
            bytes
        };
        let avif = ftyp(b"mif1", &[b"mif1", b"avif"]);
        assert_eq!(sniff_bytes(&avif, Some("heic")), image(ImageFormat::Avif));
        let heic = ftyp(b"mif1", &[b"miaf", b"heic"]);
        assert_eq!(sniff_bytes(&heic, Some("avif")), image(ImageFormat::Heif));
        let bare = ftyp(b"mif1", &[b"mif1"]);
        assert_eq!(sniff_bytes(&bare, Some("AVIF")), image(ImageFormat::Avif));
        assert_eq!(sniff_bytes(&bare, Some("heif")), image(ImageFormat::Heif));
        assert_eq!(
            sniff_bytes(&ftyp(b"avis", &[]), None),
            image(ImageFormat::Avif)
        );

        assert_eq!(
            sniff_bytes(&ftyp(b"isom", &[]), None),
            video(VideoFormat::Mp4)
        );
        assert_eq!(
            sniff_bytes(&ftyp(b"isom", &[]), Some("MOV")),
            video(VideoFormat::Mov)
        );
        assert_eq!(
            sniff_bytes(&ftyp(b"qt  ", &[]), None),
            video(VideoFormat::Mov)
        );
        assert_eq!(sniff_bytes(&ftyp(b"M4A ", &[]), None), None, "audio only");
        assert_eq!(sniff_bytes(&ftyp(b"crx ", &[]), None), None, "Canon raw");
    }

    #[test]
    fn camera_raw_is_unsupported() {
        let tiff = b"II*\0\x08\0\0\0CR\x02\0";
        for extension in [
            "cr2", "CR2", "crw", "nef", "nrw", "arw", "sr2", "srf", "orf", "pef", "dng", "DNG",
            "3fr", "erf", "iiq", "rw2", "raf", "mos", "mrw", "x3f",
        ] {
            assert_eq!(sniff_bytes(tiff, Some(extension)), None, "{extension}");
        }
        assert_eq!(sniff_bytes(tiff, Some("tif")), image(ImageFormat::Tiff));
        assert_eq!(sniff_bytes(tiff, None), image(ImageFormat::Tiff));
        /* Panasonic's, Olympus's and Fujifilm's own magics are no format this crate knows. */
        for magic in [
            &b"IIU\0\x08\0\0\0"[..],
            b"IIRO\x08\0\0\0",
            b"IIRS\x08\0\0\0",
        ] {
            assert_eq!(sniff_bytes(magic, Some("tif")), None);
        }
        let mut raf = b"FUJIFILMCCD-RAW 0201".to_vec();
        raf.resize(64, 0);
        assert_eq!(sniff_bytes(&raf, Some("raf")), None);
    }

    #[test]
    fn sniffs_containers_from_headers() {
        let mut legacy_mov = vec![0, 0, 0, 8];
        legacy_mov.extend_from_slice(b"wide\0\0\0\x10mdat");
        assert_eq!(sniff_bytes(&legacy_mov, None), video(VideoFormat::Mov));

        let mut webm = vec![0x1A, 0x45, 0xDF, 0xA3, 0x9F, 0x42, 0x86, 0x81, 0x01];
        webm.extend_from_slice(b"\x42\x82\x84webm");
        assert_eq!(sniff_bytes(&webm, None), video(VideoFormat::Webm));

        let mut mkv = vec![0x1A, 0x45, 0xDF, 0xA3, 0xA3, 0x42, 0x86, 0x81, 0x01];
        mkv.extend_from_slice(b"\x42\x82\x88matroska");
        assert_eq!(sniff_bytes(&mkv, Some("webm")), video(VideoFormat::Mkv));
        let ebml_only = [0x1A, 0x45, 0xDF, 0xA3, 0x80];
        assert_eq!(
            sniff_bytes(&ebml_only, Some("webm")),
            video(VideoFormat::Webm)
        );

        let mut avi = b"RIFF\0\0\0\0AVI LIST".to_vec();
        avi.resize(64, 0);
        assert_eq!(sniff_bytes(&avi, None), None, "AVI is unsupported");
        assert_eq!(sniff_bytes(&avi, Some("avi")), None);

        assert_eq!(
            sniff_bytes(b"MM\0*\0\0\0\x08", None),
            image(ImageFormat::Tiff)
        );
    }

    #[test]
    fn rejects_non_media() {
        assert_eq!(sniff_bytes(b"", None), None);
        assert_eq!(
            sniff_bytes(b"<svg xmlns='http://www.w3.org/2000/svg'/>", None),
            None
        );
        assert_eq!(sniff_bytes(b"BM is not a bitmap, just text!", None), None);
        assert_eq!(sniff_bytes(b"RIFF\0\0\0\0WAVEfmt ", None), None);
        assert_eq!(
            sniff_bytes(&[0u8; 64], Some("jpg")),
            None,
            "the extension never decides"
        );
    }

    #[test]
    fn an_unreadable_file_is_an_error() {
        let dir = tempfile::tempdir().unwrap();
        assert!(sniff_file(&dir.path().join("missing.jpg")).is_err());
    }
}
