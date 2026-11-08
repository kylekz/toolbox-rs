//! A still's container, walked before it is decoded (docs/stills.md "Cut-off files"): is the
//! whole picture in the file, and could a fully transparent one be what it holds? The OS
//! decoders hand back a cut-off file's picture with its missing part grey, black or
//! transparent, so a file still being copied, or damaged, would show blank or half drawn. The
//! walks read the file once, from the OS's cache the decoder then reads again, and never decode.
//!
//! - JPEG: the markers, through the entropy-coded data, up to an EOI. What follows it (a
//!   Motion Photo's video, a maker's trailer) is never read.
//! - PNG: the chunks up to `IEND`. GIF: the blocks up to the trailer.
//! - BMP and WebP: the file holds the size its header declares.
//! - HEIF and AVIF: every top-level box and `iloc` extent ends within the file
//!   ([`crate::heif`]).
//! - TIFF: every strip or tile of the first directory ends within the file. ImageIO refuses a
//!   cut-off TIFF itself; WIC refuses most, but not one whose picture is a single strip, which
//!   it hands back with the missing rows black.

use std::io::{self, BufRead, Read, Seek, SeekFrom};

use crate::heif::Boxes;
use crate::sniff::{ImageFormat, gif_color_table_len};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Container {
    /// The file ends before the picture does.
    Truncated,
    /**
     * The picture is all there. `alpha` says whether the format, or this file, can hold
     * transparent pixels: JPEG and BMP can't, nor a HEIF or AVIF without an alpha plane.
     */
    Complete { alpha: bool },
}

/// Walks `file`, a still of `format`, from its start.
pub(crate) fn inspect(
    file: &mut (impl BufRead + Seek),
    format: ImageFormat,
) -> io::Result<Container> {
    let len = file.seek(SeekFrom::End(0))?;
    file.rewind()?;
    let (complete, alpha) = match format {
        ImageFormat::Jpeg => (jpeg(file), false),
        ImageFormat::Png => (png(file), true),
        ImageFormat::Gif => (gif(file), true),
        ImageFormat::Bmp => (declared_size_fits(file, len, 2..6, 0), false),
        /* RIFF's size counts what follows its 8-byte header. */
        ImageFormat::Webp => (declared_size_fits(file, len, 4..8, 8), true),
        ImageFormat::Heif | ImageFormat::Avif => {
            let boxes = Boxes::read(file)?;
            (Ok(boxes.is_complete()), boxes.has_alpha())
        }
        ImageFormat::Tiff => (tiff(file, len), true),
    };
    let complete = match complete {
        Err(error) if error.kind() == io::ErrorKind::UnexpectedEof => false,
        result => result?,
    };
    Ok(if complete {
        Container::Complete { alpha }
    } else {
        Container::Truncated
    })
}

/**
 * Whether a JPEG reaches an EOI marker. Segments are skipped by their length; between them,
 * and through a scan's entropy-coded data, the next marker is found by its `FF` (`FF 00` is a
 * stuffed byte, repeated `FF`s are fill).
 */
fn jpeg(file: &mut impl BufRead) -> io::Result<bool> {
    skip(file, 2)?;
    while let Some(marker) = next_jpeg_marker(file)? {
        match marker {
            0xD9 => return Ok(true),
            /* Standalone: TEM, the restart markers and SOI. */
            0x01 | 0xD0..=0xD8 => {}
            _ => {
                let mut len = [0u8; 2];
                file.read_exact(&mut len)?;
                skip(file, u64::from(u16::from_be_bytes(len)).saturating_sub(2))?;
            }
        }
    }
    Ok(false)
}

/// The code of the next JPEG marker in `file`, or `None` at its end.
fn next_jpeg_marker(file: &mut impl BufRead) -> io::Result<Option<u8>> {
    let mut after_ff = false;
    loop {
        let buffer = file.fill_buf()?;
        if buffer.is_empty() {
            return Ok(None);
        }
        let mut used = 0;
        let mut found = None;
        while used < buffer.len() {
            if after_ff {
                match buffer[used] {
                    0xFF => {}
                    0x00 => after_ff = false,
                    code => found = Some(code),
                }
                used += 1;
                if found.is_some() {
                    break;
                }
            } else {
                match buffer[used..].iter().position(|byte| *byte == 0xFF) {
                    Some(at) => {
                        used += at + 1;
                        after_ff = true;
                    }
                    None => used = buffer.len(),
                }
            }
        }
        file.consume(used);
        if found.is_some() {
            return Ok(found);
        }
    }
}

