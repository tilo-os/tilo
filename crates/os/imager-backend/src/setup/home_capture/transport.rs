//! App compatibility surface for native authenticated capture channels.
pub use crate::capture::transport::*;

#[cfg(test)]
#[cfg(feature = "native-capture-qualification")]
mod native_inputs;
