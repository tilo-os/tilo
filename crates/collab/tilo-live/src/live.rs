//! The live plane's namespace, its tracks and how each track is delivered.
//!
//! One namespace per device per scope: `workspace/<workspace>/scope/<scope
//! id>/participant/<device hex>`. Every track of a participant is published in
//! that namespace, so a subscriber needs one subscription per participant and
//! a relay needs one authority decision per namespace
//! ([`crate::relay::SessionAuthorization`]).

use std::time::Duration;

use moq_net::Pattern;

use crate::video::{self, Layer, Source};

/// The segment that introduces a workspace's live namespaces.
const WORKSPACE: &str = "workspace";
/// The segment that introduces a scope within a workspace.
const SCOPE: &str = "scope";
/// The segment that introduces a participant's own namespace.
const PARTICIPANT: &str = "participant";

/// The namespace one device publishes one scope on.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Namespace {
    workspace: String,
    scope: String,
    device: [u8; 32],
}

impl Namespace {
    /// The namespace `device` publishes `scope` on.
    #[must_use]
    pub fn new(scope: &crate::scope::Scope, device: [u8; 32]) -> Self {
        Self {
            workspace: scope.workspace_id().to_owned(),
            scope: scope.id(),
            device,
        }
    }

    /// The workspace's own namespace for `device`.
    #[must_use]
    pub fn workspace(workspace: &str, device: [u8; 32]) -> Self {
        Self {
            workspace: workspace.to_owned(),
            scope: crate::scope::Scope::workspace(workspace).id(),
            device,
        }
    }

    /// `workspace/<workspace>/scope/<scope id>/participant/<device hex>`.
    #[must_use]
    pub fn path(&self) -> String {
        format!(
            "{}/{PARTICIPANT}/{}",
            self.scope_path(),
            hex::encode(self.device)
        )
    }

    /// `workspace/<workspace>/scope/<scope id>`: the subtree every participant
    /// of this scope publishes in, this device included.
    #[must_use]
    pub fn scope_path(&self) -> String {
        format!("{WORKSPACE}/{}/{SCOPE}/{}", self.workspace, self.scope)
    }

    /// The subtree of everything this participant publishes.
    #[must_use]
    pub fn prefix(&self) -> Option<Pattern> {
        Pattern::subtree(&self.path()).ok()
    }

    /// The scope this namespace belongs to, as its id.
    #[must_use]
    pub fn scope_id(&self) -> &str {
        &self.scope
    }

    /// The workspace this namespace belongs to.
    #[must_use]
    pub fn workspace_id(&self) -> &str {
        &self.workspace
    }

    /// The device that publishes this namespace.
    #[must_use]
    pub fn device(&self) -> [u8; 32] {
        self.device
    }

    /// The parts of the namespace `name`, or `None` for any other name.
    #[must_use]
    pub fn parse(name: &str) -> Option<Self> {
        let rest = name.strip_prefix(WORKSPACE)?.strip_prefix('/')?;
        let (workspace, rest) = rest.split_once('/')?;
        let rest = rest.strip_prefix(SCOPE)?.strip_prefix('/')?;
        let (scope, device) = rest.rsplit_once("/participant/")?;
        if workspace.is_empty() || scope.is_empty() || device.len() != 64 {
            return None;
        }
        let mut bytes = [0_u8; 32];
        hex::decode_to_slice(device, &mut bytes).ok()?;
        Some(Self {
            workspace: workspace.to_owned(),
            scope: scope.to_owned(),
            device: bytes,
        })
    }
}

/// Exact-path patterns, or `None` when a name cannot be spelled as a path.
pub(crate) fn exact(names: &[&str]) -> Option<moq_net::Patterns> {
    names
        .iter()
        .map(|name| moq_net::Pattern::literal(name).ok())
        .collect()
}

/// The subtree under which every participant of `scope` publishes.
#[must_use]
pub fn participants(scope: &crate::scope::Scope) -> String {
    format!(
        "{WORKSPACE}/{}/{SCOPE}/{}/{PARTICIPANT}",
        scope.workspace_id(),
        scope.id()
    )
}

/// The workspace segment for a workspace record's uuid bytes: the hyphenated
/// spelling a `WorkspaceId` displays, which is what a namespace and a grant's
/// bytes must agree on.
#[must_use]
pub fn workspace_id(bytes: [u8; 16]) -> String {
    let hex = hex::encode(bytes);
    let mut id = String::with_capacity(36);
    for (index, nibble) in hex.bytes().enumerate() {
        if matches!(index, 8 | 12 | 16 | 20) {
            id.push('-');
        }
        id.push(char::from(nibble));
    }
    id
}

