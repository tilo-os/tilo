//! Typed CUPS integration for Settings and Print Centre: a narrow vocabulary, so callers cannot
//! pass command fragments or arbitrary option strings. The device's print service answers
//! through its own clients (`ipptool`, `lp`), never a bus an app holds.

pub mod printing;