/// Whether a PNG's chunks reach `IEND`, its CRC included.
fn png(file: &mut impl Read) -> io::Result<bool> {
    skip(file, 8)?;
    loop {
        let mut header = [0u8; 8];
        file.read_exact(&mut header)?;
        let (len, kind) = header.split_at(4);
        /* The data, then the CRC. */
        skip(
            file,
            u64::from(u32::from_be_bytes([len[0], len[1], len[2], len[3]])) + 4,
        )?;
        if kind == b"IEND" {
            return Ok(true);
        }
    }
}

/**
 * Whether a GIF's blocks reach the trailer (`3B`). A stray byte where a block should start
 * ends the picture as decoders do, after the frames before it.
 */
fn gif(file: &mut impl Read) -> io::Result<bool> {
    /* Header (6) and logical screen descriptor (7), then the global colour table. */
    let mut header = [0u8; 13];
    file.read_exact(&mut header)?;
    skip(file, gif_color_table_len(header[10]) as u64)?;
    loop {
        match byte(file)? {
            /* An extension: its label, then sub-blocks. */
            0x21 => {
                skip(file, 1)?;
                skip_gif_sub_blocks(file)?;
            }
            0x2C => {
                let mut descriptor = [0u8; 9];
                file.read_exact(&mut descriptor)?;
                /* The local colour table and the LZW code size, then the image data. */
                skip(file, gif_color_table_len(descriptor[8]) as u64 + 1)?;
                skip_gif_sub_blocks(file)?;
            }
            _ => return Ok(true),
        }
    }
}

fn skip_gif_sub_blocks(file: &mut impl Read) -> io::Result<()> {
    loop {
        match byte(file)? {
            0 => return Ok(()),
            len => skip(file, u64::from(len))?,
        }
    }
}

/**
 * Whether the file is at least as long as the little-endian `u32` at `field` plus `extra`
 * declares; a declared size of 0 (some BMP writers) says nothing.
 */
fn declared_size_fits(
    file: &mut impl Read,
    len: u64,
    field: std::ops::Range<usize>,
    extra: u64,
) -> io::Result<bool> {
    let mut header = [0u8; 8];
    file.read_exact(&mut header[..field.end])?;
    let mut size = [0u8; 4];
    size.copy_from_slice(&header[field]);
    let declared = u64::from(u32::from_le_bytes(size));
    Ok(declared == 0 || len >= declared + extra)
}

/**
 * Whether a TIFF's first directory, the picture the decoders show, has every strip or tile
 * within the file's `len` bytes: classic TIFF and BigTIFF, either byte order. WIC hands back a
 * picture cut short inside a strip with its missing rows black. A directory without strips or
 * tiles is left to the decoder.
 */
fn tiff(file: &mut (impl Read + Seek), len: u64) -> io::Result<bool> {
    let mut header = [0u8; 8];
    file.read_exact(&mut header)?;
    let big_endian = match &header[..2] {
        b"II" => false,
        b"MM" => true,
        _ => return Ok(true),
    };
    let number = |bytes: &[u8]| -> u64 {
        let fold = |value: u64, byte: &u8| value << 8 | u64::from(*byte);
        if big_endian {
            bytes.iter().fold(0, fold)
        } else {
            bytes.iter().rev().fold(0, fold)
        }
    };
    /* Classic TIFF: 42, a 4-byte directory offset and 12-byte entries. BigTIFF: 43, then 8. */
    let big = match number(&header[2..4]) {
        42 => false,
        43 => true,
        _ => return Ok(true),
    };
    let directory = if big {
        let mut offset = [0u8; 8];
        file.read_exact(&mut offset)?;
        number(&offset)
    } else {
        number(&header[4..8])
    };
    let (count_size, entry_size, field) = if big { (8, 20, 8) } else { (2, 12, 4) };
    file.seek(SeekFrom::Start(directory))?;
    let mut count = [0u8; 8];
    file.read_exact(&mut count[..count_size])?;
    let mut entries = vec![0u8; entry_size * number(&count[..count_size]) as usize];
    file.read_exact(&mut entries)?;
    /* Each tag's values: inline when they fit the entry's field, else at its offset. */
    let mut values = |tag: u16| -> io::Result<Option<Vec<u64>>> {
        let Some(entry) = entries
            .chunks_exact(entry_size)
            .find(|entry| number(&entry[..2]) == u64::from(tag))
        else {
            return Ok(None);
        };
        let size = match number(&entry[2..4]) {
            3 => 2,
            4 => 4,
            16 => 8,
            _ => return Ok(None),
        };
        let count = number(&entry[4..4 + field]) as usize;
        let at = &entry[4 + field..4 + 2 * field];
        let bytes = if count * size <= field {
            at[..count * size].to_vec()
        } else {
            file.seek(SeekFrom::Start(number(at)))?;
            let mut bytes = vec![0u8; count * size];
            file.read_exact(&mut bytes)?;
            bytes
        };
        Ok(Some(bytes.chunks_exact(size).map(number).collect()))
    };
    /* StripOffsets and StripByteCounts, else TileOffsets and TileByteCounts. */
    let pieces = match (values(273)?, values(279)?) {
        (Some(offsets), Some(counts)) => Some((offsets, counts)),
        _ => values(324)?.zip(values(325)?),
    };
    let Some((offsets, counts)) = pieces else {
        return Ok(true);
    };
    Ok(offsets
        .iter()
        .zip(&counts)
        .all(|(offset, count)| offset.saturating_add(*count) <= len))
}