/// The uuid bytes of a workspace record's id; `None` for any other spelling.
#[must_use]
pub fn workspace_bytes(workspace: &str) -> Option<[u8; 16]> {
    if workspace.len() != 36 {
        return None;
    }
    let mut bytes = [0_u8; 16];
    let hex: String = workspace.chars().filter(|c| *c != '-').collect();
    if hex.len() != 32 {
        return None;
    }
    hex::decode_to_slice(hex, &mut bytes).ok()?;
    // Only the one canonical spelling names the same workspace.
    (workspace_id(bytes) == workspace).then_some(bytes)
}

/// A track of a live namespace.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Track {
    /// Who is here and what they are doing.
    Presence,
    /// Microphone audio.
    Audio,
    /// Camera picture at one layer.
    Camera(Layer),
    /// Screen picture at one layer.
    Screen(Layer),
    /// One window's picture at one layer.
    Window(u64, Layer),
    /// One window's page as a paint stream.
    Page(u64),
    /// Pointer and keyboard input for one window.
    Input(u64),
    /// What a device is doing, for its own surfaces.
    Activity,
}

impl Track {
    /// The source whose picture this track carries, or `None` for any other
    /// track.
    #[must_use]
    pub const fn picture_of(self) -> Option<Source> {
        match self {
            Self::Camera(_) => Some(Source::Camera),
            Self::Screen(_) => Some(Source::Screen),
            _ => None,
        }
    }

    /// How large this track's content is: a picture at its layer, a window or
    /// a page at the window's own size.
    #[must_use]
    pub const fn layer(self) -> Layer {
        match self {
            Self::Camera(layer) | Self::Screen(layer) | Self::Window(_, layer) => layer,
            _ => Layer::High,
        }
    }
}

/// The name `track` is published under.
#[must_use]
pub fn track(track: Track) -> String {
    match track {
        Track::Presence => "presence".to_owned(),
        Track::Audio => "audio".to_owned(),
        Track::Camera(layer) => video::track(video::Source::Camera, layer),
        Track::Screen(layer) => video::track(video::Source::Screen, layer),
        Track::Window(tab, layer) => format!("{}/{}", video::window_track(tab), layer_name(layer)),
        Track::Page(tab) => crate::page::track(tab),
        Track::Input(tab) => format!("input/{tab:016x}"),
        Track::Activity => "activity".to_owned(),
    }
}

/// The track `name` names, or `None` for any other name.
#[must_use]
pub fn parse_track(name: &str) -> Option<Track> {
    match name {
        "presence" => return Some(Track::Presence),
        "audio" => return Some(Track::Audio),
        "activity" => return Some(Track::Activity),
        _ => {}
    }
    if let Some((source, layer)) = video::parse_track(name) {
        return Some(match source {
            video::Source::Camera => Track::Camera(layer),
            video::Source::Screen => Track::Screen(layer),
        });
    }
    if let Some(tab) = crate::page::parse_track(name) {
        return Some(Track::Page(tab));
    }
    if let Some(rest) = name.strip_prefix("window/") {
        let (window, layer) = rest.rsplit_once('/')?;
        let window = video::parse_window_track(&format!("window/{window}"))?;
        return Some(Track::Window(window, parse_layer(layer)?));
    }
    if let Some(tab) = name.strip_prefix("input/")
        && tab.len() == 16
    {
        return u64::from_str_radix(tab, 16).ok().map(Track::Input);
    }
    None
}

fn layer_name(layer: Layer) -> &'static str {
    match layer {
        Layer::Low => "low",
        Layer::Mid => "mid",
        Layer::High => "high",
    }
}

fn parse_layer(name: &str) -> Option<Layer> {
    match name {
        "low" => Some(Layer::Low),
        "mid" => Some(Layer::Mid),
        "high" => Some(Layer::High),
        _ => None,
    }
}

/// How one track's objects are delivered.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Delivery {
    /// One object per datagram, redundant bundles, dropped when late.
    Datagram { retain: Duration, redundancy: bool },
    /// Newest group wins: a subscriber keeps only the latest.
    Latest { max_age: Duration },
    /// Groups of pictures, entered at a keyframe.
    Keyed { retain: Duration, max_objects: u64 },
    /// Ordered, reliable, small objects.
    Stream { max_age: Duration },
}

/// How `track` is delivered.
#[must_use]
pub fn delivery(track: Track) -> Delivery {
    match track {
        Track::Audio => Delivery::Datagram {
            retain: Duration::from_millis(200),
            redundancy: true,
        },
        Track::Camera(_) | Track::Screen(_) | Track::Window(..) | Track::Page(_) => {
            Delivery::Keyed {
                retain: Duration::from_secs(2),
                max_objects: video::MAX_GROUP_OBJECTS,
            }
        }
        Track::Input(_) => Delivery::Stream {
            max_age: Duration::from_secs(5),
        },
        Track::Presence | Track::Activity => Delivery::Stream {
            max_age: Duration::from_secs(30),
        },
    }
}

