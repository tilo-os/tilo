//! The native shell side of the toolkit, behind the default-on `native` feature: the GPU backend
//! a window starts on, the one instance a standalone app runs as, and startup warmup.

pub(crate) mod gpu_backends;
pub mod standalone_instance;
pub mod warmup;