fn byte(file: &mut impl Read) -> io::Result<u8> {
    let mut byte = [0u8; 1];
    file.read_exact(&mut byte)?;
    Ok(byte[0])
}

/// Reads past `count` bytes; `UnexpectedEof` when the file ends first.
fn skip(file: &mut impl Read, count: u64) -> io::Result<()> {
    if io::copy(&mut file.take(count), &mut io::sink())? < count {
        return Err(io::ErrorKind::UnexpectedEof.into());
    }
    Ok(())
}

#[cfg(test)]
pub(crate) mod tests {
    use std::io::Cursor;
    use std::path::Path;

    use pretty_assertions::assert_eq;

    use super::*;

    fn fixture(name: &str) -> Vec<u8> {
        std::fs::read(
            Path::new(env!("CARGO_MANIFEST_DIR"))
                .join("tests/fixtures")
                .join(name),
        )
        .unwrap()
    }

    fn check(bytes: &[u8], format: ImageFormat) -> Container {
        inspect(&mut Cursor::new(bytes), format).unwrap()
    }

    #[test]
    fn whole_files_are_complete_and_cut_ones_are_truncated() {
        for (name, format, alpha) in [
            ("still-1920x1080.jpg", ImageFormat::Jpeg, false),
            ("orient6-160x90.jpg", ImageFormat::Jpeg, false),
            ("still-1080x1350.png", ImageFormat::Png, true),
            ("anim-160x90.png", ImageFormat::Png, true),
            ("still-160x90.gif", ImageFormat::Gif, true),
            ("anim-320x180.gif", ImageFormat::Gif, true),
            ("still-160x90.webp", ImageFormat::Webp, true),
            ("still.heic", ImageFormat::Heif, false),
            ("still.avif", ImageFormat::Avif, false),
        ] {
            let bytes = fixture(name);
            assert_eq!(
                check(&bytes, format),
                Container::Complete { alpha },
                "{name}"
            );
            for percent in [10, 50, 90] {
                let cut = &bytes[..bytes.len() * percent / 100];
                assert_eq!(
                    check(cut, format),
                    Container::Truncated,
                    "{name} cut at {percent} %"
                );
            }
        }
    }

    #[test]
    fn a_jpeg_may_carry_data_after_its_eoi() {
        let mut bytes = fixture("still-1920x1080.jpg");
        /* A Motion Photo's video, or padding. */
        bytes.extend_from_slice(b"\0\0\0\x18ftypmp42 and the rest of a video");
        bytes.extend_from_slice(&[0; 4096]);
        assert_eq!(
            check(&bytes, ImageFormat::Jpeg),
            Container::Complete { alpha: false }
        );
    }