/// The send priority of `track`: higher is sooner.
#[must_use]
pub const fn priority(track: Track) -> u8 {
    match track {
        Track::Audio => 200,
        Track::Input(_) => 180,
        Track::Presence => 160,
        Track::Screen(_) => 120,
        Track::Window(..) => 100,
        Track::Camera(_) => 80,
        Track::Page(_) => 140,
        Track::Activity => 40,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::scope::Scope;

    const DEVICE: [u8; 32] = [0xAB; 32];

    #[test]
    fn a_namespace_names_the_scope_and_the_participant() {
        let scope = Scope::workspace("w1");
        let namespace = Namespace::new(&scope, DEVICE);
        assert_eq!(
            namespace.path(),
            format!(
                "workspace/w1/scope/workspace/participant/{}",
                "ab".repeat(32)
            )
        );
        assert_eq!(Namespace::parse(&namespace.path()), Some(namespace.clone()));
        assert_eq!(namespace.scope_path(), "workspace/w1/scope/workspace");
        assert!(namespace.path().starts_with(&namespace.scope_path()));
        assert_eq!(namespace.scope_id(), "workspace");
        assert_eq!(namespace.device(), DEVICE);
    }

    #[test]
    fn a_channel_scope_has_its_own_namespace() {
        let route = collab_types::ChannelRoute::new("so.tilo.sheets", "payroll");
        let scope = Scope::channel("w1", route.channel("w1"));
        let path = Namespace::new(&scope, DEVICE).path();
        assert!(path.contains(&format!("/scope/{}/", scope.id())));
        assert_ne!(path, Namespace::new(&Scope::workspace("w1"), DEVICE).path());
    }

    #[test]
    fn every_track_round_trips_and_has_its_own_name() {
        let tracks = [
            Track::Presence,
            Track::Audio,
            Track::Camera(Layer::Low),
            Track::Camera(Layer::High),
            Track::Window(7, Layer::Low),
            Track::Page(7),
            Track::Input(7),
            Track::Activity,
        ];
        for track in tracks {
            assert_eq!(parse_track(&super::track(track)), Some(track));
        }
        let mut names: Vec<_> = tracks.iter().map(|track| super::track(*track)).collect();
        let count = names.len();
        names.sort();
        names.dedup();
        assert_eq!(names.len(), count, "two tracks share a name");
    }

    #[test]
    fn a_screen_has_no_mid_layer() {
        assert_eq!(parse_track("screen/mid"), None);
        assert_eq!(parse_track("screen/high"), Some(Track::Screen(Layer::High)));
    }

    #[test]
    fn a_window_track_names_its_layer() {
        assert_eq!(
            parse_track("window/0000000000000007/low"),
            Some(Track::Window(7, Layer::Low))
        );
        assert_eq!(parse_track("window/0000000000000007"), None);
        assert_eq!(parse_track("window/nothex/low"), None);
    }

    #[test]
    fn a_page_track_names_its_window() {
        assert_eq!(parse_track("page/0000000000000007"), Some(Track::Page(7)));
        assert_eq!(Track::Page(7).layer(), Layer::High);
        assert_eq!(Track::Page(7).picture_of(), None);
        assert_eq!(
            Track::Camera(Layer::Mid).picture_of(),
            Some(crate::video::Source::Camera)
        );
        assert_eq!(parse_track("page/nothex"), None);
        assert_eq!(parse_track("page/0000000000000007/low"), None);
    }

    #[test]
    fn priorities_put_audio_before_pictures() {
        assert!(priority(Track::Audio) > priority(Track::Presence));
        assert!(priority(Track::Presence) > priority(Track::Screen(Layer::High)));
        assert!(priority(Track::Screen(Layer::High)) > priority(Track::Camera(Layer::High)));
        assert!(priority(Track::Camera(Layer::High)) > priority(Track::Activity));
    }

    #[test]
    fn a_workspace_id_round_trips_through_its_grant_bytes() {
        let workspace = "0f8fad5b-d9cb-469f-a165-70867728950e";
        let bytes = workspace_bytes(workspace).unwrap();
        assert_eq!(workspace_id(bytes), workspace);
        assert_eq!(
            workspace_bytes("0F8FAD5B-D9CB-469F-A165-70867728950E"),
            None
        );
        assert_eq!(workspace_bytes("0f8fad5bd9cb469fa16570867728950"), None);
        assert_eq!(workspace_bytes("workspace"), None);
    }

    #[test]
    fn participants_names_the_subtree_of_one_scope() {
        let scope = Scope::workspace("0f8fad5b-d9cb-469f-a165-70867728950e");
        assert_eq!(
            participants(&scope),
            "workspace/0f8fad5b-d9cb-469f-a165-70867728950e/scope/workspace/participant"
        );
    }

    #[test]
    fn audio_is_a_redundant_datagram_track() {
        assert_eq!(
            delivery(Track::Audio),
            Delivery::Datagram {
                retain: Duration::from_millis(200),
                redundancy: true
            }
        );
        assert!(matches!(delivery(Track::Presence), Delivery::Stream { .. }));
    }
}
