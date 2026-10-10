pub mod hdr_composite;
pub mod hdr_encode;
pub mod icc_lut;
pub mod orientation;
pub mod paint;
/// Returning the device memory a software GPU freed to the kernel (TIL-731).
#[cfg(target_os = "linux")]
pub mod software_gpu_memory;
pub mod warmup;
pub mod xrgb_history;

#[derive(Copy, Clone, Debug, Eq, PartialEq, Hash, PartialOrd, Ord)]
pub struct OutputId(pub u32);
