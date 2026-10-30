//! `org.freedesktop.impl.portal.ScreenCast`: an app sharing outputs or
//! windows the person picks, as `PipeWire` streams.
//!
//! `CreateSession` makes the session ([`crate::session`]); `SelectSources`
//! records what kinds of source the app wants; `Start` asks the person
//! which outputs or windows to share, a question of kind
//! [`SCREENCAST_QUESTION`] offering what the session can capture now
//! ([`CaptureStreams::sources`]), and starts one stream per source picked.
//! A valid `restore_data` the app kept from an earlier share
//! ([`crate::restore`]) shares the same sources again without asking.
//! Every running stream is a capture lease the person sees and may stop
//! ([`crate::captures`]); stopping it ends the session.
//!
//! A `RemoteDesktop` session takes `SelectSources` too: its `Start` asks
//! once for the devices and the sources together ([`share`]).

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use serde::{Deserialize, Serialize};
use tilo_lock::MutexRecover as _;
use tilo_service::{Answer, Question};
use zbus::interface;
use zbus::zvariant::{ObjectPath, OwnedValue, Value};

use crate::reply::{Ended, OrFail, Reply, Results, answer, ensure};
use crate::restore::{Grant, restore_data, restore_key, unix_now};
use crate::session::SessionKind;
use crate::streams::{CaptureSource, SourceKind, Stream, StreamRequest, cursor_mode};
use crate::{Portal, PortalCaller, option, question_handle};

/// The kind of the question that picks what a screen share shows.
pub const SCREENCAST_QUESTION: &str = "portal.screencast";
/// The most sources one share may show.
const MAX_SHARED: usize = 16;

/// A source offered in a share question.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct OfferedSource {
    /// What the answer names it by, in `uris`: `output:<name>` or
    /// `window:<identifier>`.
    pub choice: String,
    pub kind: SourceKind,
    /// The output's description or the window's title, as given.
    pub label: String,
    /// The window's app, when it named one.
    pub app_id: Option<String>,
}

/// What the sheet shows for a share: the sources to pick from (the answer
/// lists the picked ones' `choice` in `uris`) and, for a remote desktop,
/// the input devices the app would drive.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ShareQuestion {
    pub sources: Vec<OfferedSource>,
    /// Whether more than one source may be picked.
    pub multiple: bool,
    /// Whether the person may let the app share the same again without
    /// asking (the answer's `remember`).
    pub may_remember: bool,
    /// The input devices asked for: `keyboard`, `pointer`.
    pub devices: Vec<String>,
}

/// What an app selected for a session's share, before it started.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct Selection {
    pub types: u32,
    pub cursor_mode: u32,
    pub multiple: bool,
    /// What the app asked to be remembered for: 0, 1 or 2.
    pub persist_mode: u32,
    /// What a restore key the app handed back shares again.
    pub restore: Option<Grant>,
}

#[derive(Default)]
struct Cast {
    selection: Option<Selection>,
    starting: bool,
    /// The nodes of its running streams; started once this is set.
    nodes: Option<Vec<u32>>,
}

/// The sessions' shares, by session path.
#[derive(Clone, Default)]
pub struct Casts {
    casts: Arc<Mutex<HashMap<String, Cast>>>,
}

impl Casts {
    /// Stop the streams of sessions that end, through `portal`'s capture.
    pub(crate) fn follow(&self, portal: &Portal) {
        let casts = self.clone();
        let streams = Arc::clone(&portal.streams);
        portal.sessions.on_end(move |path| {
            let cast = casts.casts.lock_recover().remove(path);
            for node in cast.and_then(|cast| cast.nodes).unwrap_or_default() {
                streams.stop(node);
            }
        });
    }

    pub(crate) fn selection(&self, path: &str) -> Option<Selection> {
        self.casts
            .lock_recover()
            .get(path)
            .and_then(|cast| cast.selection.clone())
    }

    /// Record `selection` for session `path`, which has not started.
    fn select(&self, path: &str, selection: Selection) -> bool {
        let mut casts = self.casts.lock_recover();
        let cast = casts.entry(path.to_owned()).or_default();
        let free = !cast.starting && cast.nodes.is_none();
        if free {
            cast.selection = Some(selection);
        }
        drop(casts);
        free
    }

