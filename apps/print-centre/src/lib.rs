//! Print Centre: a focused, live view of printers, jobs, supplies, recovery,
//! document-centric printer, queue, recovery and scanner workflows.

pub mod resource;
mod surface;

pub use resource::{PrintCentreEffect, PrintCentreShell, PrintDefaults};
