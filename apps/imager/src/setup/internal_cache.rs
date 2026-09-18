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
