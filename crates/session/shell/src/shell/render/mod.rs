pub mod paint;
pub mod xrgb_history;

#[derive(Copy, Clone, Debug, Eq, PartialEq, Hash, PartialOrd, Ord)]
pub struct OutputId(pub u32);
