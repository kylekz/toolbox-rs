//! X11 window capture for Linux.
//!
//! A core-protocol `GetImage` of the window over a fresh connection to
//! `$DISPLAY` (gpui's own connection belongs to the main thread). With no
//! compositing manager, as under Xvfb in a GPU-less cloud container, that is
//! exactly the frame gpui presented. Under a compositor, or while another
//! window covers it, the covered parts are undefined, as the protocol says.
//!
//! Sizes are physical pixels. X11 has no per-window scale, so on an unscaled
//! display a 1440x900 logical window captures as 1440x900.

use anyhow::{Context as _, Result, bail};
use x11rb::connection::Connection as _;
use x11rb::protocol::xproto::{
    ConnectionExt as _, ImageFormat, ImageOrder, MapState, Setup, VisualClass, Visualid,
};

/// Tightly packed 8-bit RGBA pixels, alpha forced opaque.
pub(crate) struct RgbaImage {
    pub width: u32,
    pub height: u32,
    pub rgba: Vec<u8>,
}

/// Capture the X11 window `window`.
pub(crate) fn capture_window(window: u32) -> Result<RgbaImage> {
    let (conn, _) = x11rb::connect(None).context("connecting to the X server on $DISPLAY")?;
    let attributes = conn
        .get_window_attributes(window)?
        .reply()
        .context("the window no longer exists")?;
    if attributes.map_state != MapState::VIEWABLE {
        bail!("the window is not viewable; is it minimised or hidden?");
    }
    let geometry = conn.get_geometry(window)?.reply()?;
    let (w, h) = (geometry.width, geometry.height);
    if w == 0 || h == 0 {
        bail!("the window has an empty client area ({w}x{h})");
    }
    let image = conn
        .get_image(ImageFormat::Z_PIXMAP, window, 0, 0, w, h, !0)?
        .reply()
        .context("GetImage failed")?;
    let format = PixelFormat::lookup(conn.setup(), image.depth, image.visual)?;
    let rgba = format.to_rgba(&image.data, w.into(), h.into())?;
    Ok(RgbaImage {
        width: w.into(),
        height: h.into(),
        rgba,
    })
}

/// How a `ZPixmap` image of one depth and visual lays out its pixels.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct PixelFormat {
    bits_per_pixel: u8,
    scanline_pad: u8,
    msb_first: bool,
    red_mask: u32,
    green_mask: u32,
    blue_mask: u32,
}

impl PixelFormat {
    fn lookup(setup: &Setup, depth: u8, visual: Visualid) -> Result<Self> {
        let pixmap = setup
            .pixmap_formats
            .iter()
            .find(|format| format.depth == depth)
            .with_context(|| format!("the X server lists no pixmap format for depth {depth}"))?;
        let visual_type = setup
            .roots
            .iter()
            .flat_map(|screen| &screen.allowed_depths)
            .flat_map(|depth| &depth.visuals)
            .find(|visual_type| visual_type.visual_id == visual)
            .with_context(|| format!("the X server lists no visual {visual:#x}"))?;
        if !matches!(
            visual_type.class,
            VisualClass::TRUE_COLOR | VisualClass::DIRECT_COLOR
        ) {
            bail!("unsupported visual class {:?}", visual_type.class);
        }
        Ok(Self {
            bits_per_pixel: pixmap.bits_per_pixel,
            scanline_pad: pixmap.scanline_pad,
            msb_first: setup.image_byte_order == ImageOrder::MSB_FIRST,
            red_mask: visual_type.red_mask,
            green_mask: visual_type.green_mask,
            blue_mask: visual_type.blue_mask,
        })
    }

    /// Convert `data` (`width` x `height` pixels in this format) to RGBA.
    fn to_rgba(self, data: &[u8], width: usize, height: usize) -> Result<Vec<u8>> {
        // gpui picks 24- or 32-bit TrueColor visuals, both stored 32 bits
        // per pixel; anything else is a server we have no reason to support.
        if self.bits_per_pixel != 32 {
            bail!("unsupported {}-bit pixels", self.bits_per_pixel);
        }
        let channel = |mask: u32| -> Result<u32> {
            if mask.count_ones() != 8 || (mask >> mask.trailing_zeros()) != 0xff {
                bail!("unsupported colour mask {mask:#x}");
            }
            Ok(mask.trailing_zeros())
        };
        let shifts = [
            channel(self.red_mask)?,
            channel(self.green_mask)?,
            channel(self.blue_mask)?,
        ];
        let pad = usize::from(self.scanline_pad.max(8)) / 8;
        let stride = (width * 4).div_ceil(pad) * pad;
        if data.len() < stride * height {
            bail!(
                "GetImage returned {} bytes, expected {} for {width}x{height}",
                data.len(),
                stride * height
            );
        }
        let mut rgba = Vec::with_capacity(width * height * 4);
        for row in data.chunks_exact(stride).take(height) {
            for &bytes in row[..width * 4].as_chunks::<4>().0 {
                let pixel = if self.msb_first {
                    u32::from_be_bytes(bytes)
                } else {
                    u32::from_le_bytes(bytes)
                };
                rgba.extend(shifts.map(|shift| (pixel >> shift) as u8));
                // A 32-bit (ARGB) visual carries gpui's window alpha, which
                // can't be flattened into a screenshot, so force it opaque.
                rgba.push(0xff);
            }
        }
        Ok(rgba)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const BGRX: PixelFormat = PixelFormat {
        bits_per_pixel: 32,
        scanline_pad: 32,
        msb_first: false,
        red_mask: 0xff_0000,
        green_mask: 0xff00,
        blue_mask: 0xff,
    };

    #[test]
    fn converts_little_endian_truecolor() {
        // Pixels 0x80112233 and 0x00445566 stored little-endian.
        let data = [0x33, 0x22, 0x11, 0x80, 0x66, 0x55, 0x44, 0x00];
        let rgba = BGRX.to_rgba(&data, 2, 1).unwrap();
        assert_eq!(rgba, [0x11, 0x22, 0x33, 0xff, 0x44, 0x55, 0x66, 0xff]);
    }

    #[test]
    fn converts_big_endian_with_other_masks() {
        let format = PixelFormat {
            msb_first: true,
            red_mask: 0xff,
            blue_mask: 0xff_0000,
            ..BGRX
        };
        let data = [0x00, 0x33, 0x22, 0x11];
        assert_eq!(
            format.to_rgba(&data, 1, 1).unwrap(),
            [0x11, 0x22, 0x33, 0xff]
        );
    }

    #[test]
    fn skips_scanline_padding() {
        let format = PixelFormat {
            scanline_pad: 64,
            ..BGRX
        };
        // One pixel per row, each row padded from 4 to 8 bytes.
        let data = [1, 2, 3, 0, 9, 9, 9, 9, 4, 5, 6, 0, 9, 9, 9, 9];
        assert_eq!(
            format.to_rgba(&data, 1, 2).unwrap(),
            [3, 2, 1, 0xff, 6, 5, 4, 0xff]
        );
    }

    #[test]
    fn rejects_what_it_cannot_decode() {
        let short = BGRX.to_rgba(&[0; 7], 2, 1).unwrap_err();
        assert!(short.to_string().contains("expected 8"), "{short}");
        let rgb565 = PixelFormat {
            bits_per_pixel: 16,
            ..BGRX
        };
        assert!(rgb565.to_rgba(&[0; 2], 1, 1).is_err());
        let wide_mask = PixelFormat {
            red_mask: 0x3ff0_0000,
            ..BGRX
        };
        assert!(wide_mask.to_rgba(&[0; 4], 1, 1).is_err());
    }
}
