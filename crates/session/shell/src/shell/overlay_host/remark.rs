//! The remark surface's host side: what the composer needs from the
//! broker, the recorder, the review gateway and the record store, done on
//! the frame loop without ever blocking it.
//!
//! The composer's state lives in [`ChromeState::remark`] and changes only
//! through the `remark.*` shell actions. This pump turns their queued
//! requests into work (inspecting every app on screen for pick mode,
//! building the walk-through and asking the review model, delivering the
//! remark through chat's own `remark` action) and folds the answers back.

use std::collections::BTreeMap;
use std::sync::mpsc;
use std::time::{Duration, Instant};

use vello::kurbo::{Point, Rect};

use crate::shell::collab_broker::AppActionBroker;
use crate::shell::collab_broker_capnp::{InspectCaller, InspectionJob};
use crate::shell::remark::pick::{PickNode, PickScene, PickSurface};
use crate::shell::remark::review::{ModelUse, Review, SetUpChoice, Verdict};
use crate::shell::remark::view::{RemarkView, StateNode, StepFrame, StepState};
use crate::shell::remark::{
    CommunityRoom, DestinationKind, Directory, Flow, Request, Sending, Stage,
};
use crate::shell::state::WorkspaceAuthority as _;

use super::ShellOverlayHost;

/// The app every remark is delivered through.
pub const CHAT_APP: &str = "so.tilo.chat";
/// Chat's typed action that takes a remark from another app.
const CHAT_REMARK_ACTION: &str = "remark";
/// How often pick mode asks an app for its tree again.
const PICK_REFRESH: Duration = Duration::from_millis(400);
/// Depth of the subtree pick mode asks for.
const PICK_DEPTH: u16 = 32;
/// How long delivery waits for chat to open in the destination room.
const DELIVERY_TIMEOUT: Duration = Duration::from_secs(30);
/// How often the destination list is gathered again while open.
const DIRECTORY_REFRESH: Duration = Duration::from_secs(5);

/// One app surface pick mode is asking about.
struct InspectedSurface {
    app_id: String,
    app_label: String,
    room: String,
    record: String,
    screen: Rect,
    reply: Option<tilo_reference::inspect::InspectReply>,
    asked: Option<Instant>,
    job: Option<InspectionJob>,
    /// The app's semantic generation the last question was asked at.
    generation: Option<u64>,
}

/// What one batch's review came back with.
struct BatchAnswer {
    batch: usize,
    outcome: Result<tilo_model_gateway::session::ReviewOutcome, String>,
}

/// A remark on its way to chat.
struct DeliveryJob {
    delivery: crate::shell::remark::Delivery,
    me: String,
    started: Instant,
    launched: bool,
    invocation: Option<(tilo_actions::InvocationId, tilo_actions::remote::Caller)>,
    pending: Option<
        tilo_actions::remote::Pending<
            Result<tilo_actions::remote::InvocationReceipt, tilo_actions::remote::ActionHostError>,
        >,
    >,
    /// The persisted redacted duplicate this remark attaches.
    record: Option<String>,
    label: String,
}

/// A replay check waiting for the replay's frames.
struct ReplayCheck {
    check: u64,
    trace: session_trace::SessionTrace,
    frames: session_trace::frames::FrameStrip,
}

/// Host-side state of the remark surface.
#[derive(Default)]
pub(crate) struct RemarkHost {
    pub(crate) view: RemarkView,
    surfaces: BTreeMap<String, InspectedSurface>,
    last_stage: Stage,
    directory_at: Option<Instant>,
    review_answers: Option<mpsc::Receiver<BatchAnswer>>,
    /// How this device replays a session, when it can.
    replay: Option<std::sync::Arc<dyn crate::shell::remark::OffscreenReplay>>,
    /// The replay running for the session under review, until its frames
    /// are in.
    replay_check: Option<ReplayCheck>,
    /// The replay check of the session under review, while it runs.
    verdict_answer:
        Option<mpsc::Receiver<Result<session_trace::divergence::DivergenceReport, String>>>,
    review_states: Vec<StepState>,
    review_states_key: Option<(String, u32, usize)>,
    /// The session under review, so the preview follows what is hidden.
    review_trace: Option<(
        session_trace::SessionTrace,
        Option<session_trace::frames::FrameStrip>,
    )>,
    delivery: Option<DeliveryJob>,
    recording_since: Option<Instant>,
    last_second: Option<u64>,
    /// Destinations a headless render offers, where nothing configures any.
    fixture_directory: Option<Directory>,
    /// The shell's own semantic tree, for apps it hosts in-process.
    shell_tree: Option<ui::a11y_tree::SemanticTree>,
    shell_tree_at: Option<Instant>,
}

pub(super) fn app_label(app_id: &str) -> String {
    crate::shell::launcher::display_name_for_app_id(app_id).unwrap_or_else(|| {
        let tail = app_id.rsplit('.').next().unwrap_or(app_id);
        let mut chars = tail.chars();
        chars.next().map_or_else(String::new, |first| {
            first.to_uppercase().chain(chars).collect()
        })
    })
}

fn rect_of(rect: tilo_reference::inspect::SurfaceRect) -> Rect {
    Rect::new(rect.x, rect.y, rect.x + rect.width, rect.y + rect.height)
}

/// An app's answer as pick mode draws it.
fn pick_surface(key: &str, surface: &InspectedSurface) -> Option<PickSurface> {
    let reply = surface.reply.as_ref()?;
    let root_width = reply
        .nodes
        .first()
        .and_then(|node| node.bounds)
        .map_or(0.0, |bounds| bounds.width);
    let scale = if root_width > 0.0 {
        surface.screen.width() / root_width
    } else {
        1.0
    };
    let nodes = reply
        .nodes
        .iter()
        .filter_map(|node| {
            let bounds = node.visible.or(node.bounds)?;
            // A field goes by its name; only a node without one is shown by
            // what it holds.
            let name = node.label.clone().filter(|name| !name.trim().is_empty());
            let (label, shows_content) = match (&node.content, name) {
                (tilo_reference::inspect::Content::Value, Some(name)) => (Some(name), false),
                (tilo_reference::inspect::Content::Value, None) => {
                    (node.value.clone(), node.value.is_some())
                }
                (tilo_reference::inspect::Content::Label, name) => (name, true),
            };
            Some(PickNode {
                id: node.id.clone(),
                role: node.role.clone(),
                label,
                bounds: rect_of(bounds),
                children: node.children.clone(),
                reference: node.reference.clone(),
                personal: shows_content && node.sensitivity == tilo_actions::Sensitivity::Personal,
            })
        })
        .collect();
    Some(PickSurface {
        key: key.to_string(),
        app_id: surface.app_id.clone(),
        app_label: surface.app_label.clone(),
        room: surface.room.clone(),
        record: surface.record.clone(),
        screen: surface.screen,
        scale,
        nodes,
    })
}

/// An in-process app's nodes, out of the shell's own tree: every node
/// inside the app's tile, in tile coordinates.
fn in_process_surface(
    tree: &ui::a11y_tree::SemanticTree,
    key: String,
    app_id: &str,
    label: &str,
    room: &str,
    tile: Rect,
) -> PickSurface {
    let inside = |rect: Rect| {
        rect.x0 >= tile.x0 - 1.0
            && rect.y0 >= tile.y0 - 1.0
            && rect.x1 <= tile.x1 + 1.0
            && rect.y1 <= tile.y1 + 1.0
    };
    let kept: Vec<&ui::a11y_tree::SemanticNode> = tree
        .iter()
        .filter(|node| !node.state.hidden)
        .filter(|node| node.bounds.is_some_and(inside))
        .collect();
    let ids: std::collections::BTreeSet<&str> = kept.iter().map(|node| node.id.as_str()).collect();
    let nodes = kept
        .iter()
        .filter_map(|node| {
            let bounds = node.visible_bounds().or(node.bounds)?;
            // A field goes by its name; only a node without one is shown by
            // what it holds.
            let name = node.name.clone().filter(|name| !name.trim().is_empty());
            let (label, shows_content) = if node.presents_value() {
                match name {
                    Some(name) => (Some(name), false),
                    None => (node.value.clone(), node.value.is_some()),
                }
            } else {
                (name, true)
            };
            Some(PickNode {
                id: node.id.as_str().to_string(),
                role: ui::inspect::role_name(node.role),
                label,
                bounds: Rect::new(
                    bounds.x0 - tile.x0,
                    bounds.y0 - tile.y0,
                    bounds.x1 - tile.x0,
                    bounds.y1 - tile.y0,
                ),
                children: node
                    .children
                    .iter()
                    .map(|child| child.as_str().to_string())
                    .filter(|child| ids.contains(child.as_str()))
                    .collect(),
                reference: node.reference.clone(),
                personal: shows_content && node.sensitivity == tilo_actions::Sensitivity::Personal,
            })
        })
        .collect();
    PickSurface {
        key,
        app_id: app_id.to_string(),
        app_label: label.to_string(),
        room: room.to_string(),
        record: String::new(),
        screen: tile,
        scale: 1.0,
        nodes,
    }
}

