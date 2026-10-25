//! The people overlay on the native desktop: the roster pinned out of the
//! bar's dropdown (`tilo_desktop::people_overlay`, the same overlay the
//! browser shows), waiting at an edge past the desk widgets as one of them
//! does and kept there in the person's settings as the widgets keep theirs.
//!
//! Its peek draws each person's screen from the layout their live presence
//! carries (`ShellNetwork::live_layouts_of`, through [`LiveLayoutBridge`]),
//! each window matched against the windows here by its record's digest:
//! one on a record this desktop shows is pictured live from this desktop's
//! own window, any other shows as its app alone. This desktop says where
//! its own windows are the same way. Follow is the host's one follow
//! ([`ShellOverlayHost::set_followed_peer`]); a message goes through Chat's
//! own `send-direct-message` action on the person's conversation.

#[allow(
    clippy::wildcard_imports,
    reason = "private implementation module shares its enclosing host types"
)]
use super::*;

use std::collections::BTreeMap;

use settings_store::desk_widgets::{PEOPLE_OVERLAY, PeopleOverlayPrefs};
use tilo_actions::InvocationId;
use tilo_actions::remote::{
    Caller, InvokeArgument, InvokeRequest, PendingPoll, application_action_target,
};
use tilo_desktop::people_overlay::{
    MessageLine, PeekPictures, PeerMessages, PeerScreen, PeerWindow, PeopleHit, PeopleOverlay,
    PeopleOverlayModel,
};
use tilo_relay_protocol::live::{LAYOUT_UNITS, LiveLayout, LiveRecord};

use crate::shell::remote_actions::BrokerActionHost;
use crate::shell::state::RoomId;

/// Chat, whose person conversations take a direct message.
const CHAT_APP: &str = "so.tilo.chat";
/// Chat's operation on a person's conversation that sends them a message.
const SEND_DIRECT_MESSAGE: &str = "send-direct-message";
/// How many of the last lines of a conversation the message box shows.
const SHOWN_LINES: usize = 3;

/// Where people's windows are, through the room's live presence: what each
/// member said, by session handle, and saying this person's own.
#[derive(Clone)]
pub struct LiveLayoutBridge {
    pub heard: std::rc::Rc<dyn Fn(RoomId) -> BTreeMap<u64, LiveLayout>>,
    pub publish: std::rc::Rc<dyn Fn(RoomId, Option<LiveLayout>)>,
}

/// The overlay as the person's settings left it: pinned or not, at its
/// edge.
pub(super) fn from_settings() -> PeopleOverlay {
    let prefs = PEOPLE_OVERLAY.get();
    let mut overlay = PeopleOverlay::new(prefs.edge);
    overlay.show(prefs.pinned);
    overlay
}

/// One window here, by the record it shows as the live layer names it.
struct LocalWindow {
    tab: crate::shell::state::TabId,
    /// The record as the live layer names it.
    record: LiveRecord,
    digest: [u8; 8],
    title: String,
    group: u64,
}

impl ShellOverlayHost {
    pub fn set_live_layout_bridge(&mut self, bridge: LiveLayoutBridge) {
        self.live_layout_bridge = Some(bridge);
        self.published_layout = None;
    }

    /// Pin the roster to the desk, or put it back in the bar; kept.
    pub(super) fn pin_people_overlay(&mut self, pinned: bool) {
        self.people_overlay.show(pinned);
        if let Err(error) = PEOPLE_OVERLAY.update(|prefs| PeopleOverlayPrefs { pinned, ..prefs }) {
            tracing::warn!("keeping the people overlay's pin: {error}");
        }
        self.bump_chrome();
    }

    /// The area under the top bar the overlay floats in.
    fn people_overlay_area(&self) -> Rect {
        let area = self.overlay_area();
        Rect::new(
            area.x0,
            area.y0 + f64::from(tilo_desktop::topbar::TOPBAR_HEIGHT_PX),
            area.x1,
            area.y1,
        )
    }

