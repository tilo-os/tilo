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

/// What a host lists as apps.
pub mod apps;

/// The clipboard a role hands the person something on.
pub mod clipboard;

/// The workspace in front, as this desk makes invitations into it.
#[cfg(target_os = "linux")]
pub mod invite;

/// Whether this device still holds custody of a workspace, as this desktop checks it.
pub mod custody;

/// The person's certified devices, as a role reaches them.
#[cfg(target_os = "linux")]
pub mod fabric;

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

/// Where the activity model's code is, when a host does not link it in.
pub mod pane;

/// One notice a role tells the person, and where choosing it lands.
pub mod notice;

/// What a role reads of the workspaces' records, and what changed in them.
pub mod records;

/// Who a principal is, as presence shows them, and the actors this host's own mounts write as.
pub mod authors;

/// A store a role keeps its records and its own view in: the desktop's profile, shared with the
/// store's own thread, and the page's own store, which it drives itself.
#[cfg(target_os = "linux")]
pub type Store = std::sync::Arc<dyn durable_store::DurableStore + Send + Sync>;
#[cfg(not(target_os = "linux"))]
pub type Store = std::rc::Rc<dyn durable_store::DurableStore>;
