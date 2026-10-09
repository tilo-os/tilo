//! Facts about the host operating system that portable apps display or
//! depend on.
//!
//! Each fact is answered where the platform can answer it cheaply and
//! reported as unknown elsewhere, so callers never guess.

pub mod process;