    pub(super) fn draw_people_overlay(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        text_renderer: &mut TextRenderer,
        theme: &Theme,
        presence: &PresenceCtx,
    ) {
        if !self.people_overlay.is_visible() {
            ui::window_pictures::want(&[]);
            return;
        }
        let rows = self.roster_frame(presence).rows;
        let local = self.local_windows();
        let screens = self.peer_screens(&local);
        let wanted: Vec<u64> = self
            .people_overlay
            .peeked()
            .and_then(|id| screens.iter().find(|screen| screen.id == id))
            .into_iter()
            .flat_map(|screen| &screen.windows)
            .filter_map(|window| window.picture)
            .collect();
        ui::window_pictures::want(&wanted);
        let messages = self
            .people_overlay
            .peeked()
            .and_then(|id| {
                let row = rows.iter().find(|row| row.id == id)?;
                Some(PeerMessages {
                    id: id.to_owned(),
                    lines: last_lines(&person_key(row)?),
                })
            })
            .into_iter()
            .collect::<Vec<_>>();
        let unavailable = self
            .action_broker()
            .is_none()
            .then(|| crate::shell::i18n::t("people-overlay-messages-unavailable"));
        let occupied = self.desk_glances();
        let host = self.people_overlay_area();
        self.people_overlay.draw(
            scene,
            transform,
            text_renderer,
            theme,
            &PeopleOverlayModel {
                host,
                occupied: &occupied,
                rows: &rows,
                screens: &screens,
                messages: &messages,
                followed_id: self.followed_peer.as_deref(),
                messaging_unavailable: unavailable.as_deref(),
            },
            &mut LivePictures,
        );
    }

    /// The overlay's share of `event`: what lands on it, and the message
    /// box's keys while it holds the keyboard. Returns whether it took it.
    pub(super) fn route_people_overlay_event(
        &mut self,
        event: &WindowEvent,
        ctx: &mut ui::widget::EventCtx<'_>,
    ) -> bool {
        if !self.people_overlay.is_visible() {
            return false;
        }
        let hit = match event {
            WindowEvent::KeyboardInput { event: key, .. } => {
                if key.state != ElementState::Pressed {
                    if !self.people_overlay.composing() {
                        return false;
                    }
                    *ctx.handled = true;
                    return true;
                }
                match self
                    .people_overlay
                    .key(&key.logical_key, ctx.modifiers.state())
                {
                    Some(hit) => {
                        *ctx.handled = true;
                        hit
                    }
                    None => return false,
                }
            }
            WindowEvent::Ime(winit::event::Ime::Commit(text)) => {
                if !self.people_overlay.commit_text(text) {
                    return false;
                }
                *ctx.handled = true;
                None
            }
            _ => {
                let mut handled = false;
                let hit = self
                    .people_overlay
                    .handle_event(event, ctx.scale_factor, &mut handled);
                if handled {
                    *ctx.handled = true;
                }
                hit
            }
        };
        if let Some(hit) = hit {
            self.apply_people_hit(hit);
        }
        if *ctx.handled {
            self.bump_chrome();
        }
        *ctx.handled
    }

    pub(super) fn apply_people_hit(&mut self, hit: PeopleHit) {
        match hit {
            PeopleHit::ToggleFollow(id) => {
                if self.followed_peer.as_deref() == Some(id.as_str()) {
                    self.set_followed_peer(None);
                } else if self.followable_peer(&id) {
                    self.set_followed_peer(Some(id));
                }
            }
            PeopleHit::Unpin => self.pin_people_overlay(false),
            PeopleHit::Moved(edge) => {
                if let Err(error) =
                    PEOPLE_OVERLAY.update(|prefs| PeopleOverlayPrefs { edge, ..prefs })
                {
                    tracing::warn!("keeping the people overlay's edge: {error}");
                }
            }
            PeopleHit::Send { to, text } => self.send_direct_message(&to, &text),
        }
    }

    /// Whether `id` is someone here this person may follow, as the live
    /// roster says now.
    fn followable_peer(&self, id: &str) -> bool {
        self.chrome.as_ref().is_some_and(|chrome| {
            let state = chrome.borrow();
            let local_user = state.active_local_user();
            tilo_desktop::collaborators::collaborator_roster_rows(
                &state.tree().desktop,
                &local_user,
            )
            .into_iter()
            .any(|row| row.id == id && row.followable)
        })
    }

