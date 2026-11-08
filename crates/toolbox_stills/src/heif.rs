//! A HEIC's or AVIF's boxes, walked without decoding (ISO/IEC 23008-12 on ISO base media,
//! 14496-12): whether the file holds every byte its boxes and item locations claim (the
//! completeness check, docs/stills.md "Cut-off files"), whether it has an alpha plane, and its
//! ICC profile. WIC's HEIF decoder exposes no colour context, so the Windows decoder hands this
//! profile to its colour transform itself; an iPhone's Display P3 photos would look desaturated
//! otherwise.
//!
//! Properties live in the top-level `meta` box (a full box): `iprp` → `ipco`. The first `colr`
//! of type `prof` or `rICC` is the profile; `nclx` (CICP) carries none. An `auxC` naming an
//! alpha plane means the picture can be transparent.

use std::io::{self, Read, Seek, SeekFrom};

/// A `meta` box larger than this isn't read; real ones are a few KB.
const MAX_META_LEN: u64 = 16 * 1024 * 1024;

/// The `auxC` types of an alpha plane: MPEG-B's (AVIF) and HEVC's (HEIC).
const ALPHA_AUX_TYPES: [&[u8]; 2] = [
    b"urn:mpeg:mpegB:cicp:systems:auxiliary:alpha",
    b"urn:mpeg:hevc:2015:auxid:1",
];

/// What a walk of the top-level boxes found.
pub(crate) struct Boxes {
    /// The `meta` box's payload, when it has one of a sane size.
    meta: Option<Vec<u8>>,
    len: u64,
    /// Every top-level box ends within the file.
    boxes_fit: bool,
}

impl Boxes {
    /**
     * Walks `file`'s top-level boxes, reading only `meta`. A malformed box ends the walk
     * without counting against the file: the decoder judges those.
     */
    pub(crate) fn read(mut file: impl Read + Seek) -> io::Result<Self> {
        let len = file.seek(SeekFrom::End(0))?;
        file.rewind()?;
        let mut meta = None;
        let mut start = 0u64;
        let boxes_fit = loop {
            if start == len {
                break true;
            }
            let mut header = [0u8; 8];
            match file.read_exact(&mut header) {
                Err(error) if error.kind() == io::ErrorKind::UnexpectedEof => break false,
                result => result?,
            }
            let (size, kind) = header.split_at(4);
            let (size, header_len) = match u32::from_be_bytes([size[0], size[1], size[2], size[3]])
            {
                /* The box runs to the end of the file. */
                0 => break true,
                1 => {
                    let mut large = [0u8; 8];
                    match file.read_exact(&mut large) {
                        Err(error) if error.kind() == io::ErrorKind::UnexpectedEof => break false,
                        result => result?,
                    }
                    (u64::from_be_bytes(large), 16)
                }
                size => (u64::from(size), 8),
            };
            let Some(end) = start.checked_add(size).filter(|_| size >= header_len) else {
                break true;
            };
            if end > len {
                break false;
            }
            let payload_len = size - header_len;
            if kind == b"meta" && meta.is_none() && payload_len <= MAX_META_LEN {
                let mut payload = vec![0u8; payload_len as usize];
                file.read_exact(&mut payload)?;
                meta = Some(payload);
            }
            file.seek(SeekFrom::Start(end))?;
            start = end;
        };
        Ok(Self {
            meta,
            len,
            boxes_fit,
        })
    }

    /// Every top-level box, and every item extent `iloc` places in the file, ends within it.
    pub(crate) fn is_complete(&self) -> bool {
        self.boxes_fit
            && self
                .meta_child(b"iloc")
                .and_then(|iloc| extents_fit(iloc, self.len))
                .unwrap_or(true)
    }

    /// Whether an `auxC` property names an alpha plane.
    pub(crate) fn has_alpha(&self) -> bool {
        self.properties().any(|(kind, payload)| {
            /* A full box: version and flags, then the type as a C string. */
            kind == b"auxC"
                && payload.get(4..).is_some_and(|rest| {
                    let aux_type = rest.split(|byte| *byte == 0).next().unwrap_or_default();
                    ALPHA_AUX_TYPES.contains(&aux_type)
                })
        })
    }