    /// Mark session `path` starting; `false` when it is or did.
    pub(crate) fn begin(&self, path: &str) -> bool {
        let mut casts = self.casts.lock_recover();
        let cast = casts.entry(path.to_owned()).or_default();
        let free = !cast.starting && cast.nodes.is_none();
        cast.starting |= free;
        drop(casts);
        free
    }

    /// Session `path` started (`Some` nodes) or did not.
    pub(crate) fn finish(&self, path: &str, nodes: Option<Vec<u32>>) -> bool {
        let mut casts = self.casts.lock_recover();
        let Some(cast) = casts.get_mut(path) else {
            return false;
        };
        cast.starting = false;
        cast.nodes = nodes;
        drop(casts);
        true
    }

    /// The nodes session `path` streams into.
    #[must_use]
    pub fn nodes(&self, path: &str) -> Vec<u32> {
        self.casts
            .lock_recover()
            .get(path)
            .and_then(|cast| cast.nodes.clone())
            .unwrap_or_default()
    }
}

/// What a share started.
pub(crate) struct Shared {
    pub streams: Vec<(CaptureSource, Stream)>,
    /// The persist mode granted: 0 when nothing is remembered.
    pub persist_mode: u32,
}

/// The sources of `restore` as they are now, when every one is still
/// there: outputs by name, windows by identifier or else by their app
/// (a window's identifier does not outlive the session).
fn restored_sources(restore: &Grant, offered: &[CaptureSource]) -> Option<Vec<CaptureSource>> {
    let mut picked: Vec<CaptureSource> = Vec::new();
    for remembered in &restore.sources {
        let found = offered
            .iter()
            .filter(|source| source.kind == remembered.kind && !picked.contains(source))
            .find(|source| source.id == remembered.id)
            .or_else(|| {
                (remembered.kind == SourceKind::Window && remembered.app_id.is_some())
                    .then(|| {
                        offered.iter().find(|source| {
                            source.kind == SourceKind::Window
                                && source.app_id == remembered.app_id
                                && !picked.contains(source)
                        })
                    })
                    .flatten()
            })?;
        picked.push(found.clone());
    }
    (!picked.is_empty()).then_some(picked)
}

/// The sources an answer picked from `offered`.
fn picked(
    answer: &Answer,
    offered: &[CaptureSource],
    multiple: bool,
) -> Option<Vec<CaptureSource>> {
    let mut picked: Vec<CaptureSource> = Vec::new();
    for choice in &answer.uris {
        let source = offered.iter().find(|source| &source.choice() == choice)?;
        if !picked.contains(source) {
            picked.push(source.clone());
        }
    }
    let fits = !picked.is_empty() && picked.len() <= if multiple { MAX_SHARED } else { 1 };
    fits.then_some(picked)
}

/// The persist mode an answer grants: an app asking to be remembered
/// until revoked is only if the person said so; otherwise it is
/// remembered while it runs.
const fn granted_persist_mode(asked: u32, remember: bool) -> u32 {
    match asked {
        2 if remember => 2,
        0 => 0,
        _ => 1,
    }
}

const fn device_names(devices: u32) -> [Option<&'static str>; 2] {
    use crate::remote_desktop::device_type;
    [
        if devices & device_type::KEYBOARD != 0 {
            Some("keyboard")
        } else {
            None
        },
        if devices & device_type::POINTER != 0 {
            Some("pointer")
        } else {
            None
        },
    ]
}

/// What a share asks.
pub(crate) struct Ask<'a> {
    /// The session.
    pub path: &'a str,
    /// The request the question is asked under.
    pub request: &'a str,
    pub caller: &'a PortalCaller,
    /// `None` for a remote desktop sharing no screen.
    pub selection: Option<&'a Selection>,
    /// What a restore key shares again without asking.
    pub restore: Option<&'a Grant>,
    /// The question's kind.
    pub kind: &'a str,
    /// The devices a remote desktop drives.
    pub devices: u32,
}