impl ShellOverlayHost {
    /// Offer `directory`'s destinations in a headless render (`shots`),
    /// where no community is configured and nobody else is in a room.
    /// Let review check a recording's frames by replaying it.
    pub fn set_remark_replay(
        &mut self,
        replay: std::sync::Arc<dyn crate::shell::remark::OffscreenReplay>,
    ) {
        self.remark.replay = Some(replay);
    }

    /// Where a remark can go, for a session nothing configures: a
    /// headless render or a test, with no community or collaborators.
    pub fn seed_remark_directory(&mut self, directory: Directory) {
        self.remark.fixture_directory = Some(directory);
        self.remark.directory_at = None;
    }

    /// Whether pick mode wants the shell's own semantic tree this frame.
    pub(crate) fn remark_wants_shell_tree(&self) -> bool {
        self.chrome
            .as_ref()
            .is_some_and(|chrome| chrome.borrow().remark.stage == Stage::Picking)
            && self
                .remark
                .shell_tree_at
                .is_none_or(|at| at.elapsed() >= PICK_REFRESH)
    }

    pub(crate) fn set_remark_shell_tree(&mut self, tree: ui::a11y_tree::SemanticTree) {
        self.remark.shell_tree = Some(tree);
        self.remark.shell_tree_at = Some(Instant::now());
    }

    /// The apps this shell hosts in-process, with their tile rects.
    pub(crate) fn in_process_tiles(&self) -> Vec<(u64, String, String, Rect)> {
        let Some(chrome) = self.chrome.as_ref() else {
            return Vec::new();
        };
        let groups: Vec<(u64, u64, String, String)> = {
            let st = chrome.borrow();
            let tree = st.tree();
            let Some(workspace) = tree.desktop.active_workspace() else {
                return Vec::new();
            };
            let groups = workspace
                .strips
                .iter()
                .flat_map(|strip| strip.window_groups.iter())
                .filter_map(|group| {
                    let tab = group.tabs.iter().find(|tab| tab.id == group.active_tab)?;
                    if matches!(tab.tile, crate::shell::state::TileContent::Foreign { .. }) {
                        return None;
                    }
                    let (app_id, label) = crate::shell::remark::tab_app(tab)?;
                    Some((group.id, tab.id, app_id, label))
                })
                .collect();
            drop(tree);
            groups
        };
        groups
            .into_iter()
            .filter_map(|(group, tab, app_id, label)| {
                let rect = self.active_tile_rect(group)?;
                (rect.area() > 0.0).then_some((tab, app_id, label, rect))
            })
            .collect()
    }

    /// Frame-loop upkeep of the remark surface. Returns whether anything
    /// on screen changed.
    pub(super) fn pump_remark(&mut self) -> bool {
        let Some(chrome) = self.chrome.clone() else {
            return false;
        };
        let requests = chrome.borrow_mut().take_remark_requests();
        let mut dirty = !requests.is_empty();
        let stage = chrome.borrow().remark.stage;
        if stage != self.remark.last_stage {
            if matches!(stage, Stage::Compose | Stage::Review)
                && !matches!(self.remark.last_stage, Stage::Compose | Stage::Review)
                || stage == Stage::Review
            {
                self.remark.view.note_opened();
            }
            if stage == Stage::Closed {
                self.remark.surfaces.clear();
                self.remark.review_answers = None;
                self.remark.verdict_answer = None;
                self.remark.replay_check = None;
                self.remark.review_states.clear();
                self.remark.review_states_key = None;
                self.remark.review_trace = None;
                self.remark.view = RemarkView::default();
                self.remark.directory_at = None;
                self.remark.shell_tree = None;
                self.remark.shell_tree_at = None;
            }
            self.remark.last_stage = stage;
            dirty = true;
        }
        if stage == Stage::Closed {
            return dirty;
        }
        if self
            .remark
            .directory_at
            .is_none_or(|at| at.elapsed() >= DIRECTORY_REFRESH)
        {
            self.remark.directory_at = Some(Instant::now());
            let directory = self
                .remark
                .fixture_directory
                .clone()
                .unwrap_or_else(|| self.gather_remark_directory());
            let mut st = chrome.borrow_mut();
            if st.remark.destination.is_none() {
                st.remark.destination = directory.default_destination();
            }
            st.set_remark_directory(directory);
        }
        let mut refresh = false;
        for request in requests {
            match request {
                Request::RefreshPick => refresh = true,
                Request::OpenReview => self.open_remark_review(),
                Request::SetUpReviewModel => self.set_up_review_model(),
                Request::ConfirmReviewModel { action } => self.confirm_review_model(&action),
                Request::Send => self.start_remark_delivery(),
            }
        }
        if stage == Stage::Picking {
            dirty |= self.refresh_pick_scene(refresh);
        }
        dirty |= self.pump_remark_flow();
        dirty |= self.pump_review_answers();
        dirty |= self.pump_review_verdict();
        dirty |= self.refresh_review_states();
        dirty |= self.pump_remark_delivery();
        dirty
    }

    /// Where a remark can go: the Tilo team and the community from this
    /// device's community configuration, then every shared workspace and
    /// every person or agent in one.
    fn gather_remark_directory(&self) -> Directory {
        let mut directory = Directory::default();
        let config = crate::shell::community::load().unwrap_or_default();
        if let Some(team) = &config.team {
            directory.team = Some((team.person_id(), team.label.clone()));
        }
        if let Some(invite) = &config.invite
            && let Ok(token) = crate::network::invite::validate_invite_token(invite)
            && token.standing
        {
            let joined = self
                .community_request(crate::shell::peer_sync_overlay::CommunityRequest::Status)
                .ok()
                .is_some_and(|status| status.get("role").is_some_and(|role| !role.is_null()));
            directory.community = Some(CommunityRoom {
                room: token.room_id,
                channel: tilo_reference::FEEDBACK_CHANNEL.to_string(),
                label: crate::shell::i18n::t("remark-community-label"),
                joined,
                left: settings_store::community::membership_in(&settings_store::load())
                    == settings_store::community::Membership::Left,
            });
        }
        let Some(chrome) = self.chrome.as_ref() else {
            return directory;
        };
        let st = chrome.borrow();
        let tree = st.tree();
        let me = st.active_local_user();
        let team = directory.team.as_ref().map(|(id, _)| id.clone());
        for workspace in &tree.desktop.workspaces {
            let agents = workspace.agent_members();
            let others: Vec<&crate::shell::state::Collaborator> = workspace
                .collaborators
                .iter()
                .filter(|person| person.id != me && !person.invited)
                .filter(|person| person.id != crate::shell::recorder::RECORDER_AGENT)
                .collect();
            if others.is_empty() || workspace.room.is_nil() {
                continue;
            }
            let room = workspace.room.to_string();
            let is_direct = others.len() == 1
                && others.first().is_some_and(|other| {
                    tilo_reference::DirectRoom::between(&me, &other.id)
                        .is_some_and(|direct| direct.room_id() == room)
                });
            if !is_direct && directory.community.as_ref().is_none_or(|c| c.room != room) {
                directory.rooms.push((room, workspace.name.clone()));
            }
            for person in others {
                if Some(&person.id) == team.as_ref()
                    || directory.people.iter().any(|known| known.id == person.id)
                {
                    continue;
                }
                let is_agent = agents
                    .iter()
                    .any(|(agent, _)| agent.eq_ignore_ascii_case(&person.id));
                let name = if person.name.trim().is_empty() {
                    person.id.clone()
                } else {
                    person.name.clone()
                };
                directory.people.push(tilo_reference::RoomMember::new(
                    person.id.clone(),
                    name,
                    is_agent,
                ));
            }
        }
        drop(tree);
        drop(st);
        directory
    }

