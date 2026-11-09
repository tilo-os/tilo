//! The session's roles, written once for every host that runs a desk: the native session and the
//! page the browser runtime shows.
//! A host gives the core what it cannot do itself — where the desk's bytes are kept — and the core
//! gives back what the desk is, how it changed and who changed it. Every role that moves here
//! removes the copy the other host kept, so the two run the same rules.

pub mod arrangement;
pub mod desk;
pub mod door;
pub mod presence;