    /// The ICC profile of the first `colr` box that carries one.
    #[cfg(any(windows, test))]
    pub(crate) fn icc_profile(&self) -> Option<Vec<u8>> {
        self.properties().find_map(|(kind, payload)| {
            let colour_type = payload.get(0..4)?;
            (kind == b"colr" && matches!(colour_type, b"prof" | b"rICC"))
                .then(|| payload[4..].to_vec())
        })
    }

    /// The child of `meta` of type `kind`; `meta` is a full box, so its children start at 4.
    fn meta_child(&self, kind: &[u8; 4]) -> Option<&[u8]> {
        child(self.meta.as_deref()?.get(4..)?, kind)
    }

    /// The item properties in `iprp`'s `ipco`, as (type, payload).
    fn properties(&self) -> impl Iterator<Item = (&[u8], &[u8])> {
        self.meta_child(b"iprp")
            .and_then(|iprp| child(iprp, b"ipco"))
            .into_iter()
            .flat_map(boxes)
    }
}

/**
 * Whether every extent `iloc` places in this file (construction method 0, data reference 0)
 * ends within its `len` bytes; an extent length of 0 means "to the end". `None` when the box
 * is malformed.
 */
fn extents_fit(iloc: &[u8], len: u64) -> Option<bool> {
    let mut fields = Fields(iloc);
    let version = fields.u8()?;
    fields.take(3)?;
    let sizes = fields.u8()?;
    let (offset_size, length_size) = (sizes >> 4, sizes & 0x0F);
    let sizes = fields.u8()?;
    let base_offset_size = sizes >> 4;
    let has_method = matches!(version, 1 | 2);
    let index_size = if has_method { sizes & 0x0F } else { 0 };
    let items = if version < 2 {
        u32::from(fields.u16()?)
    } else {
        fields.u32()?
    };
    for _ in 0..items {
        fields.take(if version < 2 { 2 } else { 4 })?;
        let method = if has_method { fields.u16()? & 0x0F } else { 0 };
        let data_reference = fields.u16()?;
        let base = fields.sized(base_offset_size)?;
        for _ in 0..fields.u16()? {
            fields.sized(index_size)?;
            let offset = fields.sized(offset_size)?;
            let length = fields.sized(length_size)?;
            if method == 0
                && data_reference == 0
                && base.checked_add(offset)?.checked_add(length)? > len
            {
                return Some(false);
            }
        }
    }
    Some(true)
}

/// Big-endian fields read off the front of a box's payload.
struct Fields<'a>(&'a [u8]);

impl<'a> Fields<'a> {
    fn take(&mut self, count: usize) -> Option<&'a [u8]> {
        let (taken, rest) = self.0.split_at_checked(count)?;
        self.0 = rest;
        Some(taken)
    }

    fn u8(&mut self) -> Option<u8> {
        Some(self.take(1)?[0])
    }

    fn u16(&mut self) -> Option<u16> {
        Some(u16::from_be_bytes(self.take(2)?.try_into().ok()?))
    }

    fn u32(&mut self) -> Option<u32> {
        Some(u32::from_be_bytes(self.take(4)?.try_into().ok()?))
    }

    /// An `iloc` field of 0, 4 or 8 bytes.
    fn sized(&mut self, size: u8) -> Option<u64> {
        match size {
            0 => Some(0),
            4 => self.u32().map(u64::from),
            8 => Some(u64::from_be_bytes(self.take(8)?.try_into().ok()?)),
            _ => None,
        }
    }
}

/// The payload of the first child box of type `kind` in `data`.
fn child<'a>(data: &'a [u8], kind: &[u8; 4]) -> Option<&'a [u8]> {
    boxes(data).find_map(|(found, payload)| (found == kind).then_some(payload))
}

/// The boxes laid end to end in `data`, as (type, payload); ends at the first malformed one.
fn boxes(mut data: &[u8]) -> impl Iterator<Item = (&[u8], &[u8])> {
    std::iter::from_fn(move || {
        let size = u32::from_be_bytes(data.get(0..4)?.try_into().ok()?);
        let kind = data.get(4..8)?;
        let (header_len, size) = match size {
            0 => (8, data.len()),
            1 => (
                16,
                usize::try_from(u64::from_be_bytes(data.get(8..16)?.try_into().ok()?)).ok()?,
            ),
            size => (8, size as usize),
        };
        let payload = data.get(header_len..size)?;
        data = &data[size..];
        Some((kind, payload))
    })
}