    /// Ask every app on screen for its tree, and fold the answers into the
    /// pick scene. Returns whether the scene changed.
    #[allow(
        clippy::too_many_lines,
        reason = "one pass: which apps are on screen, asking them, folding the answers in"
    )]
    fn refresh_pick_scene(&mut self, force: bool) -> bool {
        let Some(chrome) = self.chrome.clone() else {
            return false;
        };
        let broker = self.app_action_broker.clone();
        let (tiles, room, person, tabs) = {
            let st = chrome.borrow();
            let tree = st.tree();
            let tiles = crate::shell::input_policy::foreign_tiles(&tree, self);
            let Some(workspace) = tree.desktop.active_workspace() else {
                return false;
            };
            let room = workspace.room.to_string();
            let person = st.active_local_user();
            let tabs: BTreeMap<u64, String> = workspace
                .strips
                .iter()
                .flat_map(|strip| strip.window_groups.iter())
                .flat_map(|group| group.tabs.iter())
                .filter_map(|tab| match &tab.tile {
                    crate::shell::state::TileContent::Foreign { app_id, surface_id } => {
                        Some((*surface_id, app_id.clone()))
                    }
                    _ => None,
                })
                .collect();
            drop(tree);
            drop(st);
            (tiles, room, person, tabs)
        };
        let apps = broker
            .as_ref()
            .map(AppActionBroker::inspectable_apps)
            .unwrap_or_default();
        let caller = InspectCaller {
            person,
            audience: ui::a11y::Audience::LocalSession,
        };
        let mut live: Vec<String> = Vec::new();
        for tile in &tiles {
            let Some(app_id) = tabs.get(&tile.surface_id) else {
                continue;
            };
            // The launch a window and an app connection both descend from
            // says which connection draws it; a window the shell did not
            // launch is matched by its app and room.
            let launched = broker
                .as_ref()
                .and_then(|broker| broker.inspectable_app_for_surface(tile.surface_id));
            let Some(app) = launched.or_else(|| {
                apps.iter()
                    .find(|app| app.room == room && app.app_id == *app_id)
                    .cloned()
            }) else {
                continue;
            };
            let key = format!("surface:{}", tile.surface_id);
            live.push(key.clone());
            let entry = self
                .remark
                .surfaces
                .entry(key)
                .or_insert_with(|| InspectedSurface {
                    app_id: app.app_id.clone(),
                    app_label: app_label(&app.app_id),
                    room: room.clone(),
                    record: app.record.clone(),
                    screen: tile.rect,
                    reply: None,
                    asked: None,
                    job: None,
                    generation: None,
                });
            entry.screen = tile.rect;
            // Ask again when the app says its tree changed, and now and then
            // regardless, so a missed change never leaves a stale outline.
            let changed = entry.generation != Some(app.semantic_generation);
            let stale = entry
                .asked
                .is_none_or(|at| at.elapsed() >= PICK_REFRESH.saturating_mul(5));
            if entry.job.is_none()
                && (force || changed || stale)
                && let Some(broker) = broker.as_ref()
            {
                match broker.inspect(
                    app.peer_id,
                    ui::inspect::InspectQuery::Subtree {
                        root: None,
                        depth: PICK_DEPTH,
                    },
                    &caller,
                ) {
                    Ok(job) => entry.job = Some(job),
                    Err(error) => tracing::debug!(%error, "pick: an app cannot be inspected"),
                }
                entry.asked = Some(Instant::now());
                entry.generation = Some(app.semantic_generation);
            }
        }
        drop(broker);
        self.remark.surfaces.retain(|key, _| live.contains(key));
        let mut answered = false;
        for surface in self.remark.surfaces.values_mut() {
            let Some(job) = surface.job.as_ref() else {
                continue;
            };
            match job.try_answer() {
                None => {}
                Some(Ok(reply)) => {
                    answered |= surface.reply.as_ref() != Some(&reply);
                    surface.reply = Some(reply);
                    surface.job = None;
                }
                Some(Err(error)) => {
                    tracing::debug!(%error, "pick: an app did not answer");
                    surface.job = None;
                }
            }
        }
        let mut surfaces: Vec<PickSurface> = self
            .remark
            .surfaces
            .iter()
            .filter_map(|(key, surface)| pick_surface(key, surface))
            .collect();
        if let Some(tree) = self.remark.shell_tree.as_ref() {
            for (tab, app_id, label, rect) in self.in_process_tiles() {
                let surface =
                    in_process_surface(tree, format!("tab:{tab}"), &app_id, &label, &room, rect);
                if !surface.nodes.is_empty() {
                    surfaces.push(surface);
                }
            }
        }
        let scene = PickScene { surfaces };
        let mut st = chrome.borrow_mut();
        if st.pick.scene == scene {
            return false;
        }
        st.pick.set_scene(scene);
        let _ = answered;
        true
    }

    /// Keep the composer's recording in step with the recorder: a capture
    /// that finished, or a recording revoked from Workspace Settings.
    fn pump_remark_flow(&mut self) -> bool {
        let Some(chrome) = self.chrome.clone() else {
            return false;
        };
        let (flow, known) = {
            let st = chrome.borrow();
            (st.remark.flow.clone(), st.remark.known_sessions.clone())
        };
        // The pill's clock runs from when this session saw the recording
        // start, whether or not a recorder is attached to read it from.
        if matches!(flow, Flow::Recording { .. }) {
            self.remark.recording_since.get_or_insert_with(Instant::now);
        }
        // Whether it still records, then the newest session sealed since the
        // composer opened: a recording that stopped has sealed by now.
        let Some((recording, newest)) = self.session_services.recorder().map(|recorder| {
            let recording = recorder.is_recording();
            let newest = recorder
                .awaiting_review()
                .into_iter()
                .rev()
                .find(|(checkout, _)| !known.iter().any(|id| id == checkout.id.as_str()));
            (recording, newest)
        }) else {
            return false;
        };
        match flow {
            Flow::Capturing => {
                if let Some((checkout, summary)) = newest {
                    chrome.borrow_mut().remark.flow = Flow::Attached {
                        record: checkout.id.as_str().to_string(),
                        summary,
                    };
                    return true;
                }
                false
            }
            Flow::Recording { .. } => {
                let since = *self.remark.recording_since.get_or_insert_with(Instant::now);
                if !recording {
                    self.remark.recording_since = None;
                    let mut st = chrome.borrow_mut();
                    st.remark.flow =
                        newest.map_or(Flow::None, |(checkout, summary)| Flow::Attached {
                            record: checkout.id.as_str().to_string(),
                            summary,
                        });
                    if st.remark.stage == Stage::Recording {
                        st.remark.stage = Stage::Compose;
                    }
                    return true;
                }
                let second = since.elapsed().as_secs();
                let changed = self.remark.last_second != Some(second);
                self.remark.last_second = Some(second);
                changed
            }
            Flow::None | Flow::Attached { .. } => {
                self.remark.recording_since = None;
                false
            }
        }
    }

    /// How long the composer's recording has run, for its clock.
    pub(super) fn remark_recording_ms(&self) -> Option<u64> {
        self.remark
            .recording_since
            .map(|since| u64::try_from(since.elapsed().as_millis()).unwrap_or(u64::MAX))
    }

    /// Build the walk-through of the attached session and ask the review
    /// model about each of its batches, off the frame loop.
    fn open_remark_review(&mut self) {
        let Some(chrome) = self.chrome.clone() else {
            return;
        };
        let record = {
            let st = chrome.borrow();
            let Flow::Attached { record, .. } = &st.remark.flow else {
                return;
            };
            record.clone()
        };
        let Some(id) = ui::record::RecordId::parse(&record) else {
            return;
        };
        let trace = self
            .session_services
            .recorder()
            .and_then(|recorder| {
                recorder.with_pending(&id, crate::shell::recorder::PendingSession::trace)
            })
            .and_then(Result::ok);
        let Some(trace) = trace else {
            chrome.borrow_mut().remark.notice = Some(crate::shell::remark::Notice {
                key: "remark-review-empty",
                detail: None,
            });
            return;
        };
        let frames = self
            .session_services
            .recorder()
            .and_then(|recorder| recorder.with_pending(&id, |pending| pending.frames().cloned()))
            .flatten();
        self.begin_remark_review(&record, &trace, frames);
    }

    /// Walk through `trace` (the session `record`, with the `frames` kept
    /// of it), reviewed by the floor at once and by the review model as it
    /// answers.
    pub fn begin_remark_review(
        &mut self,
        record: &str,
        trace: &session_trace::SessionTrace,
        frames: Option<session_trace::frames::FrameStrip>,
    ) {
        let Some(chrome) = self.chrome.clone() else {
            return;
        };
        let mut review = Review::from_trace(record, trace, &app_label);
        review.verdict = self.start_replay_check(record, trace, frames.as_ref());
        let requests: Vec<(usize, tilo_feedback_review::ReviewTraceRequest)> = review
            .batches()
            .iter()
            .enumerate()
            .map(|(index, batch)| (index, batch.request.clone()))
            .collect();
        self.remark.review_states = step_states(&review, trace, frames.as_ref());
        self.remark.review_trace = Some((trace.clone(), frames));
        chrome.borrow_mut().remark.review = Some(review);
        let (send, receive) = mpsc::channel();
        self.remark.review_answers = Some(receive);
        if requests.is_empty() {
            return;
        }
        let socket = launcher::services::socket_for(launcher::services::MODEL_GATEWAY_SERVICE);
        let spawned = std::thread::Builder::new()
            .name("remark-review".into())
            .spawn(move || {
                for (batch, request) in requests {
                    let outcome = tilo_model_gateway::session::review(&socket, &request)
                        .map_err(|error| error.to_string());
                    if send.send(BatchAnswer { batch, outcome }).is_err() {
                        return;
                    }
                }
            });
        if let Err(error) = spawned {
            tracing::warn!(%error, "review: could not start the review worker; the floor stands");
        }
    }

    /// Check the recording's frames against a replay of it, once per
    /// session and off the frame loop. A session checked before keeps its
    /// report.
    fn start_replay_check(
        &mut self,
        record: &str,
        trace: &session_trace::SessionTrace,
        frames: Option<&session_trace::frames::FrameStrip>,
    ) -> Verdict {
        let checked = self
            .session_services
            .recorder()
            .cloned()
            .zip(ui::record::RecordId::parse(record))
            .and_then(|(recorder, id)| {
                recorder.with_pending(&id, |pending| pending.divergence().cloned())
            })
            .flatten();
        if let Some(report) = checked {
            return Verdict::Checked(report);
        }
        let Some(strip) = frames else {
            return Verdict::Unchecked;
        };
        let (Some(replay), Some(id)) = (
            self.remark.replay.clone(),
            ui::record::RecordId::parse(record),
        ) else {
            return Verdict::Unavailable;
        };
        match replay.start(&id, strip) {
            Ok(check) => {
                self.remark.replay_check = Some(ReplayCheck {
                    check,
                    trace: trace.clone(),
                    frames: strip.clone(),
                });
                Verdict::Checking
            }
            Err(reason) => {
                tracing::info!(%reason, "review: this session cannot be replayed here");
                Verdict::Unavailable
            }
        }
    }

    /// Once the replay's frames are in, compare them with the recorded
    /// ones off the frame loop: decoding and comparing is the costly part.
    fn compare_replayed_frames(&mut self) -> Option<Result<(), String>> {
        let replay = self.remark.replay.clone()?;
        let pending = self.remark.replay_check.as_ref()?;
        let collected = replay.take(pending.check)?;
        let ReplayCheck { trace, frames, .. } = self.remark.replay_check.take()?;
        let mut provider = match collected {
            Ok(provider) => provider,
            Err(reason) => return Some(Err(reason)),
        };
        let (send, receive) = mpsc::channel();
        let spawned = std::thread::Builder::new()
            .name("remark-replay-check".into())
            .spawn(move || {
                let outcome = session_trace::divergence::check(
                    &trace,
                    &frames,
                    provider.as_mut(),
                    &session_trace::divergence::Tolerance::default(),
                )
                .map_err(|error| error.to_string());
                let _ = send.send(outcome);
            });
        Some(match spawned {
            Ok(_) => {
                self.remark.verdict_answer = Some(receive);
                Ok(())
            }
            Err(error) => Err(error.to_string()),
        })
    }

    /// Take the replay check's answer: onto the review, and onto the
    /// pending session so it is not checked again and is summarized with
    /// the layer found.
    fn pump_review_verdict(&mut self) -> bool {
        let outcome = if let Some(Err(reason)) = self.compare_replayed_frames() {
            Err(reason)
        } else {
            let Some(receive) = self.remark.verdict_answer.as_ref() else {
                return false;
            };
            match receive.try_recv() {
                Ok(outcome) => outcome,
                Err(mpsc::TryRecvError::Empty) => return false,
                Err(mpsc::TryRecvError::Disconnected) => {
                    Err("the replay check stopped".to_string())
                }
            }
        };
        self.remark.verdict_answer = None;
        let Some(chrome) = self.chrome.clone() else {
            return false;
        };
        let mut st = chrome.borrow_mut();
        let Some(review) = st.remark.review.as_mut() else {
            return false;
        };
        review.verdict = match outcome {
            Ok(report) => {
                if let Some((recorder, id)) = self
                    .session_services
                    .recorder()
                    .zip(ui::record::RecordId::parse(&review.record))
                {
                    recorder.record_divergence(&id, report.clone());
                }
                Verdict::Checked(report)
            }
            Err(reason) => {
                tracing::warn!(%reason, "review: the replay check did not finish");
                Verdict::Failed(reason)
            }
        };
        st.bump();
        drop(st);
        // The preview marks where replay differs.
        self.remark.review_states_key = None;
        true
    }

    /// Draw the preview again when what is hidden or cut changed.
    fn refresh_review_states(&mut self) -> bool {
        let Some(chrome) = self.chrome.clone() else {
            return false;
        };
        let st = chrome.borrow();
        let Some(review) = st.remark.review.as_ref() else {
            return false;
        };
        let key = (
            review.record.clone(),
            review.redaction_count(),
            review.cut.len(),
        );
        if self.remark.review_states_key.as_ref() == Some(&key) {
            return false;
        }
        let Some((trace, frames)) = self.remark.review_trace.as_ref() else {
            return false;
        };
        self.remark.review_states = step_states(review, trace, frames.as_ref());
        drop(st);
        self.remark.review_states_key = Some(key);
        true
    }

    /// Fold the review model's answers into the walk-through.
    fn pump_review_answers(&self) -> bool {
        let Some(chrome) = self.chrome.clone() else {
            return false;
        };
        let Some(receive) = self.remark.review_answers.as_ref() else {
            return false;
        };
        let mut changed = false;
        while let Ok(answer) = receive.try_recv() {
            let mut st = chrome.borrow_mut();
            let Some(review) = st.remark.review.as_mut() else {
                continue;
            };
            changed = true;
            match answer.outcome {
                Ok(outcome) => {
                    use tilo_model_gateway::session::ModelContribution;
                    review.apply_model(answer.batch, outcome.proposals);
                    review.model = match outcome.model {
                        ModelContribution::Merged(model) => ModelUse::OnDevice {
                            model: model.model_id,
                        },
                        ModelContribution::NoLocalModel => ModelUse::Floor,
                        ModelContribution::Failed(tilo_model_gateway::ClientError::Unavailable) => {
                            ModelUse::Floor
                        }
                        ModelContribution::Failed(error) => ModelUse::Failed {
                            reason: error.to_string(),
                        },
                    };
                }
                Err(reason) => {
                    review.model = ModelUse::Failed { reason };
                }
            }
        }
        changed
    }

    /// The AI hub's ways to set up the review model here, recommendation
    /// first: its `set-up-on-device` action on the review task and each
    /// `set-up-on-device-<rank>` after it, each with the hub's own words.
    fn review_model_offers(&self) -> Vec<(u64, String, SetUpChoice)> {
        const RESOURCE: &str = "settings.ai-task:feedback.review";
        const ACTION: &str = "set-up-on-device";
        let Some(broker) = self.app_action_broker.as_ref() else {
            return Vec::new();
        };
        let rank = |action: &str| -> Option<usize> {
            if action == ACTION {
                return Some(0);
            }
            action
                .strip_prefix(ACTION)?
                .strip_prefix('-')?
                .parse::<usize>()
                .ok()
                .filter(|rank| *rank > 0)
        };
        let tag = |tags: &[String], prefix: &str| {
            tags.iter().find(|tag| tag.starts_with(prefix)).cloned()
        };
        let mut offers: Vec<(usize, u64, String, SetUpChoice)> = broker
            .surfaces()
            .into_iter()
            .flat_map(|surface| {
                let peer = surface.peer_id;
                let room = surface.route.cluster_id.clone();
                surface
                    .actions
                    .into_iter()
                    .filter(|action| action.resource == RESOURCE)
                    .filter_map(move |action| {
                        let rank = rank(&action.action)?;
                        let tags = &action.contract.tags;
                        Some((
                            rank,
                            peer,
                            room.clone(),
                            SetUpChoice {
                                fit: tag(tags, "fit-"),
                                speed: tag(tags, "speed-"),
                                action: action.action,
                                label: action.action_label,
                                terms: action.description.unwrap_or_default(),
                            },
                        ))
                    })
            })
            .collect();
        offers.sort_by_key(|(rank, ..)| *rank);
        offers
            .into_iter()
            .map(|(_, peer, room, choice)| (peer, room, choice))
            .collect()
    }

    /// Offer to set up the review model in place: what the hub recommends
    /// for this device, and its alternatives. Nothing downloads until the
    /// person confirms one.
    fn set_up_review_model(&self) {
        let Some(chrome) = self.chrome.clone() else {
            return;
        };
        let choices: Vec<SetUpChoice> = self
            .review_model_offers()
            .into_iter()
            .map(|(_, _, choice)| choice)
            .collect();
        if let Some(review) = chrome.borrow_mut().remark.review.as_mut() {
            review.model = if choices.is_empty() {
                ModelUse::Floor
            } else {
                ModelUse::Offer {
                    choices,
                    expanded: false,
                }
            };
        }
    }

    /// Set up the choice the person confirmed, through the hub's own action:
    /// the confirmation is theirs, given on the offer that showed its size,
    /// license, fit and speed.
    fn confirm_review_model(&self, action: &str) {
        const RESOURCE: &str = "settings.ai-task:feedback.review";
        let Some(chrome) = self.chrome.clone() else {
            return;
        };
        let Some(broker) = self.app_action_broker.clone() else {
            return;
        };
        // The hub is asked again: the choice must still be one it offers.
        let offered = self
            .review_model_offers()
            .into_iter()
            .find(|(_, _, choice)| choice.action == action);
        let Some((peer, room, choice)) = offered else {
            if let Some(review) = chrome.borrow_mut().remark.review.as_mut() {
                review.model = ModelUse::Floor;
            }
            return;
        };
        let me = chrome.borrow().active_local_user();
        let host = crate::shell::remote_actions::BrokerActionHost::new(broker);
        let caller = tilo_actions::remote::Caller::local_person(me).in_room(room);
        let request = tilo_actions::remote::InvokeRequest {
            invocation: tilo_actions::InvocationId::new(),
            target: tilo_actions::remote::RemoteActionTarget {
                provider: peer.to_string(),
                address: tilo_actions::ActionAddress::new(RESOURCE, action),
            },
            argument: tilo_actions::remote::InvokeArgument::None,
            context: None,
            confirmation: Some(tilo_actions::remote::Confirmation::Confirmed),
            revision: None,
        };
        drop(tilo_actions::remote::ActionHost::invoke(
            &host, &caller, request,
        ));
        if let Some(review) = chrome.borrow_mut().remark.review.as_mut() {
            review.model = ModelUse::SettingUp {
                model: choice.label,
                progress: None,
            };
        }
    }

    /// Hand the remark to chat: open its conversation, persist the redacted
    /// recording, and start delivery.
    #[allow(
        clippy::too_many_lines,
        reason = "one ordered hand-off: open the conversation, persist the recording, address the remark"
    )]
    fn start_remark_delivery(&mut self) {
        let Some(chrome) = self.chrome.clone() else {
            return;
        };
        let fail = |chrome: &crate::shell::chrome_state::Chrome, reason: String| {
            let mut st = chrome.borrow_mut();
            st.remark.sending = Sending::Failed { reason };
            st.remark.stage = Stage::Compose;
            st.bump();
        };
        let (option, remark_text, me) = {
            let st = chrome.borrow();
            let options = st.remark_directory().options();
            let option = st
                .remark
                .destination
                .as_deref()
                .and_then(|id| options.into_iter().find(|option| option.id == id));
            (option, st.remark.text.clone(), st.active_local_user())
        };
        let Some(option) = option else {
            fail(
                &chrome,
                crate::shell::i18n::t("remark-blocked-no-destination"),
            );
            return;
        };
        // Open the conversation it goes to, before anything is written.
        let opened = match &option.kind {
            DestinationKind::Team { .. } => self
                .community_request(crate::shell::peer_sync_overlay::CommunityRequest::OpenTeamRoom)
                .map(drop),
            DestinationKind::Person { person, .. } => self
                .community_request(
                    crate::shell::peer_sync_overlay::CommunityRequest::OpenDirectRoom {
                        person: person.clone(),
                        label: option.label.clone(),
                    },
                )
                .map(drop),
            DestinationKind::Community { joined: false, .. } => self
                .community_request(crate::shell::peer_sync_overlay::CommunityRequest::Join)
                .map(drop),
            DestinationKind::Community { .. } | DestinationKind::Room { .. } => Ok(()),
        };
        if let Err(error) = opened {
            tracing::debug!(%error, "remark: the conversation could not be opened here; delivering to it as it is");
        }
        // Named before anything is written: the recording that travels with
        // the message is a record named after it.
        let id = format!("m_{}", uuid::Uuid::new_v4().simple());
        let recording = crate::shell::record_custody::device_public_key()
            .and_then(|device| crate::shell::remark::recording_record(&option, &id, device));
        let attachment = match self.persist_redacted_session(recording) {
            Ok(attachment) => attachment,
            Err(reason) => {
                fail(&chrome, reason);
                return;
            }
        };
        let record = attachment
            .as_ref()
            .map(|attachment| attachment.record.clone());
        let remark = {
            let st = chrome.borrow();
            let members = st.remark_directory().people.clone();
            let also: Vec<tilo_reference::ResourceRef> = st
                .remark
                .picks
                .iter()
                .skip(1)
                .map(|pick| pick.outbound_reference(option.leaves_the_workspace()))
                .collect();
            st.remark
                .remark(&option, &members, attachment)
                .map(|remark| remark.with_also(also))
        };
        let Some(remark) = remark else {
            fail(
                &chrome,
                crate::shell::i18n::t("remark-blocked-nothing-picked"),
            );
            return;
        };
        let remark = match chrome.borrow().remark.review.as_ref() {
            Some(review) => crate::shell::remark::hide_reviewed(
                remark,
                &session_trace::redact::Masker::new(review.hidden_values()),
            ),
            None => remark,
        };
        let ts = tilo_clock::unix_now_i64();
        let delivery = match crate::shell::remark::plan_delivery(&remark, &option, &me, &id, ts) {
            Ok(delivery) => delivery,
            Err(error) => {
                fail(&chrome, error.to_string());
                return;
            }
        };
        let _ = remark_text;
        self.remark.delivery = Some(DeliveryJob {
            delivery,
            me,
            started: Instant::now(),
            launched: false,
            invocation: None,
            pending: None,
            record,
            label: option.label,
        });
        let mut st = chrome.borrow_mut();
        st.remark.sending = Sending::InFlight {
            step: "opening".into(),
        };
        st.bump();
    }

    /// Build the redacted duplicate of the attached session, persist it in
    /// this person's own room, and name it for the attachment. `None` when
    /// the remark has no recording.
    fn persist_redacted_session(
        &self,
        recording: Option<ui::record::RecordId>,
    ) -> Result<Option<tilo_reference::RecordAttachment>, String> {
        let Some(chrome) = self.chrome.as_ref() else {
            return Ok(None);
        };
        let st = chrome.borrow();
        let Flow::Attached { record, .. } = &st.remark.flow else {
            return Ok(None);
        };
        let review = st
            .remark
            .review
            .as_ref()
            .ok_or_else(|| crate::shell::i18n::t("remark-blocked-review"))?;
        let id = ui::record::RecordId::parse(record)
            .ok_or_else(|| "the recording's id does not parse".to_string())?;
        let trace = self
            .session_services
            .recorder()
            .ok_or_else(|| "this session has no recorder".to_string())?
            .with_pending(&id, crate::shell::recorder::PendingSession::trace)
            .ok_or_else(|| "the recording is no longer in memory".to_string())?
            .map_err(|error| error.to_string())?;
        // What review hid in each keyframe is addressed to the text the app
        // showed; mask it there before the duplicate redacts the keyframes
        // for leaving the device, and leave out what the app refused.
        let trace = located_redactions(trace, &review.located(), &review.input.refused_keyframes);
        // Every value review hid, wherever the duplicate meets it: in the
        // documents, presence, labels and the keyframes' texts.
        let redaction = session_trace::redact::Redaction {
            masker: session_trace::redact::Masker::new(review.hidden_values()),
            cut: review.cut_invocations(),
        };
        let (mut duplicate, _report) = session_trace::redact::duplicate(&trace, &redaction)
            .map_err(|error| error.to_string())?;
        // What replay found is what the session is summarized as.
        if let Some(layer) = review.verdict.layer() {
            duplicate.layer = layer;
        }
        duplicate.redactions = session_trace::Redactions {
            text: review.redaction_count(),
            regions: 0,
        };
        let summary = review.summary(duplicate.layer);
        let checkout = ui::record::RecordCheckout::new(
            recording.ok_or_else(|| crate::shell::i18n::t("remark-blocked-no-identity"))?,
            ui::record::RecordKind::new(session_trace::SESSION_RECORD_KIND),
        );
        let mut reservation = st
            .records()
            .reserve_record(&checkout)
            .map_err(|error| format!("the recording could not be saved: {error:?}"))?;
        reservation
            .initialize(|doc| {
                ui::record::add_reference(
                    doc,
                    &ui::record::RecordRef::new(id.clone(), session_trace::REDACTED_FROM_RELATION),
                );
                session_trace::write(doc, &duplicate).map_err(|error| error.to_string())
            })
            .map_err(|error| format!("the recording could not be saved: {error:?}"))?;
        let label = st
            .remark
            .primary()
            .map_or_else(String::new, |pick| pick.app_label.clone());
        Ok(Some(tilo_reference::RecordAttachment::session(
            checkout.id.as_str(),
            label,
            launcher::appdir::PERSONAL_ROOM,
            summary,
        )))
    }

    /// Drive a delivery: find chat in the destination room (opening it
    /// there when it is not running), hand it the remark, and wait for its
    /// answer.
    #[allow(clippy::too_many_lines, reason = "one ordered delivery state machine")]
    fn pump_remark_delivery(&mut self) -> bool {
        use tilo_actions::remote::{ActionHost as _, PendingPoll, ReceiptStatus};
        let Some(chrome) = self.chrome.clone() else {
            return false;
        };
        let Some(job) = self.remark.delivery.as_mut() else {
            return false;
        };
        let Some(broker) = self.app_action_broker.clone() else {
            let reason = "this session has no broker to deliver through".to_string();
            self.remark.delivery = None;
            let mut st = chrome.borrow_mut();
            st.remark.sending = Sending::Failed { reason };
            st.remark.stage = Stage::Compose;
            return true;
        };
        let host = crate::shell::remote_actions::BrokerActionHost::new(broker.clone());
        let room = job.delivery.room.clone();
        let resource = format!("chat.channel:{}", job.delivery.channel);
        let mut outcome: Option<Result<(), String>> = None;
        if let Some(pending) = job.pending.as_mut() {
            match pending.poll_ready() {
                PendingPoll::Waiting => {}
                PendingPoll::Abandoned => outcome = Some(Err("chat did not answer".into())),
                PendingPoll::Ready(Err(error)) => outcome = Some(Err(error.to_string())),
                PendingPoll::Ready(Ok(receipt)) => match receipt.status {
                    ReceiptStatus::Completed { .. } => outcome = Some(Ok(())),
                    ReceiptStatus::Accepted => job.pending = None,
                    ReceiptStatus::Failed { error } => outcome = Some(Err(format!("{error:?}"))),
                    ReceiptStatus::Rejected { error } => outcome = Some(Err(error.to_string())),
                    ReceiptStatus::Cancelled { message } => {
                        outcome = Some(Err(message.unwrap_or_else(|| "cancelled".into())));
                    }
                },
            }
        } else if let Some((invocation, caller)) = job.invocation.as_ref() {
            if let Some(state) = host.observe(caller, invocation)
                && state.is_terminal()
            {
                outcome = Some(match state.receipt().status {
                    ReceiptStatus::Completed { .. } => Ok(()),
                    other => Err(format!("{other:?}")),
                });
            }
        } else {
            let caller =
                tilo_actions::remote::Caller::local_person(job.me.clone()).in_room(room.clone());
            // A remark is the author's own message, so it goes to the Chat
            // that runs for the author, never to another participant's.
            let author = crate::shell::collab_broker_capnp::BrokerActor::person(job.me.clone());
            let chat = broker.surfaces().into_iter().find(|surface| {
                surface.live
                    && surface.app_id == CHAT_APP
                    && surface.route.cluster_id == room
                    && broker.state().performs_for(surface.peer_id, &author)
                    && surface.actions.iter().any(|action| {
                        action.resource == resource && action.action == CHAT_REMARK_ACTION
                    })
            });
            if let Some(chat) = chat {
                let invocation = tilo_actions::InvocationId::new();
                let serde_json::Value::Object(arguments) = job.delivery.arguments() else {
                    return false;
                };
                let request = tilo_actions::remote::InvokeRequest {
                    invocation,
                    target: tilo_actions::remote::RemoteActionTarget {
                        provider: chat.peer_id.to_string(),
                        address: tilo_actions::ActionAddress::new(resource, CHAT_REMARK_ACTION),
                    },
                    argument: tilo_actions::remote::InvokeArgument::Json(arguments),
                    context: None,
                    confirmation: None,
                    revision: None,
                };
                job.pending = Some(host.invoke(&caller, request));
                job.invocation = Some((invocation, caller));
                chrome.borrow_mut().remark.sending = Sending::InFlight {
                    step: "delivering".into(),
                };
            } else if !job.launched {
                job.launched = true;
                let request = tilo_actions::remote::InvokeRequest {
                    invocation: tilo_actions::InvocationId::new(),
                    target: tilo_actions::remote::application_open_target(&room, CHAT_APP),
                    argument: tilo_actions::remote::InvokeArgument::None,
                    context: None,
                    confirmation: None,
                    revision: None,
                };
                drop(host.invoke(&caller, request));
                chrome.borrow_mut().remark.sending = Sending::InFlight {
                    step: "opening chat".into(),
                };
            } else if job.started.elapsed() >= DELIVERY_TIMEOUT {
                outcome = Some(Err(crate::shell::i18n::t("remark-send-failed")));
            }
        }
        drop(host);
        let Some(outcome) = outcome else {
            return false;
        };
        let Some(job) = self.remark.delivery.take() else {
            return false;
        };
        let mut st = chrome.borrow_mut();
        match outcome {
            Ok(()) => {
                // Delivered: the in-memory session has done its job.
                if let Flow::Attached { record, .. } = &st.remark.flow
                    && let (Some(recorder), Some(id)) = (
                        self.session_services.recorder().cloned(),
                        ui::record::RecordId::parse(record),
                    )
                {
                    drop(recorder.take_session(&id));
                }
                let title = if job.delivery.public {
                    crate::shell::i18n::t("remark-sent-community")
                } else if job.delivery.private {
                    if st
                        .remark_directory()
                        .team
                        .as_ref()
                        .is_some_and(|(team, _)| {
                            tilo_reference::DirectRoom::between(&job.me, team)
                                .is_some_and(|direct| direct.room_id() == job.delivery.room)
                        })
                    {
                        crate::shell::i18n::t("remark-sent-team")
                    } else {
                        crate::shell::i18n::t_args(
                            "remark-sent-person",
                            &[("name", crate::shell::i18n::text(job.label.clone()))],
                        )
                    }
                } else {
                    crate::shell::i18n::t_args(
                        "remark-sent-room",
                        &[("room", crate::shell::i18n::text(job.label.clone()))],
                    )
                };
                let notice = crate::shell::notification::Notification::new(
                    title,
                    crate::shell::i18n::t("remark-sent-body"),
                )
                .with_app_name(crate::shell::i18n::t("remark-qs"))
                .with_shell_action(
                    crate::shell::i18n::t("remark-open-conversation"),
                    "remark.conversation.open",
                    serde_json::json!({ "room": job.delivery.room }),
                );
                st.push_notification_full(notice);
                st.remark.sending = Sending::Sent {
                    room: job.delivery.room.clone(),
                    channel: job.delivery.channel.clone(),
                    record: job.record,
                };
                st.last_remark = Some(st.remark.sending.clone());
                st.remark.close();
                st.pick.clear();
            }
            Err(reason) => {
                st.remark.sending = Sending::Failed { reason };
                st.remark.stage = Stage::Compose;
            }
        }
        st.bump();
        true
    }

    /// The pick overlay, composer, review or recording pill, over the
    /// desktop and under the modal chrome that outranks them.
    pub(super) fn draw_remark(
        &mut self,
        scene: &mut vello::Scene,
        transform: vello::kurbo::Affine,
        text_renderer: &mut ui::text_render::TextRenderer,
        theme: &ui::theme::Theme,
        presence: &ui::presence::PresenceCtx,
    ) {
        let Some(chrome) = self.chrome.clone() else {
            return;
        };
        let viewport = ui::widget::Widget::get_rect(self);
        let recording_ms = self.remark_recording_ms();
        let st = chrome.borrow();
        match st.remark.stage {
            Stage::Closed | Stage::Recording => {}
            Stage::Picking => {
                self.remark.view.draw_pick(
                    scene,
                    transform,
                    text_renderer,
                    theme,
                    viewport,
                    &st.pick,
                );
            }
            Stage::Compose | Stage::Sending => {
                let options = st.remark_directory().options();
                let recent = st
                    .session_view()
                    .recorder
                    .as_ref()
                    .is_some_and(|recorder| recorder.recent_buffer);
                self.remark.view.draw_composer(
                    scene,
                    transform,
                    text_renderer,
                    theme,
                    presence,
                    viewport,
                    &st.remark,
                    &options,
                    recent,
                    recording_ms,
                );
            }
            Stage::Review => {
                self.remark.view.draw_review(
                    scene,
                    transform,
                    text_renderer,
                    theme,
                    viewport,
                    st.remark.review.as_ref(),
                    &self.remark.review_states,
                );
            }
        }
        // The Recorder's pill, while the composer's recording runs and the
        // composer is not in front of it.
        if let (Flow::Recording { .. }, Some(ms)) = (&st.remark.flow, recording_ms)
            && st.remark.stage == Stage::Recording
        {
            let anchor = Point::new(
                viewport.x1 - f64::from(theme.tokens.spacing.s4),
                viewport.y0
                    + f64::from(theme.tokens.metrics.bar_height)
                    + f64::from(theme.tokens.spacing.s2),
            );
            self.remark
                .view
                .draw_pill(scene, transform, text_renderer, theme, anchor, ms);
        }
    }

    /// The remark surface's named controls on screen now.
    pub(crate) fn remark_test_rects(&self) -> Vec<(String, Rect)> {
        let open = self
            .chrome
            .as_ref()
            .is_some_and(|chrome| chrome.borrow().remark.is_open());
        if open {
            self.remark.view.test_rects()
        } else {
            Vec::new()
        }
    }

    /// Whether the remark surface takes the pointer and keyboard from the
    /// apps now: all of it while picking or reviewing, and while composing,
    /// what lands on the composer or is typed into it. A click beside the
    /// composer still reaches the app, as it would beside a comment.
    pub(crate) fn remark_captures_input(&self) -> bool {
        let Some(chrome) = self.chrome.as_ref() else {
            return false;
        };
        match chrome.borrow().remark.stage {
            Stage::Closed => false,
            Stage::Picking | Stage::Review => true,
            // Recording is the person using their apps; only the pill's
            // Stop is the shell's.
            Stage::Recording => self
                .last_cursor
                .is_some_and(|point| self.remark.view.target_at(point).is_some()),
            Stage::Compose | Stage::Sending => {
                ui::widget::Widget::is_focused(&self.remark.view.text)
                    || self
                        .last_cursor
                        .is_some_and(|point| self.remark.view.panel_rect().contains(point))
            }
        }
    }

    /// Run the shell action a remark target stands for, as a click on it.
    fn run_remark_target(&mut self, target: &crate::shell::remark::view::Target) {
        use crate::shell::remark::view::Target;
        match target {
            Target::DestinationList => {
                self.remark.view.toggle_destinations();
                return;
            }
            Target::Text => {
                ui::widget::Widget::set_focus(&mut self.remark.view.text, true);
                return;
            }
            Target::Absorb => return,
            _ => {}
        }
        let Some((id, args)) = target.action() else {
            return;
        };
        if matches!(target, Target::Destination(_)) {
            self.remark.view.close_destinations();
        }
        let Some(chrome) = self.chrome.clone() else {
            return;
        };
        let serde_json::Value::Object(args) = args else {
            return;
        };
        let result =
            crate::shell::search::providers::actions::apply(&mut chrome.borrow_mut(), id, &args);
        if let Err(error) = result {
            chrome.borrow_mut().remark.notice = Some(crate::shell::remark::Notice {
                key: "remark-send-failed",
                detail: Some(error.to_string()),
            });
        }
    }

    /// Route one input event to the remark surface. Returns whether it was
    /// consumed.
    #[allow(
        clippy::too_many_arguments,
        clippy::too_many_lines,
        reason = "the remark surface's input routing, with the text field's event context"
    )]
    pub(super) fn handle_remark_event(
        &mut self,
        event: &winit::event::WindowEvent,
        scale_factor: f64,
        modifiers: winit::event::Modifiers,
        doc: &mut automerge::AutoCommit,
        path: &automerge::ObjId,
        handled: &mut bool,
        focus_target: &mut Option<taffy::NodeId>,
    ) -> bool {
        use winit::event::{ElementState, MouseButton, WindowEvent};
        use winit::keyboard::{Key, NamedKey};
        let Some(chrome) = self.chrome.clone() else {
            return false;
        };
        let stage = chrome.borrow().remark.stage;
        if stage == Stage::Closed {
            return false;
        }
        let cursor = self.last_cursor;
        // Recording: the apps are the person's; only the pill's Stop is ours.
        if stage == Stage::Recording {
            if let WindowEvent::CursorMoved { position, .. } = event {
                self.remark
                    .view
                    .set_pointer(Some(Point::new(position.x, position.y)));
                return false;
            }
            let on_pill = cursor.and_then(|point| self.remark.view.target_at(point));
            if let (
                WindowEvent::MouseInput {
                    state,
                    button: MouseButton::Left,
                    ..
                },
                Some(target),
            ) = (event, on_pill)
            {
                if *state == ElementState::Released {
                    self.run_remark_target(&target);
                }
                *handled = true;
                return true;
            }
            return false;
        }
        match event {
            WindowEvent::CursorMoved { position, .. } => {
                let point = Point::new(position.x, position.y);
                self.remark.view.set_pointer(Some(point));
                if stage == Stage::Picking && !self.remark.view.pick_bar_rect().contains(point) {
                    chrome.borrow_mut().pick.hover(point);
                }
                *handled = true;
                return true;
            }
            WindowEvent::MouseInput {
                state,
                button: MouseButton::Left,
                ..
            } => {
                let Some(point) = cursor else {
                    return true;
                };
                let target = self.remark.view.target_at(point);
                if stage == Stage::Picking {
                    if let Some(target) = target {
                        if *state == ElementState::Released {
                            self.run_remark_target(&target);
                        }
                    } else if *state == ElementState::Pressed {
                        chrome.borrow_mut().pick.drag = Some((point, point));
                    } else {
                        let additive = modifiers.state().shift_key();
                        let mut st = chrome.borrow_mut();
                        st.pick.release(point, additive);
                        st.bump();
                    }
                    *handled = true;
                    return true;
                }
                // Outside the composer, a click passes through to the
                // desktop; the composer stays open, as a comment does.
                let inside = self.remark.view.panel_rect().contains(point);
                if !inside && stage != Stage::Review {
                    ui::widget::Widget::set_focus(&mut self.remark.view.text, false);
                    return false;
                }
                if *state == ElementState::Pressed {
                    let on_text = self.remark.view.text_rect().contains(point);
                    ui::widget::Widget::set_focus(&mut self.remark.view.text, on_text);
                } else if let Some(target) = target {
                    self.run_remark_target(&target);
                }
            }
            WindowEvent::KeyboardInput { event: key, .. } if key.state == ElementState::Pressed => {
                let shift = modifiers.state().shift_key();
                let ctrl = modifiers.state().control_key();
                if stage == Stage::Picking {
                    let run = |id: &str, args: serde_json::Value| {
                        if let serde_json::Value::Object(args) = args {
                            let _ = crate::shell::search::providers::actions::apply(
                                &mut chrome.borrow_mut(),
                                id,
                                &args,
                            );
                        }
                    };
                    let walk = |direction: &str| serde_json::json!({ "direction": direction });
                    match &key.logical_key {
                        Key::Named(NamedKey::Escape) => {
                            run("remark.pick.cancel", serde_json::json!({}));
                        }
                        Key::Named(NamedKey::ArrowUp) => run("remark.pick.walk", walk("out")),
                        Key::Named(NamedKey::ArrowDown) => run("remark.pick.walk", walk("in")),
                        Key::Named(NamedKey::ArrowLeft) => {
                            run("remark.pick.walk", walk("previous"));
                        }
                        Key::Named(NamedKey::ArrowRight) => run("remark.pick.walk", walk("next")),
                        Key::Named(NamedKey::Space) => {
                            run("remark.pick.take", serde_json::json!({ "toggle": true }));
                        }
                        Key::Named(NamedKey::Enter) => {
                            let focus_unpicked = {
                                let st = chrome.borrow();
                                st.pick
                                    .focus
                                    .as_ref()
                                    .is_some_and(|key| !st.pick.selection.contains(key))
                            };
                            if focus_unpicked {
                                run("remark.pick.take", serde_json::json!({}));
                            }
                            run("remark.pick.done", serde_json::json!({}));
                        }
                        _ => {}
                    }
                    *handled = true;
                    return true;
                }
                let text_focused = ui::widget::Widget::is_focused(&self.remark.view.text);
                match &key.logical_key {
                    Key::Named(NamedKey::Escape) => {
                        if stage == Stage::Review {
                            self.run_remark_target(&crate::shell::remark::view::Target::ReviewBack);
                        } else if text_focused {
                            ui::widget::Widget::set_focus(&mut self.remark.view.text, false);
                        } else {
                            self.run_remark_target(&crate::shell::remark::view::Target::Discard);
                        }
                        *handled = true;
                        return true;
                    }
                    Key::Named(NamedKey::Enter) if ctrl => {
                        self.run_remark_target(&crate::shell::remark::view::Target::Send);
                        *handled = true;
                        return true;
                    }
                    Key::Named(NamedKey::Tab) if !text_focused || shift => {
                        ui::widget::Widget::set_focus(&mut self.remark.view.text, false);
                        self.remark.view.move_focus(shift);
                        *handled = true;
                        return true;
                    }
                    Key::Named(NamedKey::Enter | NamedKey::Space) if !text_focused => {
                        if let Some(target) = self.remark.view.focused_target() {
                            self.run_remark_target(&target);
                        }
                        *handled = true;
                        return true;
                    }
                    _ => {}
                }
            }
            _ => {}
        }
        // A key the composer does not own, with its field not focused, is
        // the desktop's: Super+M from here goes back to picking, and other
        // chords keep working beside an open composer.
        if matches!(event, WindowEvent::KeyboardInput { .. })
            && !ui::widget::Widget::is_focused(&self.remark.view.text)
            && stage != Stage::Review
        {
            return false;
        }
        if stage == Stage::Compose || stage == Stage::Sending {
            let mut taffy = taffy::TaffyTree::<()>::new();
            if let Ok(node) = taffy.new_leaf(taffy::Style::default()) {
                let mut text_handled = false;
                ui::widget::Widget::handle_event(
                    &mut self.remark.view.text,
                    event,
                    scale_factor,
                    &modifiers,
                    doc,
                    path,
                    &mut text_handled,
                    focus_target,
                    node,
                    &taffy,
                );
                if let Some(value) = self.remark.view.text.take_dirty() {
                    let _ = crate::shell::search::providers::actions::apply(
                        &mut chrome.borrow_mut(),
                        "remark.text.set",
                        &serde_json::json!({ "text": value })
                            .as_object()
                            .cloned()
                            .unwrap_or_default(),
                    );
                }
            }
        }
        *handled = true;
        true
    }
}

