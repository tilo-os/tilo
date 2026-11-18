//! Files' resources: what each device opened or changed through Files, and the
//! folder a window shows, as the app that draws them and an agent that reads
//! them both see them.
//!
//! The files themselves, their watching, copying and painting are the app's;
//! only the recent list and a window's place travel here.

pub mod recent;
pub mod window;