/// Ask the person for a session's share (unless its restore key shares
/// the same again), then start a stream of every source picked, each held
/// as a capture lease.
pub(crate) async fn share(portal: &Portal, ask: Ask<'_>) -> Result<Shared, Ended> {
    let offered: Vec<CaptureSource> = ask.selection.map_or_else(Vec::new, |selection| {
        portal
            .streams
            .sources()
            .into_iter()
            .filter(|source| selection.types & source.kind.source_type() != 0)
            .collect()
    });
    let restored = match (ask.selection, ask.restore) {
        (Some(_), Some(restore)) => restored_sources(restore, &offered),
        (None, Some(_)) => Some(Vec::new()),
        _ => None,
    };
    let persist_asked = ask.restore.map_or_else(
        || ask.selection.map_or(0, |selection| selection.persist_mode),
        |grant| grant.persist_mode,
    );
    let (sources, persist_mode) = if let Some(sources) = restored {
        (sources, persist_asked)
    } else {
        pick(portal, &ask, &offered, persist_asked).await?
    };
    let cursor = ask
        .selection
        .map_or(cursor_mode::HIDDEN, |selection| selection.cursor_mode);
    Ok(Shared {
        streams: start_streams(portal, ask.path, ask.caller, sources, cursor).await?,
        persist_mode,
    })
}

/// Ask the person what to share of `offered`, and whether to remember it;
/// the sources and the persist mode granted.
async fn pick(
    portal: &Portal,
    ask: &Ask<'_>,
    offered: &[CaptureSource],
    persist_asked: u32,
) -> Result<(Vec<CaptureSource>, u32), Ended> {
    ensure(ask.selection.is_none() || !offered.is_empty())?;
    let multiple = ask.selection.is_some_and(|selection| selection.multiple);
    let question = ShareQuestion {
        sources: offered
            .iter()
            .map(|source| OfferedSource {
                choice: source.choice(),
                kind: source.kind,
                label: source.label.clone(),
                app_id: source.app_id.clone(),
            })
            .collect(),
        multiple,
        may_remember: persist_asked == 2,
        devices: device_names(ask.devices)
            .into_iter()
            .flatten()
            .map(str::to_owned)
            .collect(),
    };
    let answer = portal
        .questions
        .ask(Question {
            handle: question_handle(ask.request),
            kind: ask.kind.to_owned(),
            app_id: Some(ask.caller.app_id.clone()),
            label: ask.caller.app_id.clone(),
            details: serde_json::to_value(question).unwrap_or_default(),
        })
        .await
        .ok_or(Ended::Cancelled)?;
    let sources = if ask.selection.is_some() {
        picked(&answer, offered, multiple).or_fail()?
    } else {
        Vec::new()
    };
    Ok((
        sources,
        granted_persist_mode(persist_asked, answer.remember),
    ))
}

/// Start a stream of every one of `sources` for session `path`, each held
/// as a capture lease; none when one fails.
async fn start_streams(
    portal: &Portal,
    path: &str,
    caller: &PortalCaller,
    sources: Vec<CaptureSource>,
    cursor: u32,
) -> Result<Vec<(CaptureSource, Stream)>, Ended> {
    let mut streams: Vec<(CaptureSource, Stream)> = Vec::new();
    for source in sources {
        let stream = portal
            .streams
            .start(StreamRequest {
                source: source.clone(),
                cursor_mode: cursor,
                app_id: caller.app_id.clone(),
            })
            .await;
        let held = stream.map_err(|error| {
            tracing::warn!(%error, "screen share stream did not start");
        });
        let held = held.and_then(|stream| {
            let capture = Arc::clone(&portal.streams);
            let node = stream.node_id;
            portal
                .captures
                .hold_stream(
                    path,
                    caller,
                    &source.label,
                    node,
                    Arc::new(move || capture.stop(node)),
                )
                .map(|_| stream)
                .map_err(|error| {
                    portal.streams.stop(node);
                    tracing::warn!(%error, "screen share not leased");
                })
        });
        let Ok(stream) = held else {
            for (_, stream) in &streams {
                portal.streams.stop(stream.node_id);
            }
            return Err(Ended::Failed);
        };
        streams.push((source, stream));
    }
    Ok(streams)
}

