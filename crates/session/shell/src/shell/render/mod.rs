pub mod paint;
pub mod strip_layers;

#[derive(Copy, Clone, Debug, Eq, PartialEq, Hash, PartialOrd, Ord)]
pub struct OutputId(pub u32);
