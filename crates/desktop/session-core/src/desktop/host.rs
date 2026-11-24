//! What any shell host answers about the shell it shows, beside the surface every host draws: the
//! questions the tree does not hold by itself, so a page and a native session answer them the same
//! way. [`chrome`] holds the readings a host supplies and the requests only it carries out,
//! [`contract`] the asks the shell leaves its host, and [`records`] which record each of a shell's
//! windows shows on the workspace's shared layout. What only one host answers, because it is what
//! that host is made of — a page's canvas and storage, a session's compositor and windows — stays
//! in that host's adapter.

pub mod chrome;
pub mod contract;
pub mod records;
