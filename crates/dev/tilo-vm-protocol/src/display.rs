//! The virtual monitors of a headful slot: `tilo-vm slot create --headful
//! --outputs 2560x1600@2,1920x1080`.
//!
//! Each output becomes one vkms connector on the slot's card. Like a monitor
//! without an EDID, each connector offers the standard VESA (DMT) modes at
//! 60 Hz, 1024x768 preferred; the requested mode and scale reach the
//! compositor through the display settings every user goes through (a
//! confirmed display change, remembered for that set of monitors).

use std::fmt;
use std::str::FromStr;

use serde::{Deserialize, Serialize};

/// More outputs than this is a typo, not a test setup.
pub const MAX_OUTPUTS: usize = 8;
/// No mode vkms can offer is larger than this in either dimension.
pub const MAX_EXTENT: u32 = 8192;
/// The scale range the display settings accept.
pub const SCALE_RANGE: std::ops::RangeInclusive<f64> = 0.5..=4.0;

/// One output: its mode in pixels and its scale.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
#[serde(try_from = "String", into = "String")]
pub struct OutputSpec {
    pub width: u32,
    pub height: u32,
    pub scale: f64,
}

// The scale is always finite: parsing (the only way in) rejects anything else.
impl Eq for OutputSpec {}

impl fmt::Display for OutputSpec {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}x{}@{}", self.width, self.height, self.scale)
    }
}

impl From<OutputSpec> for String {
    fn from(spec: OutputSpec) -> Self {
        spec.to_string()
    }
}

impl TryFrom<String> for OutputSpec {
    type Error = String;

    fn try_from(text: String) -> Result<Self, Self::Error> {
        text.parse()
    }
}

impl FromStr for OutputSpec {
    type Err = String;

    /// `WIDTHxHEIGHT` or `WIDTHxHEIGHT@SCALE`, e.g. `1920x1080`, `2560x1600@2`.
    fn from_str(text: &str) -> Result<Self, Self::Err> {
        let text = text.trim();
        let shape = || format!("'{text}' is not WIDTHxHEIGHT[@SCALE] (e.g. 2560x1600@2)");
        let (mode, scale) = match text.split_once('@') {
            Some((mode, scale)) => (mode, Some(scale)),
            None => (text, None),
        };
        let (width, height) = mode.split_once(['x', 'X']).ok_or_else(shape)?;
        let extent = |value: &str| {
            value
                .parse::<u32>()
                .ok()
                .filter(|v| (1..=MAX_EXTENT).contains(v))
                .ok_or_else(|| {
                    format!("'{text}': {value} is not a pixel size between 1 and {MAX_EXTENT}")
                })
        };
        let width = extent(width)?;
        let height = extent(height)?;
        let scale = match scale {
            None => 1.0,
            Some(scale) => scale
                .parse::<f64>()
                .ok()
                .filter(|s| s.is_finite() && SCALE_RANGE.contains(s))
                .ok_or_else(|| {
                    format!(
                        "'{text}': scale {scale} is outside {} to {}",
                        SCALE_RANGE.start(),
                        SCALE_RANGE.end()
                    )
                })?,
        };
        Ok(Self {
            width,
            height,
            scale,
        })
    }
}

/// A comma-separated list of outputs, left to right, e.g.
/// `2560x1600@2,1920x1080@1`.
///
/// # Errors
/// A sentence naming the first output that does not parse, or a list that
/// is empty or longer than [`MAX_OUTPUTS`].
pub fn parse_outputs(text: &str) -> Result<Vec<OutputSpec>, String> {
    let outputs = text
        .split(',')
        .map(str::parse)
        .collect::<Result<Vec<OutputSpec>, String>>()?;
    validate_outputs(&outputs)?;
    Ok(outputs)
}

/// The list as `parse_outputs` reads it.
pub fn format_outputs(outputs: &[OutputSpec]) -> String {
    outputs
        .iter()
        .map(ToString::to_string)
        .collect::<Vec<_>>()
        .join(",")
}

/// # Errors
/// An empty list or more than [`MAX_OUTPUTS`] outputs.
pub fn validate_outputs(outputs: &[OutputSpec]) -> Result<(), String> {
    if outputs.is_empty() {
        return Err("no outputs given".into());
    }
    if outputs.len() > MAX_OUTPUTS {
        return Err(format!(
            "{} outputs asked for; a slot has at most {MAX_OUTPUTS}",
            outputs.len()
        ));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn outputs_parse_left_to_right_with_default_scale() {
        let outputs = parse_outputs("2560x1600@2,1920x1080").unwrap();
        assert_eq!(
            outputs,
            [
                OutputSpec {
                    width: 2560,
                    height: 1600,
                    scale: 2.0
                },
                OutputSpec {
                    width: 1920,
                    height: 1080,
                    scale: 1.0
                },
            ]
        );
        assert_eq!(format_outputs(&outputs), "2560x1600@2,1920x1080@1");
        assert!((parse_outputs("1280X720@1.25").unwrap()[0].scale - 1.25).abs() < f64::EPSILON);
    }

    #[test]
    fn bad_outputs_name_the_problem() {
        for (text, needle) in [
            ("2560", "WIDTHxHEIGHT"),
            ("0x600", "between 1 and 8192"),
            ("9000x600", "between 1 and 8192"),
            ("800x600@5", "outside 0.5 to 4"),
            ("800x600@NaN", "outside"),
            ("800x600,", "WIDTHxHEIGHT"),
            ("", "WIDTHxHEIGHT"),
        ] {
            let error = parse_outputs(text).unwrap_err();
            assert!(error.contains(needle), "{text}: {error}");
        }
        let nine = ["800x600"; 9].join(",");
        assert!(parse_outputs(&nine).unwrap_err().contains("at most 8"));
    }

    #[test]
    fn an_output_travels_as_its_spec_string() {
        let spec: OutputSpec = "1920x1080@1.5".parse().unwrap();
        let json = serde_json::to_string(&spec).unwrap();
        assert_eq!(json, r#""1920x1080@1.5""#);
        assert_eq!(serde_json::from_str::<OutputSpec>(&json).unwrap(), spec);
        assert!(serde_json::from_str::<OutputSpec>(r#""1920x1080@9""#).is_err());
    }
}
