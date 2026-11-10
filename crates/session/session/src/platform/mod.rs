//! The effects a host provides to the session's roles.
//!
//! The session's roles publish what a person sees and does as resources and
//! actions. What they cannot do themselves they ask a host for: surfaces to
//! draw on, apps to run, a store to keep records in, a peer to reach the
//! person's other devices, and the machine's own services. On a Linux
//! desktop that host is [`session`], this session's compositor and the
//! services it starts. On the web it is the page, which provides the same
//! effects itself: the canvas the surfaces are drawn on, the browser's app
//! host, the browser's store, and the page's own transport.
//!
//! Each concern the roles ask for lives here as one seam, so a role names
//! the effect and never a platform crate: [`watched`] is the store a value
//! is kept current in, and a concern with a Linux half in [`session`] gains
//! its page half here beside it.

#[cfg(target_os = "linux")]
pub mod session;

/// What a session says to the collaboration plane, as every host carries it.
pub(crate) mod plane;

/// What a host lists as apps.
pub mod apps;

/// Whether this device still holds custody of a workspace, as its host checks it.
pub mod custody;

/// The identity this device signs with, as its host keeps it.
#[cfg(target_os = "linux")]
pub mod identity;

/// What apps hold and what the person's files say, as this host indexes them.
pub mod indexes;

/// What this host calls the person at it.
pub mod person;

/// What the session's own actions ask of the session that holds them.
pub mod steps;

/// Where an agent's task programs run.
pub mod programs;

/// Where a session's own question is answered: the model gateway its host runs. The capability it
/// names is built where the question runs — on the answering thread on a desktop, on the page's
/// own turn on the web — so a host whose models hold its one thread's handles answers too.
pub trait Models: Send + Sync {
    /// The capability a question asked in this session runs on; `None` while the person set no
    /// provider up, or no gateway runs here.
    fn models(&self) -> Option<ui::models::Models>;
}

/// The layers the session's roles draw on: a panel, a roster or a card, shown over the windows.
pub mod surfaces;

/// A value the host keeps current: a directory of files it watches on a
/// Linux desktop, the page's own store on the web.
pub mod watched;
