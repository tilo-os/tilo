//! The domain vocabulary search results are grouped by.

use ui::icons::Icon;

/// One well-known category a result can belong to — the domain vocabulary
/// the chip strip and grouping use. Providers pick from it or, via
/// [`ResourceKind::Custom`], mint a new one.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum ResourceKind {
    /// A palette-registered chrome action (e.g. "Open workspace settings").
    Action,
    /// An installed Tilo application. Enter launches it, sandboxed, into
    /// the workspace you are looking at.
    Application,
    /// An app Tilo did not package (Flatpak, distro binary). Its own kind
    /// because a Tilo app runs sandboxed in a room; a host app runs unconfined.
    HostApplication,
    /// A note document (title + body). Enter opens/focuses the note tab.
    Note,
    /// A record an app made, collected or imported. Enter brings it into
    /// the current room (a reference, not a copy) and opens it if openable.
    Record,
    File,
    Folder,
    /// A creator/channel-style entity — pre-declared so a future provider
    /// plugs into the same registry without teaching the palette a new kind.
    Creator,
    Video,
    Playlist,
    /// Escape hatch for provider-specific kinds, shown verbatim in the chip strip.
    Custom(String),
}

impl ResourceKind {
    /// Human-readable label used in the filter chip strip and group
    /// headers. `Custom` variants surface their own string verbatim.
    pub fn label(&self) -> &str {
        match self {
            ResourceKind::Action => "Actions",
            ResourceKind::Application => "Applications",
            ResourceKind::HostApplication => "Host Apps",
            ResourceKind::Note => "Notes",
            ResourceKind::Record => "Records",
            ResourceKind::File => "Files",
            ResourceKind::Folder => "Folders",
            ResourceKind::Creator => "Creators",
            ResourceKind::Video => "Videos",
            ResourceKind::Playlist => "Playlists",
            ResourceKind::Custom(s) => s.as_str(),
        }
    }

    /// Stable sort key pinning the well-known kinds to a fixed palette
    /// order. Applications rank above content kinds because Enter
    /// activates the top row — a typed app name must launch the app, not
    /// surface a source file that merely contains the word.
    pub fn sort_key(&self) -> (u8, String) {
        match self {
            ResourceKind::Action => (0, String::new()),
            ResourceKind::Application => (1, String::new()),
            ResourceKind::HostApplication => (2, String::new()),
            ResourceKind::Note => (3, String::new()),
            ResourceKind::Record => (4, String::new()),
            ResourceKind::File => (5, String::new()),
            ResourceKind::Folder => (6, String::new()),
            ResourceKind::Creator => (7, String::new()),
            ResourceKind::Video => (8, String::new()),
            ResourceKind::Playlist => (9, String::new()),
            ResourceKind::Custom(s) => (10, s.clone()),
        }
    }

    /// The glyph a palette row of this kind leads with.
    pub fn icon(&self) -> Icon {
        match self {
            ResourceKind::Action => Icon::Settings2,
            ResourceKind::Application => Icon::LayoutGrid,
            // Deliberately *not* the applications glyph. A host app is not
            // one of Tilo's — it runs unconfined, outside everything the
            // grid stands for — and the row has to look different at a
            // glance, not only read differently.
            ResourceKind::HostApplication => Icon::ExternalLink,
            ResourceKind::Note => Icon::FileText,
            // A record is one document several rooms can reach — the stacked
            // plates say "this is the same thing in more than one place",
            // which is exactly what a reference is.
            ResourceKind::Record => Icon::Layers,
            ResourceKind::File => Icon::File,
            ResourceKind::Folder => Icon::Folder,
            ResourceKind::Creator => Icon::Star,
            ResourceKind::Video => Icon::Film,
            ResourceKind::Playlist => Icon::List,
            ResourceKind::Custom(_) => Icon::Search,
        }
    }
}
