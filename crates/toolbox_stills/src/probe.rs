//! The startup codec probe and decoder warm-up (docs/stills.md "Missing codecs"): a tiny
//! embedded HEIC and AVIF decoded through the OS decoder. The first HEIC/AVIF decode in a
//! process costs 50–200 ms extra, which the probe pays at startup instead of the first picture.
//!
//! On Windows the codecs are Store extensions: a failed probe tells the app which are missing,
//! and makes a later HEIC/AVIF failure [`crate::DecodeError::MissingCodec`]. On macOS ImageIO
//! always has both, and the probe is the warm-up. Elsewhere neither decodes.

use std::sync::OnceLock;

use crate::decode::{Codec, Source, os};
use crate::sniff::ImageFormat;

/// 64 px samples (sips for HEIC, SVT-AV1 for AVIF; `tests/fixtures/README.md`).
const PROBE_HEIC: &[u8] = include_bytes!("../assets/probe.heic");
const PROBE_AVIF: &[u8] = include_bytes!("../assets/probe.avif");

/// Which Store codecs decode on this machine.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Codecs {
    pub heic: bool,
    pub avif: bool,
}

impl Codecs {
    pub fn supports(self, codec: Codec) -> bool {
        match codec {
            Codec::Heic => self.heic,
            Codec::Avif => self.avif,
        }
    }
}

/**
 * Probes the HEIC and AVIF decoders, once per process; later calls return the first result.
 * Blocking for up to a few hundred milliseconds the first time: call it on a background
 * thread at startup. A decode that needs the answer first waits for it.
 */
pub fn probe_codecs() -> Codecs {
    static CODECS: OnceLock<Codecs> = OnceLock::new();
    /*
     * On a thread of its own, which on Windows joins COM's multithreaded apartment for WIC; the
     * caller's thread (an executor's, say) is left as it was.
     */
    *CODECS.get_or_init(|| {
        let probe = || Codecs {
            heic: decodes(Codec::Heic, ImageFormat::Heif, PROBE_HEIC),
            avif: decodes(Codec::Avif, ImageFormat::Avif, PROBE_AVIF),
        };
        std::thread::scope(|scope| match scope.spawn(probe).join() {
            Ok(codecs) => codecs,
            Err(panic) => std::panic::resume_unwind(panic),
        })
    })
}

fn decodes(codec: Codec, format: ImageFormat, sample: &'static [u8]) -> bool {
    match os::decode(Source::Bytes(sample), format, (64, 64)) {
        Ok(_) => true,
        Err(error) => {
            log::warn!("{codec} doesn't decode: {error}");
            false
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(any(target_os = "macos", windows))]
    #[test]
    fn the_samples_decode_at_their_size() {
        for (format, sample) in [
            (ImageFormat::Heif, PROBE_HEIC),
            (ImageFormat::Avif, PROBE_AVIF),
        ] {
            let fitted = os::decode(Source::Bytes(sample), format, (64, 64)).unwrap();
            assert_eq!(fitted.source, (64, 64));
            let image = fitted.image;
            assert_eq!((image.width(), image.height()), (64, 64));
            assert_eq!(image.byte_size(), 64 * 64 * 4);
        }
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn imageio_has_both_codecs() {
        let codecs = probe_codecs();
        assert_eq!(
            codecs,
            Codecs {
                heic: true,
                avif: true
            }
        );
        assert!(codecs.supports(Codec::Heic) && codecs.supports(Codec::Avif));
        assert_eq!(probe_codecs(), codecs, "probed once");
    }

    #[cfg(not(any(target_os = "macos", windows)))]
    #[test]
    fn nothing_decodes_without_an_os_decoder() {
        assert_eq!(
            probe_codecs(),
            Codecs {
                heic: false,
                avif: false
            }
        );
    }
}