    /// Send `text` to the person with roster id `to` through Chat's own
    /// action on their conversation; says so when it did not go.
    fn send_direct_message(&mut self, to: &str, text: &str) {
        let (Some(chrome), Some(room)) = (self.chrome.clone(), self.active_room()) else {
            return;
        };
        let row = self
            .people_overlay
            .shown_rows()
            .iter()
            .find(|row| row.id == to)
            .cloned();
        let name = row
            .as_ref()
            .map_or_else(|| to.to_owned(), |row| row.label.clone());
        let workspace = room.to_string();
        let caller = Caller::local_person(chrome.borrow().local_user_for_room(&room))
            .in_room(workspace.clone());
        let refusal = row
            .as_ref()
            .and_then(person_key)
            .zip(self.action_broker())
            .map_or_else(
                || Some(crate::shell::i18n::t("people-overlay-messages-unavailable")),
                |(key, broker)| Self::ask_chat_to_send(broker, &caller, &workspace, &key, text),
            );
        match refusal {
            None => self.people_overlay.sent(),
            Some(reason) => {
                chrome.borrow_mut().push_warning_notification(
                    crate::shell::i18n::t_args(
                        "people-overlay-message-failed",
                        &[("name", name.into())],
                    ),
                    reason,
                );
            }
        }
    }

    /// Ask Chat, through `broker`, to send `text` to the person with key
    /// `key` in `workspace`; the reason it refused, if it did.
    fn ask_chat_to_send(
        broker: crate::shell::collab_broker::AppActionBroker,
        caller: &Caller,
        workspace: &str,
        key: &str,
        text: &str,
    ) -> Option<String> {
        let mut argument = serde_json::Map::new();
        argument.insert("text".into(), serde_json::Value::String(text.to_owned()));
        let request = InvokeRequest {
            on_behalf_of: None,
            invocation: InvocationId::new(),
            target: application_action_target(
                workspace,
                CHAT_APP,
                format!("chat.person:{key}"),
                SEND_DIRECT_MESSAGE,
            ),
            argument: InvokeArgument::Json(argument),
            context: None,
            confirmation: None,
            revision: None,
            wait_ms: None,
            appearance: None,
        };
        let polled = BrokerActionHost::new(broker)
            .invoke_job(caller, request)
            .poll_ready();
        match polled {
            PendingPoll::Ready(Ok(_)) | PendingPoll::Waiting => None,
            PendingPoll::Ready(Err(error)) => Some(error.to_string()),
            PendingPoll::Abandoned => {
                Some(crate::shell::i18n::t("people-overlay-messages-unavailable"))
            }
        }
    }

    /// The windows of the active workspace, each with the record it shows
    /// as the live layer names it (app and wire record id, digested).
    fn local_windows(&self) -> Vec<LocalWindow> {
        let Some(chrome) = self.chrome.as_ref() else {
            return Vec::new();
        };
        let state = chrome.borrow();
        let tree = state.tree();
        let Some(workspace) = tree.desktop.active_workspace() else {
            return Vec::new();
        };
        let room = workspace.room();
        let cluster = room.to_string();
        let windows = workspace
            .strips
            .iter()
            .flat_map(|strip| &strip.window_groups)
            .flat_map(|group| group.tabs.iter().map(move |tab| (group.id, tab)))
            .filter_map(|(group, tab)| {
                let tilo_desktop_model::TileContent::LocalApp { app_id, .. } = &tab.tile else {
                    return None;
                };
                let record = tree.room_tab_record(room, tab.id)?;
                let record = LiveRecord {
                    app: app_id.clone(),
                    record: crate::network::wire_record::wire_record_id(&cluster, app_id, &record),
                };
                Some(LocalWindow {
                    tab: tab.id,
                    digest: record.short_digest(),
                    record,
                    title: tab.title.clone(),
                    group,
                })
            })
            .collect();
        drop(tree);
        drop(state);
        windows
    }

    /// Each person's screen as their live layout says, by roster id.
    fn peer_screens(&self, local: &[LocalWindow]) -> Vec<PeerScreen> {
        let (Some(bridge), Some(chrome)) = (self.live_layout_bridge.as_ref(), self.chrome.as_ref())
        else {
            return Vec::new();
        };
        let Some(room) = self.active_room() else {
            return Vec::new();
        };
        let layouts = (bridge.heard)(room);
        if layouts.is_empty() {
            return Vec::new();
        }
        let state = chrome.borrow();
        let tree = state.tree();
        let Some(workspace) = tree.desktop.active_workspace() else {
            return Vec::new();
        };
        let screens = workspace
            .collaborators
            .iter()
            .filter_map(|person| {
                let layout = layouts.get(&person.session?)?;
                Some(peer_screen(&person.id, layout, local))
            })
            .collect();
        drop(tree);
        drop(state);
        screens
    }

