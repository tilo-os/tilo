//! The live plane's namespace, its tracks and how each track is delivered.
//!
//! One namespace per device per scope: `workspace/<workspace>/scope/<scope
//! id>/participant/<device hex>`. Every track of a participant is published in
//! that namespace, so a subscriber needs one subscription per participant and
//! a relay needs one authority decision per namespace
//! ([`crate::relay::SessionAuthorization`]).

use std::time::Duration;

use moq_net::Pattern;

use crate::video::{self, Layer};

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
            "{WORKSPACE}/{}/{SCOPE}/{}/{PARTICIPANT}/{}",
            self.workspace,
            self.scope,
            hex::encode(self.device)
        )
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

/// A track of a live namespace.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
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
    /// Pointer and keyboard input for one window.
    Input(u64),
    /// What a device is doing, for its own surfaces.
    Activity,
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
        Track::Camera(_) | Track::Screen(_) | Track::Window(..) => Delivery::Keyed {
            retain: Duration::from_secs(2),
            max_objects: video::MAX_GROUP_OBJECTS,
        },
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
        assert_eq!(namespace.scope_id(), "workspace");
        assert_eq!(namespace.device(), DEVICE);
    }

    #[test]
    fn a_channel_scope_has_its_own_namespace() {
        let route = collab_types::ChannelRoute::new("so.tilo.sheets", "payroll");
        let scope = Scope::channel("w1", route);
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
    fn priorities_put_audio_before_pictures() {
        assert!(priority(Track::Audio) > priority(Track::Presence));
        assert!(priority(Track::Presence) > priority(Track::Screen(Layer::High)));
        assert!(priority(Track::Screen(Layer::High)) > priority(Track::Camera(Layer::High)));
        assert!(priority(Track::Camera(Layer::High)) > priority(Track::Activity));
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
