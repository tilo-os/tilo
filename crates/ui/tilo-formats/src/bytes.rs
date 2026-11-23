//! Byte counts formatted as text for a person, in SI decimal (`B`, `kB`, `MB`,
//! `GB`) or IEC binary (`B`, `KiB`, `MiB`, `GiB`) units.

/// Base and unit labels for formatting a byte count.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ByteUnits {
    /// SI powers of 1000: `B`, `kB`, `MB`, `GB`.
    Decimal,
    /// IEC powers of 1024: `B`, `KiB`, `MiB`, `GiB`.
    Binary,
}

impl ByteUnits {
    const fn scale(self, unit: ByteUnit) -> u64 {
        match (self, unit) {
            (_, ByteUnit::Byte) => 1,
            (Self::Decimal, ByteUnit::Kilo) => 1_000,
            (Self::Decimal, ByteUnit::Mega) => 1_000_000,
            (Self::Decimal, ByteUnit::Giga) => 1_000_000_000,
            (Self::Binary, ByteUnit::Kilo) => 1_024,
            (Self::Binary, ByteUnit::Mega) => 1_048_576,
            (Self::Binary, ByteUnit::Giga) => 1_073_741_824,
        }
    }

    const fn label(self, unit: ByteUnit) -> &'static str {
        match (self, unit) {
            (_, ByteUnit::Byte) => "B",
            (Self::Decimal, ByteUnit::Kilo) => "kB",
            (Self::Decimal, ByteUnit::Mega) => "MB",
            (Self::Decimal, ByteUnit::Giga) => "GB",
            (Self::Binary, ByteUnit::Kilo) => "KiB",
            (Self::Binary, ByteUnit::Mega) => "MiB",
            (Self::Binary, ByteUnit::Giga) => "GiB",
        }
    }
}

/// Unit tier a byte formatter selects between.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum ByteUnit {
    /// Single bytes (`B`).
    Byte,
    /// Thousands (`kB` or `KiB`).
    Kilo,
    /// Millions (`MB` or `MiB`).
    Mega,
    /// Billions (`GB` or `GiB`).
    Giga,
}

/// How fractional units are reduced when formatting a byte count.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ByteRounding {
    /// Round to the nearest tenth (or whole unit).
    Nearest,
    /// Truncate toward zero.
    Truncate,
}

/// Configuration for turning a byte count into text for a person.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ByteFormat {
    /// SI decimal (1000) or IEC binary (1024) base and labels.
    pub units: ByteUnits,
    /// Smallest unit emitted; values below it stay in this unit.
    pub min_unit: ByteUnit,
    /// First unit emitted with one fractional digit; smaller units print whole numbers.
    pub decimal_from: ByteUnit,
    /// Rounding mode applied when scaling down to a larger unit.
    pub rounding: ByteRounding,
}

impl ByteFormat {
    /// SI decimal units (`B`, `kB`, `MB`, `GB`), whole bytes and one rounded decimal from `kB`.
    #[must_use]
    pub const fn decimal() -> Self {
        Self {
            units: ByteUnits::Decimal,
            min_unit: ByteUnit::Byte,
            decimal_from: ByteUnit::Kilo,
            rounding: ByteRounding::Nearest,
        }
    }

    /// IEC binary units (`B`, `KiB`, `MiB`, `GiB`), whole bytes and one truncated decimal from
    /// `KiB`.
    #[must_use]
    pub const fn binary() -> Self {
        Self {
            units: ByteUnits::Binary,
            min_unit: ByteUnit::Byte,
            decimal_from: ByteUnit::Kilo,
            rounding: ByteRounding::Truncate,
        }
    }

    /// Set the smallest unit emitted.
    #[must_use]
    pub const fn min_unit(mut self, unit: ByteUnit) -> Self {
        self.min_unit = unit;
        self
    }

    /// Set the first unit emitted with one fractional digit.
    #[must_use]
    pub const fn decimal_from(mut self, unit: ByteUnit) -> Self {
        self.decimal_from = unit;
        self
    }

    /// Set the rounding mode for scaled units.
    #[must_use]
    pub const fn rounding(mut self, rounding: ByteRounding) -> Self {
        self.rounding = rounding;
        self
    }

