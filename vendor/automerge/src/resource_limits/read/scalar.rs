//! Streaming legacy scalar diagnostics without owning native values.
use super::NativeScalar;
use crate::value::{Counter, ScalarDebug};

impl NativeScalar<'_> {
    /// Writes the exact legacy queried ScalarValue debug representation. No
    /// intermediate String is created. The embedding caller must pay any
    /// output growth or use an allocation-free streaming sink.
    ///
    /// Legacy map get/get_all fixes counter sums before into_legacy constructs
    /// Counter::from(sum), so both displayed fields are the queried sum. This
    /// does not stand in for an arbitrary original ScalarValue counter.
    pub fn write_legacy_debug(&self, sink: &mut impl std::fmt::Write) -> std::fmt::Result {
        let counter = Counter::from(match self {
            Self::Counter(value) => *value,
            _ => 0,
        });
        let value = match self {
            Self::Bytes(v) => ScalarDebug::Bytes(v),
            Self::Str(v) => ScalarDebug::Str(v),
            Self::Int(v) => ScalarDebug::Int(*v),
            Self::Uint(v) => ScalarDebug::Uint(*v),
            Self::F64(v) => ScalarDebug::F64(*v),
            Self::Counter(_) => ScalarDebug::Counter(&counter),
            Self::Timestamp(v) => ScalarDebug::Timestamp(*v),
            Self::Boolean(v) => ScalarDebug::Boolean(*v),
            Self::Unknown { type_code, bytes } => ScalarDebug::Unknown {
                type_code: *type_code,
                bytes,
            },
            Self::Null => ScalarDebug::Null,
        };
        sink.write_fmt(format_args!("{value:?}"))
    }
}
