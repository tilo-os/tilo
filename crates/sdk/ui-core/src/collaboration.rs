//! Toolkit-side collab client. The broker client (routes, transport,
//! committed reads, signing, invocation updates, presence transport) is the
//! `collaboration` crate, which a process that commits to a room links
//! without the toolkit; it keeps its path here. What stays is the widget
//! side: who is in the room as the toolkit shows them, and the content a
//! view reads.

pub use ::collaboration::*;
pub mod owner_view;
pub mod participants;
pub mod record_content;

#[cfg(test)]
#[cfg(feature = "broker-transport")]
mod approved_checks_tests;
#[cfg(test)]
#[cfg(all(unix, feature = "broker-transport"))]
mod broker_app_tests;
#[cfg(test)]
#[cfg(feature = "broker-transport")]
mod external_moderation_tests;