/// Remember `shared` for `caller` when it was granted persistence; the
/// results' `restore_data` and `persist_mode`.
pub(crate) fn remember(
    portal: &Portal,
    caller: &PortalCaller,
    shared: &Shared,
    cursor_mode: u32,
    devices: u32,
    results: &mut Results,
) -> Result<(), Ended> {
    if shared.persist_mode == 0 {
        return Ok(());
    }
    let key = portal
        .grants
        .insert(Grant {
            id: String::new(),
            app_id: caller.app_id.clone(),
            persist_mode: shared.persist_mode,
            issued_unix_seconds: unix_now(),
            sources: shared
                .streams
                .iter()
                .map(|(source, _)| source.clone())
                .collect(),
            cursor_mode,
            devices,
        })
        .map_err(|error| {
            tracing::warn!(%error, "share not remembered");
            Ended::Failed
        })?;
    results.insert("restore_data".into(), restore_data(key).or_fail()?);
    results.insert("persist_mode".into(), OwnedValue::from(shared.persist_mode));
    portal.sessions.changed();
    Ok(())
}

/// The `streams` result: `a(ua{sv})`.
pub(crate) fn streams_value(streams: &[(CaptureSource, Stream)]) -> Option<OwnedValue> {
    let streams: Vec<(u32, HashMap<String, OwnedValue>)> = streams
        .iter()
        .map(|(source, stream)| stream_tuple(source, stream))
        .collect();
    OwnedValue::try_from(Value::new(streams)).ok()
}

fn stream_tuple(source: &CaptureSource, stream: &Stream) -> (u32, HashMap<String, OwnedValue>) {
    let mut properties = HashMap::new();
    properties.insert(
        "id".to_owned(),
        OwnedValue::from(zbus::zvariant::Str::from(source.choice())),
    );
    properties.insert(
        "source_type".to_owned(),
        OwnedValue::from(source.kind.source_type()),
    );
    // The portal's sizes are signed: saturate rather than wrap.
    let size = (
        i32::try_from(stream.size.0).unwrap_or(i32::MAX),
        i32::try_from(stream.size.1).unwrap_or(i32::MAX),
    );
    if let Ok(size) = OwnedValue::try_from(Value::new(size)) {
        properties.insert("size".to_owned(), size);
    }
    // Only an output has a place in the layout.
    if source.kind == SourceKind::Output
        && let Some(position) = stream.position
        && let Ok(position) = OwnedValue::try_from(Value::new(position))
    {
        properties.insert("position".to_owned(), position);
    }
    (stream.node_id, properties)
}

/// The least significant bit of `mask`: a supported default without
/// inventing a capability.
const fn first_bit(mask: u32) -> u32 {
    mask & mask.wrapping_neg()
}

/// The selection `options` ask for of what `available` types and
/// `cursors` the session offers; `None` when they ask only for what it
/// cannot do.
pub(crate) fn selection_of(
    options: &HashMap<String, OwnedValue>,
    available: u32,
    cursors: u32,
) -> Option<Selection> {
    let types = option::<u32>(options, "types").unwrap_or_else(|| first_bit(available)) & available;
    let cursor =
        option::<u32>(options, "cursor_mode").unwrap_or_else(|| first_bit(cursors)) & cursors;
    let persist_mode = option::<u32>(options, "persist_mode").unwrap_or(0);
    (types != 0 && cursor != 0 && persist_mode <= 2).then(|| Selection {
        types,
        // One mode, the plainest the app allowed.
        cursor_mode: first_bit(cursor),
        multiple: option::<bool>(options, "multiple").unwrap_or(false),
        persist_mode,
        restore: None,
    })
}

/// The `org.freedesktop.impl.portal.ScreenCast` interface.
pub struct ScreenCast {
    portal: Portal,
}

impl ScreenCast {
    #[must_use]
    pub const fn new(portal: Portal) -> Self {
        Self { portal }
    }

    fn available_types(&self) -> u32 {
        crate::streams::source_types(&self.portal.streams.sources())
    }
}

#[interface(name = "org.freedesktop.impl.portal.ScreenCast")]
impl ScreenCast {
    #[zbus(property, name = "version")]
    const fn version(&self) -> u32 {
        5
    }

    #[zbus(property)]
    fn available_source_types(&self) -> u32 {
        self.available_types()
    }

    #[zbus(property)]
    fn available_cursor_modes(&self) -> u32 {
        self.portal.streams.cursor_modes() & cursor_mode::ALL
    }

