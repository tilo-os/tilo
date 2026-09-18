//! Private checkpoint protocol and native cache providers. No caller or support
//! route is activated by this module.
mod checkpoint;
pub(super) use checkpoint::*;
#[cfg(test)]
pub(super) mod tests;

#[cfg(any(target_os = "linux", all(test, unix)))]
pub(super) mod linux;

#[cfg(any(windows, test))]
pub(super) mod windows;

#[cfg(any(target_os = "linux", windows, all(test, unix)))]
pub(super) mod coordinator;

/// Scoped consumer interface: no File, raw handle, clone or path authority.
pub(in crate::setup) trait HeldReadSeek: std::io::Read + std::io::Seek {}
impl<T: std::io::Read + std::io::Seek> HeldReadSeek for T {}