    #[test]
    fn a_bmp_holds_its_declared_size() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("a.bmp");
        image::RgbImage::from_pixel(40, 30, image::Rgb([10, 20, 30]))
            .save(&path)
            .unwrap();
        let bytes = std::fs::read(&path).unwrap();
        assert_eq!(
            check(&bytes, ImageFormat::Bmp),
            Container::Complete { alpha: false }
        );
        for percent in [10, 50, 90] {
            let cut = &bytes[..bytes.len() * percent / 100];
            assert_eq!(check(cut, ImageFormat::Bmp), Container::Truncated);
        }
        let mut undeclared = bytes.clone();
        undeclared[2..6].fill(0);
        undeclared.truncate(bytes.len() / 2);
        assert_eq!(
            check(&undeclared, ImageFormat::Bmp),
            Container::Complete { alpha: false },
            "nothing declared, nothing to check"
        );
    }

    /**
     * An uncompressed `w`×`h` RGB TIFF with its directory first, so a cut loses only pixels, in
     * strips of `rows` rows (1, or `h` for the whole picture in one), either byte order.
     */
    pub(crate) fn directory_first_tiff(w: u16, h: u16, rows: u16, big_endian: bool) -> Vec<u8> {
        let put = |tiff: &mut Vec<u8>, value: u32, size: usize| {
            let bytes = value.to_be_bytes();
            let bytes = &bytes[4 - size..];
            if big_endian {
                tiff.extend(bytes);
            } else {
                tiff.extend(bytes.iter().rev());
            }
        };
        let row = u32::from(w) * 3;
        let strips = u32::from(h.div_ceil(rows));
        let strip = |index: u32| row * u32::from(rows).min(u32::from(h) - index * u32::from(rows));
        let entries: u32 = 9;
        let arrays = 8 + 2 + entries * 12 + 4;
        let (bits_at, offsets_at) = (arrays, arrays + 6);
        let counts_at = offsets_at + 4 * strips;
        let data_at = counts_at + 4 * strips;
        let mut tiff = if big_endian {
            b"MM".to_vec()
        } else {
            b"II".to_vec()
        };
        put(&mut tiff, 42, 2);
        put(&mut tiff, 8, 4);
        put(&mut tiff, entries, 2);
        /* A single strip's offset and length are the entries' own values. */
        let (offsets, counts) = if strips == 1 {
            (data_at, strip(0))
        } else {
            (offsets_at, counts_at)
        };
        /* (tag, type, count, value): type 3 is SHORT, 4 LONG. */
        for (tag, kind, count, value) in [
            (256, 3, 1, u32::from(w)),
            (257, 3, 1, u32::from(h)),
            (258, 3, 3, bits_at),
            (259, 3, 1, 1),
            (262, 3, 1, 2),
            (273, 4, strips, offsets),
            (277, 3, 1, 3),
            (278, 3, 1, u32::from(rows)),
            (279, 4, strips, counts),
        ] {
            put(&mut tiff, tag, 2);
            put(&mut tiff, kind, 2);
            put(&mut tiff, count, 4);
            /* A SHORT value sits in the field's first two bytes. */
            if kind == 3 && count == 1 {
                put(&mut tiff, value, 2);
                put(&mut tiff, 0, 2);
            } else {
                put(&mut tiff, value, 4);
            }
        }
        put(&mut tiff, 0, 4);
        (0..3).for_each(|_| put(&mut tiff, 8, 2));
        let mut at = data_at;
        for index in 0..strips {
            put(&mut tiff, at, 4);
            at += strip(index);
        }
        (0..strips).for_each(|index| put(&mut tiff, strip(index), 4));
        tiff.extend((0..row * u32::from(h)).map(|i| (i % 251) as u8));
        tiff
    }

    #[test]
    fn a_tiff_holds_every_strip_of_its_first_directory() {
        for (rows, big_endian) in [(1, false), (90, false), (1, true), (90, true)] {
            let bytes = directory_first_tiff(160, 90, rows, big_endian);
            let name = format!("{rows}-row strips, big-endian {big_endian}");
            assert_eq!(
                check(&bytes, ImageFormat::Tiff),
                Container::Complete { alpha: true },
                "{name}"
            );
            for percent in [10, 50, 90, 99] {
                let cut = &bytes[..bytes.len() * percent / 100];
                assert_eq!(
                    check(cut, ImageFormat::Tiff),
                    Container::Truncated,
                    "{name} cut at {percent} %"
                );
            }
        }
        /* Not a TIFF this walk knows: left to the decoder. */
        assert_eq!(
            check(b"II\x2c\x00\x08\x00\x00\x00", ImageFormat::Tiff),
            Container::Complete { alpha: true }
        );
    }
}