    async fn create_session(
        &self,
        #[zbus(header)] header: zbus::message::Header<'_>,
        #[zbus(connection)] connection: &zbus::Connection,
        handle: ObjectPath<'_>,
        session_handle: ObjectPath<'_>,
        app_id: String,
        _options: HashMap<String, OwnedValue>,
    ) -> Reply {
        answer(async {
            let auth = self
                .portal
                .authenticate(&header, handle.as_str(), &app_id, None)
                .await?;
            self.portal
                .sessions
                .create(
                    connection,
                    session_handle.as_str(),
                    auth.owner.as_str(),
                    auth.caller,
                    SessionKind::ScreenCast,
                )
                .await?;
            Ok(Results::new())
        })
        .await
    }

    async fn select_sources(
        &self,
        #[zbus(header)] header: zbus::message::Header<'_>,
        handle: ObjectPath<'_>,
        session_handle: ObjectPath<'_>,
        app_id: String,
        options: HashMap<String, OwnedValue>,
    ) -> Reply {
        answer(async {
            let auth = self
                .portal
                .authenticate(&header, handle.as_str(), &app_id, None)
                .await?;
            let path = session_handle.as_str();
            let kind = self
                .portal
                .sessions
                .owned(path, auth.owner.as_str(), &auth.caller)
                .or_fail()?;
            ensure(matches!(
                kind,
                SessionKind::ScreenCast | SessionKind::RemoteDesktop
            ))?;
            let mut selection = selection_of(
                &options,
                self.available_source_types(),
                self.available_cursor_modes(),
            )
            .or_fail()?;
            if kind == SessionKind::ScreenCast {
                selection.restore = restore_key(&options)
                    .and_then(|key| self.portal.grants.get(&key, &auth.caller.app_id))
                    .filter(|grant| !grant.sources.is_empty());
                if let Some(restore) = &selection.restore {
                    selection.cursor_mode = restore.cursor_mode & self.available_cursor_modes();
                    ensure(selection.cursor_mode != 0)?;
                }
            } else {
                // A remote desktop remembers through its own devices call.
                selection.persist_mode = 0;
            }
            ensure(self.portal.casts.select(path, selection))?;
            Ok(Results::new())
        })
        .await
    }

