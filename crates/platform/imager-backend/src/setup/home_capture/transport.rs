//! App compatibility surface for native authenticated capture channels.
pub use crate::capture::transport::*;

#[cfg(all(test, feature = "native-capture-qualification"))]
mod native_inputs;
