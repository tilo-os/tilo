//! Streaming legacy scalar diagnostics without owning native values.
//! OWNER: lane L4 (0.8 added `value::ScalarDebug`; port it or equivalent).
use super::NativeScalar;
impl NativeScalar<'_> {
    /// Writes the exact legacy queried ScalarValue debug representation. No
    /// intermediate String is created.
    pub fn write_legacy_debug(&self, sink: &mut impl std::fmt::Write) -> std::fmt::Result {
        let _ = sink;
        todo!("L4")
    }
}
