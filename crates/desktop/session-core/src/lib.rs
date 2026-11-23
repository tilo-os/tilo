//! The session's roles, written once for every host that runs a layout: the native session and the
//! page the browser runtime shows.
//! A host gives the core what it cannot do itself — where the layout's bytes are kept — and the core
//! gives back what the layout is, how it changed and who changed it. Every role that moves here
//! removes the copy the other host kept, so the two run the same rules.

pub mod desktop;
pub mod layout;
pub mod presence;
pub mod shell;
pub mod super_tap;
pub mod surface;
pub mod surfaces;