    async fn start(
        &self,
        #[zbus(header)] header: zbus::message::Header<'_>,
        #[zbus(connection)] connection: &zbus::Connection,
        handle: ObjectPath<'_>,
        session_handle: ObjectPath<'_>,
        app_id: String,
        parent_window: String,
        _options: HashMap<String, OwnedValue>,
    ) -> Reply {
        answer(async {
            let auth = self
                .portal
                .authenticate(&header, handle.as_str(), &app_id, Some(&parent_window))
                .await?;
            let path = session_handle.as_str();
            let kind = self
                .portal
                .sessions
                .owned(path, auth.owner.as_str(), &auth.caller)
                .or_fail()?;
            ensure(kind == SessionKind::ScreenCast)?;
            let selection = self.portal.casts.selection(path).or_fail()?;
            ensure(self.portal.casts.begin(path))?;
            let request = handle.as_str();
            let shared = self
                .portal
                .requests
                .run(
                    connection,
                    request,
                    auth.owner.as_str(),
                    share(
                        &self.portal,
                        Ask {
                            path,
                            request,
                            caller: &auth.caller,
                            selection: Some(&selection),
                            restore: selection.restore.as_ref(),
                            kind: SCREENCAST_QUESTION,
                            devices: 0,
                        },
                    ),
                )
                .await;
            let shared = match shared {
                Ok(shared) => shared,
                Err(ended) => {
                    self.portal.casts.finish(path, None);
                    return Err(ended);
                }
            };
            let nodes: Vec<u32> = shared.streams.iter().map(|(_, s)| s.node_id).collect();
            if !self.portal.casts.finish(path, Some(nodes.clone())) {
                // The session ended while the streams started.
                for node in nodes {
                    self.portal.streams.stop(node);
                }
                return Err(Ended::Cancelled);
            }
            let mut results = Results::new();
            results.insert("streams".into(), streams_value(&shared.streams).or_fail()?);
            remember(
                &self.portal,
                &auth.caller,
                &shared,
                selection.cursor_mode,
                0,
                &mut results,
            )?;
            Ok(results)
        })
        .await
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::streams::source_type;

    fn output(name: &str) -> CaptureSource {
        CaptureSource {
            kind: SourceKind::Output,
            id: name.into(),
            app_id: None,
            label: name.into(),
        }
    }

    fn window(id: &str, app: &str) -> CaptureSource {
        CaptureSource {
            kind: SourceKind::Window,
            id: id.into(),
            app_id: Some(app.into()),
            label: format!("{app} window"),
        }
    }

    fn answered(uris: &[&str]) -> Answer {
        Answer {
            uris: uris.iter().map(|uri| (*uri).to_owned()).collect(),
            ..Answer::default()
        }
    }

    #[test]
    fn an_answer_picks_only_offered_sources_and_one_unless_multiple() {
        let offered = [output("DP-1"), output("HDMI-A-1"), window("w1", "org.a")];
        assert_eq!(
            picked(&answered(&["output:DP-1"]), &offered, false),
            Some(vec![output("DP-1")])
        );
        assert_eq!(
            picked(&answered(&["output:DP-1", "window:w1"]), &offered, false),
            None
        );
        assert_eq!(
            picked(&answered(&["output:DP-1", "window:w1"]), &offered, true)
                .map(|picked| picked.len()),
            Some(2)
        );
        assert_eq!(picked(&answered(&["output:DP-2"]), &offered, true), None);
        assert_eq!(picked(&answered(&[]), &offered, true), None);
    }

    #[test]
    fn a_restored_share_finds_its_outputs_by_name_and_windows_by_app() {
        let grant = Grant {
            id: String::new(),
            app_id: "org.example.Share".into(),
            persist_mode: 2,
            issued_unix_seconds: 0,
            sources: vec![output("DP-1"), window("old", "org.a")],
            cursor_mode: cursor_mode::HIDDEN,
            devices: 0,
        };
        let now = [output("DP-1"), window("new", "org.a")];
        assert_eq!(
            restored_sources(&grant, &now),
            Some(vec![output("DP-1"), window("new", "org.a")])
        );
        assert_eq!(restored_sources(&grant, &[output("DP-1")]), None);
    }

    #[test]
    fn selections_keep_to_what_the_session_offers() {
        let mut options = HashMap::new();
        let selection = selection_of(&options, 3, cursor_mode::ALL).expect("defaults");
        assert_eq!(
            (selection.types, selection.cursor_mode),
            (source_type::MONITOR, cursor_mode::HIDDEN)
        );
        options.insert("types".into(), OwnedValue::from(source_type::VIRTUAL));
        assert!(selection_of(&options, 3, cursor_mode::ALL).is_none());
        options.insert("types".into(), OwnedValue::from(3_u32));
        options.insert("cursor_mode".into(), OwnedValue::from(6_u32));
        let selection = selection_of(&options, 3, cursor_mode::ALL).expect("selected");
        assert_eq!((selection.types, selection.cursor_mode), (3, 2));
        options.insert("persist_mode".into(), OwnedValue::from(3_u32));
        assert!(selection_of(&options, 3, cursor_mode::ALL).is_none());
    }

    #[test]
    fn persistence_until_revoked_needs_the_persons_say() {
        assert_eq!(granted_persist_mode(2, true), 2);
        assert_eq!(granted_persist_mode(2, false), 1);
        assert_eq!(granted_persist_mode(1, false), 1);
        assert_eq!(granted_persist_mode(0, true), 0);
    }

    #[test]
    fn streams_carry_their_node_size_type_and_an_outputs_place() {
        let stream = Stream {
            node_id: 42,
            size: (640, 480),
            position: Some((100, 200)),
        };
        let (node, properties) = stream_tuple(&output("DP-1"), &stream);
        assert_eq!(node, 42);
        let size: (i32, i32) = properties["size"]
            .try_clone()
            .expect("clone")
            .try_into()
            .expect("(ii)");
        assert_eq!(size, (640, 480));
        assert!(properties.contains_key("position"));
        let (_, properties) = stream_tuple(&window("w", "org.a"), &stream);
        assert!(!properties.contains_key("position"), "monitors only");
        assert!(streams_value(&[(output("DP-1"), stream)]).is_some());
    }
}
