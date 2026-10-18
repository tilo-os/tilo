//! The Linux backend: `clone3` into a cgroup and new namespaces, where the
//! job's init runs as a new program.

pub mod idmap;
pub mod plan;
pub mod runner;
pub mod sys;
mod wire;