/// `trace` with review's hidden runs masked in the keyframes that showed
/// them, and the keyframes the app refused to answer left out.
fn located_redactions(
    mut trace: session_trace::SessionTrace,
    located: &[tilo_feedback_review::trace::TraceRedaction],
    refused: &[usize],
) -> session_trace::SessionTrace {
    use tilo_feedback_review::trace::NodeField;
    for (index, entry) in trace.timeline.iter_mut().enumerate() {
        let session_trace::Event::Inspect(keyframe) = &mut entry.event else {
            continue;
        };
        for node in &mut keyframe.reply.nodes {
            for redaction in located
                .iter()
                .filter(|redaction| redaction.keyframe == index && redaction.node == node.id)
            {
                let field = match redaction.field {
                    NodeField::Value => node.value.as_mut(),
                    NodeField::Label => node.label.as_mut(),
                };
                if let Some(field) = field {
                    *field = crate::shell::remark::review::apply_spans(field, &[redaction.span]);
                }
            }
        }
    }
    let mut index = 0_usize;
    trace.timeline.retain(|_| {
        let keep = !refused.contains(&index);
        index = index.saturating_add(1);
        keep
    });
    trace
}

/// What the review preview draws for each step: the step's app's latest
/// tree at or before it, with every text as it will leave.
/// Where replay does not vouch for a step's frames, in surface units.
fn divergent_rects(
    review: &Review,
    step: &crate::shell::remark::review::Step,
    unit: f64,
) -> Vec<Rect> {
    let unit = unit.max(f64::EPSILON);
    let rects = review
        .divergent_regions(step.index)
        .into_iter()
        .filter(|(route, _)| step.app_id.is_empty() || route.app_id == step.app_id)
        // Pixels the replay drew otherwise, not those nothing describes:
        // the verdict counts those, but outlining them marks the window.
        .filter(|(_, region)| {
            !matches!(
                region.divergence,
                session_trace::divergence::RegionDivergence::Unexplained
            )
        })
        .map(|(_, region)| {
            let b = region.bounds;
            Rect::new(
                f64::from(b.x) / unit,
                f64::from(b.y) / unit,
                f64::from(b.x.saturating_add(b.width)) / unit,
                f64::from(b.y.saturating_add(b.height)) / unit,
            )
        })
        .collect::<Vec<Rect>>();
    // The innermost only: an element and its container differing is one
    // place, marked once.
    let within = |outer: &Rect, inner: &Rect| {
        outer != inner
            && outer.x0 <= inner.x0
            && outer.y0 <= inner.y0
            && outer.x1 >= inner.x1
            && outer.y1 >= inner.y1
    };
    rects
        .iter()
        .filter(|rect| !rects.iter().any(|other| within(rect, other)))
        .copied()
        .collect()
}