    /// Format `bytes` according to this style.
    #[must_use]
    pub fn format(self, bytes: u64) -> String {
        const ORDER: [ByteUnit; 4] = [
            ByteUnit::Giga,
            ByteUnit::Mega,
            ByteUnit::Kilo,
            ByteUnit::Byte,
        ];
        let unit = ORDER
            .into_iter()
            .find(|&candidate| candidate <= self.min_unit || bytes >= self.units.scale(candidate))
            .unwrap_or(self.min_unit);
        let scale = self.units.scale(unit);
        let label = self.units.label(unit);
        if scale == 1 {
            return format!("{bytes} {label}");
        }
        if unit < self.decimal_from {
            return match self.rounding {
                ByteRounding::Truncate => {
                    let whole = bytes.checked_div(scale).unwrap_or(0);
                    format!("{whole} {label}")
                }
                ByteRounding::Nearest => {
                    #[allow(
                        clippy::cast_precision_loss,
                        reason = "display only, rounded to a whole unit; precision beyond f64's \
                                  52-bit mantissa is unobservable at this scale"
                    )]
                    let value = bytes as f64 / scale as f64;
                    format!("{value:.0} {label}")
                }
            };
        }
        match self.rounding {
            ByteRounding::Nearest => {
                #[allow(
                    clippy::cast_precision_loss,
                    reason = "display only, rounded to one decimal place; precision beyond f64's \
                              52-bit mantissa is unobservable at that rounding and would only \
                              matter for byte counts in the exabyte range"
                )]
                let value = bytes as f64 / scale as f64;
                format!("{value:.1} {label}")
            }
            ByteRounding::Truncate => {
                let whole = bytes.checked_div(scale).unwrap_or(0);
                let remainder = bytes.checked_rem(scale).unwrap_or(0);
                // `remainder < scale <= 2^30`, so `remainder * 10` never overflows `u64`.
                let tenth = remainder.saturating_mul(10).checked_div(scale).unwrap_or(0);
                format!("{whole}.{tenth} {label}")
            }
        }
    }
}

/// Format `bytes` using `format`.
#[must_use]
pub fn format_bytes(bytes: u64, format: ByteFormat) -> String {
    format.format(bytes)
}

/// Format `bytes` in SI decimal units (`0 B`, `1.5 kB`, `1.5 MB`, `1.1 GB`).
#[must_use]
pub fn decimal_bytes(bytes: u64) -> String {
    ByteFormat::decimal().format(bytes)
}

/// Format `bytes` in IEC binary units (`0 B`, `1.4 KiB`, `1.4 MiB`, `4.6 GiB`).
#[must_use]
pub fn binary_bytes(bytes: u64) -> String {
    ByteFormat::binary().format(bytes)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn decimal_bytes_formats_table() {
        let cases = [
            (0, "0 B"),
            (1, "1 B"),
            (999, "999 B"),
            (1_000, "1.0 kB"),
            (1_023, "1.0 kB"),
            (1_024, "1.0 kB"),
            (1_500, "1.5 kB"),
            (999_999, "1000.0 kB"),
            (1_000_000, "1.0 MB"),
            (1_048_576, "1.0 MB"),
            (1_500_000, "1.5 MB"),
            (1_073_741_824, "1.1 GB"),
            (5_000_000_000, "5.0 GB"),
            (u64::MAX, "18446744073.7 GB"),
        ];
        for (input, expected) in cases {
            assert_eq!(decimal_bytes(input), expected, "input={input}");
        }
    }

    #[test]
    fn binary_bytes_formats_table() {
        let cases = [
            (0, "0 B"),
            (1, "1 B"),
            (999, "999 B"),
            (1_000, "1000 B"),
            (1_023, "1023 B"),
            (1_024, "1.0 KiB"),
            (1_500, "1.4 KiB"),
            (999_999, "976.5 KiB"),
            (1_000_000, "976.5 KiB"),
            (1_048_576, "1.0 MiB"),
            (1_500_000, "1.4 MiB"),
            (1_073_741_824, "1.0 GiB"),
            (5_000_000_000, "4.6 GiB"),
            (u64::MAX, "17179869183.9 GiB"),
        ];
        for (input, expected) in cases {
            assert_eq!(binary_bytes(input), expected, "input={input}");
        }
    }

    #[test]
    fn byte_format_supports_custom_floor_decimals_and_rounding() {
        let kilo_floor_truncated = ByteFormat::decimal()
            .min_unit(ByteUnit::Kilo)
            .decimal_from(ByteUnit::Giga)
            .rounding(ByteRounding::Truncate);
        for (input, expected) in [
            (0, "0 kB"),
            (999, "0 kB"),
            (1_500, "1 kB"),
            (1_500_000, "1 MB"),
            (1_073_741_824, "1.0 GB"),
            (u64::MAX, "18446744073.7 GB"),
        ] {
            assert_eq!(
                format_bytes(input, kilo_floor_truncated),
                expected,
                "kilo_floor_truncated input={input}"
            );
        }

        let mebi_floor_truncated = ByteFormat::binary()
            .min_unit(ByteUnit::Mega)
            .decimal_from(ByteUnit::Giga);
        for (input, expected) in [
            (0, "0 MiB"),
            (1_000_000, "0 MiB"),
            (1_048_576, "1 MiB"),
            (1_073_741_824, "1.0 GiB"),
            (5_000_000_000, "4.6 GiB"),
            (u64::MAX, "17179869183.9 GiB"),
        ] {
            assert_eq!(
                format_bytes(input, mebi_floor_truncated),
                expected,
                "mebi_floor_truncated input={input}"
            );
        }

        let kilo_floor_rounded = ByteFormat::decimal()
            .min_unit(ByteUnit::Kilo)
            .decimal_from(ByteUnit::Giga)
            .rounding(ByteRounding::Nearest);
        assert_eq!(format_bytes(1_600, kilo_floor_rounded), "2 kB");
    }
}