    /// Say where this person's windows are on the strip shown, when that
    /// changed: by app and record digest only, laid out by the rule the
    /// browser uses too ([`LiveLayout::place`]).
    pub(super) fn refresh_live_layout(&mut self) {
        let Some(bridge) = self.live_layout_bridge.clone() else {
            return;
        };
        let Some(room) = self.active_room() else {
            return;
        };
        let local = self.local_windows();
        let area = self.people_overlay_area();
        let screen = self.rect;
        let windows: Vec<(LiveRecord, [f64; 4], bool)> = self
            .group_row_ptrs
            .values()
            .flat_map(|&ptr| {
                // SAFETY: same stable boxed-row contract as the other
                // `group_row_ptrs` accessors; this is a read-only query.
                let row: &GroupRow = unsafe { &*ptr };
                row.groups.iter().filter_map(|group| {
                    let rect = group.get_rect();
                    if rect.intersect(screen).area() <= 0.0 {
                        return None;
                    }
                    let tab = group.active_tab_id();
                    let window = local.iter().find(|window| window.tab == tab)?;
                    Some((
                        window.record.clone(),
                        [rect.x0, rect.y0, rect.x1, rect.y1],
                        group.focused,
                    ))
                })
            })
            .collect();
        let layout = LiveLayout::place([area.x0, area.y0, area.x1, area.y1], &windows);
        if self.published_layout.as_ref() == Some(&(room, layout.clone())) {
            return;
        }
        (bridge.publish)(room, layout.clone());
        self.published_layout = Some((room, layout));
    }
}

/// `layout` as the peek draws it, its windows matched against `local`.
fn peer_screen(id: &str, layout: &LiveLayout, local: &[LocalWindow]) -> PeerScreen {
    let units = f64::from(LAYOUT_UNITS);
    let [width, height] = layout.size;
    let windows = layout
        .windows
        .iter()
        .map(|window| {
            let here = local
                .iter()
                .find(|local| local.record.app == window.app && local.digest == window.record);
            let [x0, y0, x1, y1] = window.rect.map(f64::from);
            PeerWindow {
                rect: Rect::new(x0 / units, y0 / units, x1 / units, y1 / units),
                app: window.app.clone(),
                app_name: crate::shell::launcher::display_name_for_app_id_without_waiting(
                    &window.app,
                )
                .unwrap_or_else(|| window.app.clone()),
                title: here
                    .map(|local| local.title.clone())
                    .filter(|title| !title.trim().is_empty()),
                picture: here.map(|local| local.group),
                focused: window.focused,
            }
        })
        .collect();
    PeerScreen {
        id: id.to_owned(),
        aspect: if height == 0 {
            16.0 / 10.0
        } else {
            f64::from(width) / f64::from(height)
        },
        windows,
    }
}

/// The key Chat knows `row`'s person by: their roster id when it is a
/// person key, else the contact of that name.
fn person_key(row: &tilo_desktop::collaborators::RosterRow) -> Option<String> {
    if row.id.len() == 64 && row.id.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        return Some(row.id.to_ascii_lowercase());
    }
    ui::person_chat::contacts_snapshot()
        .into_iter()
        .find(|contact| contact.display_name.eq_ignore_ascii_case(&row.label))
        .map(|contact| contact.public_key)
}

/// The last lines of the conversation with `key`, as this desktop already
/// holds it: a passive read, which never opens one.
fn last_lines(key: &str) -> Vec<MessageLine> {
    let Ok(conversation) = ui::person_chat::read(key) else {
        return Vec::new();
    };
    let start = conversation.messages.len().saturating_sub(SHOWN_LINES);
    conversation
        .messages
        .iter()
        .skip(start)
        .map(|message| MessageLine {
            mine: message.author != key,
            text: message.text.clone(),
        })
        .collect()
}

/// The live pictures the strip keeps of the windows a peek shows, by
/// window group.
struct LivePictures;

impl PeekPictures for LivePictures {
    fn paint(&mut self, window: u64, scene: &mut Scene, transform: Affine, rect: Rect) -> bool {
        ui::window_pictures::with_picture(window, |picture| {
            ui::window_pictures::paint_fitted(picture, scene, transform, rect)
        })
        .is_some_and(|painted| painted.area() > 0.0)
    }
}