fn step_states(
    review: &Review,
    trace: &session_trace::SessionTrace,
    frames: Option<&session_trace::frames::FrameStrip>,
) -> Vec<StepState> {
    let masker = session_trace::redact::Masker::new(review.hidden_values());
    review
        .steps
        .iter()
        .enumerate()
        .map(|(index, step)| {
            // What the step left on screen: the app's last tree (and frame)
            // before the next step began, so a failure shows its banner.
            let until = review
                .steps
                .get(index.saturating_add(1))
                .map_or(trace.duration, |next| next.at);
            let shown_by = |at: session_trace::Offset| at <= step.at || at < until;
            let keyframe = trace
                .timeline
                .iter()
                .filter(|entry| shown_by(entry.at))
                .filter_map(|entry| match &entry.event {
                    session_trace::Event::Inspect(keyframe)
                        if step.app_id.is_empty() || keyframe.route.app_id == step.app_id =>
                    {
                        Some(keyframe)
                    }
                    _ => None,
                })
                .next_back();
            let acted = step.invocation.and_then(|invocation| {
                trace.events().find_map(|entry| match &entry.event {
                    session_trace::Event::Presence(session_trace::PresenceUpdate {
                        change: session_trace::PresenceChange::Interaction(interaction),
                        ..
                    }) if interaction.invocation == invocation => match &interaction.anchor {
                        tilo_actions::InteractionAnchor::SemanticControl { id } => Some(id.clone()),
                        _ => None,
                    },
                    _ => None,
                })
            });
            let nodes = keyframe
                .map(|keyframe| &keyframe.reply)
                .map(|reply| {
                    reply
                        .nodes
                        .iter()
                        .filter_map(|node| {
                            let bounds = node.visible.or(node.bounds)?;
                            let text = match node.sensitivity {
                                tilo_actions::Sensitivity::Secret => None,
                                tilo_actions::Sensitivity::Personal => {
                                    node.content_text().map(crate::shell::remark::mask)
                                }
                                tilo_actions::Sensitivity::Public => {
                                    node.content_text().map(|text| masker.mask(text))
                                }
                            };
                            let is_value = node.content == tilo_reference::inspect::Content::Value;
                            let content =
                                text.as_deref().is_some_and(|text| !text.trim().is_empty())
                                    && node.children.is_empty()
                                    && (is_value
                                        || !crate::shell::remark::view::is_control(&node.role));
                            Some(StateNode {
                                id: node.id.clone(),
                                name: node.label.clone().filter(|_| is_value),
                                leaf: node.children.is_empty(),
                                role: node.role.clone(),
                                content,
                                acted: acted.as_deref() == Some(node.id.as_str()),
                                bounds: rect_of(bounds),
                                text,
                            })
                        })
                        .collect()
                })
                .unwrap_or_default();
            let frame = frames.and_then(|strip| {
                strip
                    .frames
                    .iter()
                    .rfind(|frame| {
                        shown_by(frame.at)
                            && (step.app_id.is_empty() || frame.route.app_id == step.app_id)
                    })
                    .and_then(|stored| stored.decode().ok())
                    .map(|decoded| StepFrame {
                        rgba: std::sync::Arc::from(decoded.rgba),
                        width: decoded.width,
                        height: decoded.height,
                        scale: decoded.scale,
                    })
            });
            let unit = frame.as_ref().map_or(1.0, |frame| frame.scale);
            let divergent = divergent_rects(review, step, unit);
            StepState {
                step: step.index,
                nodes,
                frame,
                divergent,
            }
        })
        .collect()
}