#[cfg(test)]
mod tests {
    use std::fs::File;
    use std::io::Cursor;
    use std::path::Path;

    use super::*;

    fn fixture(name: &str) -> Vec<u8> {
        std::fs::read(
            Path::new(env!("CARGO_MANIFEST_DIR"))
                .join("tests/fixtures")
                .join(name),
        )
        .unwrap()
    }

    fn read(bytes: &[u8]) -> Boxes {
        Boxes::read(Cursor::new(bytes)).unwrap()
    }

    #[test]
    fn reads_the_display_p3_profile() {
        let profile = read(&fixture("p3-64.heic")).icc_profile().unwrap();
        /* An ICC header: its size first, then "acsp" at 36. */
        assert_eq!(
            u32::from_be_bytes(profile[0..4].try_into().unwrap()) as usize,
            profile.len()
        );
        assert_eq!(&profile[36..40], b"acsp");
        /* Its `mluc` description, in UTF-16BE. */
        let name: Vec<u8> = "Display P3"
            .encode_utf16()
            .flat_map(u16::to_be_bytes)
            .collect();
        assert!(profile.windows(name.len()).any(|window| window == name));
    }

    #[test]
    fn nclx_only_files_have_none() {
        assert_eq!(read(&fixture("still.heic")).icc_profile(), None);
        assert_eq!(read(&fixture("still.avif")).icc_profile(), None);
    }

    #[test]
    fn whole_files_are_complete_and_cut_ones_are_not() {
        for name in ["still.heic", "p3-64.heic", "still.avif"] {
            let bytes = fixture(name);
            let whole = read(&bytes);
            assert!(whole.is_complete(), "{name}");
            assert!(!whole.has_alpha(), "{name} is opaque");
            for percent in [10, 50, 90, 99] {
                let cut = &bytes[..bytes.len() * percent / 100];
                assert!(!read(cut).is_complete(), "{name} cut at {percent} %");
            }
        }
        let file =
            File::open(Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/still.heic"))
                .unwrap();
        assert!(Boxes::read(file).unwrap().is_complete());
    }

    #[test]
    fn iloc_extents_past_the_end_are_incomplete() {
        /* Version 0, 4-byte offsets and lengths, no base offset: one item, one extent. */
        let iloc = |offset: u32, length: u32| {
            let mut payload = vec![0, 0, 0, 0, 0x44, 0x00, 0, 1, 0, 1, 0, 0, 0, 1];
            payload.extend_from_slice(&offset.to_be_bytes());
            payload.extend_from_slice(&length.to_be_bytes());
            payload
        };
        assert_eq!(extents_fit(&iloc(100, 50), 150), Some(true));
        assert_eq!(extents_fit(&iloc(100, 51), 150), Some(false));
        assert_eq!(extents_fit(&iloc(100, 0), 150), Some(true), "to the end");
        assert_eq!(extents_fit(&iloc(100, 50)[..10], 150), None, "malformed");
    }

    #[test]
    fn an_alpha_plane_is_found() {
        let boxed = |kind: &[u8; 4], payload: &[u8]| {
            let mut bytes = ((payload.len() + 8) as u32).to_be_bytes().to_vec();
            bytes.extend_from_slice(kind);
            bytes.extend_from_slice(payload);
            bytes
        };
        let mut aux = vec![0, 0, 0, 0];
        aux.extend_from_slice(ALPHA_AUX_TYPES[0]);
        aux.push(0);
        let ipco = boxed(b"ipco", &boxed(b"auxC", &aux));
        let mut meta = vec![0, 0, 0, 0];
        meta.extend(boxed(b"iprp", &ipco));
        let file = boxed(b"meta", &meta);
        let boxes = read(&file);
        assert!(boxes.has_alpha());
        assert!(boxes.is_complete());
    }

    #[test]
    fn tolerates_files_without_meta() {
        let empty = read(&[]);
        assert!(empty.is_complete() && empty.icc_profile().is_none());
        let mut ftyp_only = 16u32.to_be_bytes().to_vec();
        ftyp_only.extend_from_slice(b"ftypheic\0\0\0\0");
        assert_eq!(read(&ftyp_only).icc_profile(), None);
        /* A box shorter than its header is the decoder's to judge. */
        assert!(read(b"\0\0\0\x04meta").is_complete());
    }
}
