//! Private checkpoint protocol. Native ownership and publication providers are
//! deliberately absent; this module does not activate persistent downloading.
mod checkpoint;
pub(super) use checkpoint::*;
#[cfg(test)]
pub(super) mod tests;
