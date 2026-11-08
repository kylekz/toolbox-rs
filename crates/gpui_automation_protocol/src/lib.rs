//! The gpui-free half of automation (docs/automation.md): the wire format ([`protocol`]), the
//! JSON-pointer expectations shared by the in-app `wait_for`/`assert` commands and the scenario
//! runner ([`matcher`]), and the builtin commands' arguments ([`commands`]).
//!
//! gpui: **never**. The scenario runner (`gpui_automation_runner`) builds on this crate alone, so
//! building it never compiles gpui.

pub mod commands;
pub mod matcher;
pub mod protocol;

use anyhow::{Context as _, Result, bail};

/// Parse a `--window-size` value such as `1440x900` into logical pixels.
pub fn parse_window_size(spec: &str) -> Result<(f32, f32)> {
    let (w, h) = spec
        .trim()
        .split_once(['x', 'X'])
        .with_context(|| format!("window size {spec:?} is not WIDTHxHEIGHT"))?;
    let parse = |part: &str| -> Result<f32> {
        let value: f32 = part
            .trim()
            .parse()
            .with_context(|| format!("window size {spec:?} is not WIDTHxHEIGHT"))?;
        if !(value.is_finite() && value >= 1.0) {
            bail!("window size {spec:?} must be positive");
        }
        Ok(value)
    };
    Ok((parse(w)?, parse(h)?))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn window_sizes_parse() {
        assert_eq!(parse_window_size("1440x900").unwrap(), (1440.0, 900.0));
        assert_eq!(parse_window_size(" 1080X1920 ").unwrap(), (1080.0, 1920.0));
        assert!(parse_window_size("1440").is_err());
        assert!(parse_window_size("0x900").is_err());
        assert!(parse_window_size("wide x tall").is_err());
    }
}
