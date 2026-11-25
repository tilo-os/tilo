//! What is live now, as the bar, quick settings and the lock screen show it (`device.live`): the
//! workspaces' live resources, which their stores derive from each type's `live when`
//! ([`tilo_workspace_service::live::LiveRows`]), and beside them what this machine plays or runs
//! that no resource holds — a player heard over MPRIS, a Rust app's activity on the board. One
//! reading, so a shell, a script and an agent read the same rows.
//!
//! A row of a resource names it by the address the session publishes it under (`tl:<app>:<id>`):
//! the shell shows that resource's own `view live`, whose buttons are the type's own actions. A
//! media key, and the lock screen's transport, ask the newest live resource whose type declares
//! the action they mean ([`media_action`]); only where none does do they go to the player here.

use serde_json::{Map, Value};
use tilo_actions::ApiError;
use tilo_broker_core::route::CrdtRoute;
use tilo_lock::MutexRecover as _;
use tilo_service::Started;

/// The resource the rows are published under.
pub(crate) const RESOURCE: &str = "session:live";

/// How long one read of the workspaces' live rows waits at the daemon for them to move, ms.
const FOLLOW_WAIT_MS: u32 = 25_000;
/// How long a follow waits before it asks a daemon that did not answer again.
const ASK_AGAIN: std::time::Duration = std::time::Duration::from_secs(1);

/// Follow what is live in the workspaces the device's daemon keeps, as `person`'s desktop, into
/// `rows` from now on: the daemon keeps the workspaces, and the session shows their rows, filtered
/// by the devices it reaches.
pub(crate) fn follow_daemon(
    daemon: &tilo_session_client::daemon_documents::DaemonShell,
    person: [u8; 32],
    rows: tilo_workspace_service::live::LiveRows,
) {
    let daemon = daemon.clone();
    let workspace = collab_types::person_workspace(&person).to_string();
    daemon.runtime().clone().spawn(async move {
        let mut after: Option<String> = None;
        loop {
            let read = tilo_actions::remote::ResourceReadRequest {
                workspace: workspace.clone(),
                app: tilo_desktop_model::commands::APP_ID.to_owned(),
                record: tilo_workspace_service::live::RESOURCE.to_owned(),
                joined: false,
                wait_ms: after.is_some().then_some(FOLLOW_WAIT_MS),
                after: after.clone(),
                answer: tilo_actions::remote::ReadAnswer::Document,
            };
            let taken = daemon.read(read).await.and_then(|readings| {
                let reading = serde_json::to_vec(&readings["records"][0]["value"])
                    .map_err(|error| error.to_string())?;
                rows.take_reading(&reading)?;
                Ok(readings["revision"].as_str().map(str::to_owned))
            });
            match taken {
                Ok(revision) if revision.is_some() && revision != after => after = revision,
                Ok(_) => tokio::time::sleep(ASK_AGAIN).await,
                Err(error) => {
                    tracing::warn!(%error, "what is live in the workspaces did not read; asking again");
                    after = None;
                    tokio::time::sleep(ASK_AGAIN).await;
                }
            }
        }
    });
}

/// What each window this device's apps hold, by the window instance the shell launched it for:
/// the session notes beside each window the record its app shows.
pub(crate) type WindowRoutes = std::sync::Arc<std::sync::Mutex<Vec<(u64, CrdtRoute)>>>;

/// Follow what each window this device's apps hold, as the daemon's broker names it, into
/// `routes` from now on: the daemon keeps the broker the windows connect to, so the session reads
/// which record each window shows there and notes it beside the window itself.
pub(crate) fn follow_window_notes(
    daemon: &tilo_session_client::daemon_documents::DaemonShell,
    person: [u8; 32],
    routes: WindowRoutes,
) {
    let daemon = daemon.clone();
    let workspace = collab_types::person_workspace(&person).to_string();
    daemon.runtime().clone().spawn(async move {
        let mut after: Option<String> = None;
        loop {
            let read = tilo_actions::remote::ResourceReadRequest {
                workspace: workspace.clone(),
                app: tilo_desktop_model::commands::APP_ID.to_owned(),
                record: tilo_workspace_service::window_notes::RESOURCE.to_owned(),
                joined: false,
                wait_ms: after.is_some().then_some(FOLLOW_WAIT_MS),
                after: after.clone(),
                answer: tilo_actions::remote::ReadAnswer::Document,
            };
            let taken = daemon.read(read).await.and_then(|readings| {
                let reading = serde_json::to_vec(&readings["records"][0]["value"])
                    .map_err(|error| error.to_string())?;
                let notes =
                    tilo_workspace_service::window_notes::WindowNotes::take_reading(&reading)?;
                *routes.lock_recover() = notes.routes();
                Ok(readings["revision"].as_str().map(str::to_owned))
            });
            match taken {
                Ok(revision) if revision.is_some() && revision != after => after = revision,
                Ok(_) => tokio::time::sleep(ASK_AGAIN).await,
                Err(error) => {
                    tracing::warn!(%error, "what the windows here hold did not read; asking again");
                    after = None;
                    tokio::time::sleep(ASK_AGAIN).await;
                }
            }
        }
    });
}

pub(crate) use tilo_device_surfaces::live::{Live, LiveControl, LiveEffect, LiveItem};

/// The most cards over the windows at once: the one touched longest ago folds into the bar's pill
/// when another comes.
pub(crate) const MAX_CARDS: usize = 3;

/// The cards this device stands over its windows, as the person left them.
#[derive(Debug, Default)]
pub(crate) struct Cards {
    /// The rows the person pulled out of the pill.
    expanded: std::collections::BTreeSet<String>,
    /// The rows whose card the person folded back into the pill.
    folded: std::collections::BTreeSet<String>,
    /// The rows the person touched, the latest first.
    touched: Vec<String>,
    /// Where the person left a card, by row.
    loose: std::collections::BTreeMap<String, (i64, i64)>,
}

impl Cards {
    fn touch(&mut self, key: &str) {
        self.touched.retain(|touched| touched != key);
        self.touched.insert(0, key.to_owned());
    }

    /// Mark which of `rows` stand as cards, and where the person left them; a row that went is
    /// forgotten, so the next thing live under its key starts as its app asks.
    fn mark(&mut self, rows: &mut [LiveItem]) {
        let live = |key: &String| rows.iter().any(|row| &row.key == key);
        self.expanded.retain(live);
        self.folded.retain(live);
        self.touched.retain(live);
        self.loose.retain(|key, _| live(key));
        let mut standing: Vec<(usize, usize)> = rows
            .iter()
            .enumerate()
            .filter(|(_, row)| {
                (row.floats || self.expanded.contains(&row.key))
                    && !self.folded.contains(&row.key)
                    && !row.collapsed
            })
            .map(|(at, row)| {
                let order = self
                    .touched
                    .iter()
                    .position(|touched| *touched == row.key)
                    .unwrap_or(usize::MAX);
                (order, at)
            })
            .collect();
        standing.sort_unstable();
        standing.truncate(MAX_CARDS);
        for (place, (_, at)) in (0_i64..).zip(standing) {
            if let Some(row) = rows.get_mut(at) {
                row.card = true;
                row.place = place;
            }
        }
        for row in rows {
            row.expanded = self.expanded.contains(&row.key);
            if let Some((x, y)) = self.loose.get(&row.key) {
                row.x = Some(*x);
                row.y = Some(*y);
            }
        }
    }
}

/// The key of a row of a Rust app's activity.
fn activity_key(app: &str, activity: &str) -> String {
    format!("activity:{app}:{activity}")
}

/// The key of the row of what this machine's players play.
const PLAYER_KEY: &str = "player";

/// The address the session publishes a tilolang resource of `kind` (`tl.<app>.<Type>`) and store
/// id `id` under, with its app; `None` for a kind no tilolang app declares.
pub(crate) fn address(kind: &str, id: &str) -> Option<(String, String)> {
    let (app, _) = kind.strip_prefix("tl.")?.rsplit_once('.')?;
    Some((tilolang_tilo::publish::resource_id(app, id), app.to_owned()))
}

/// Where the live `row` is published, by the app it is published by, and what is published of
/// it. A record is published by the app whose record holds it (Tube's Playback, as Tube
/// publishes it), which its kind does not name: the address its app publishes it at is the one
/// the shell and its actions reach it by, and a row nothing here publishes yet is at the address
/// its kind names.
fn located<'a>(
    resources: &'a [tilo_actions::ResourceSpec],
    row: &tilo_workspace_service::live::LiveRow,
) -> Option<(String, String, Option<&'a tilo_actions::ResourceSpec>)> {
    match published(resources, &row.kind, &row.id) {
        Some(spec) => Some((
            spec.id.as_str().to_owned(),
            tilolang_tilo::publish::app_of(spec.id.as_str())?.to_owned(),
            Some(spec),
        )),
        None => {
            let (resource, app) = address(&row.kind, &row.id)?;
            Some((resource, app, None))
        }
    }
}

/// The resource of `kind` whose store id is `id` among those the session publishes, wherever in
/// their tree it stands.
fn published<'a>(
    resources: &'a [tilo_actions::ResourceSpec],
    kind: &str,
    id: &str,
) -> Option<&'a tilo_actions::ResourceSpec> {
    resources.iter().find_map(|resource| {
        if resource.kind == kind
            && resource
                .id
                .as_str()
                .rsplit_once(':')
                .is_some_and(|(_, held)| held == id)
        {
            return Some(resource);
        }
        published(&resource.children, kind, id)
    })
}

/// Where the rows come from.
pub(crate) struct Sources {
    pub(crate) rows: tilo_workspace_service::live::LiveRows,
    pub(crate) activities: std::rc::Rc<crate::shell::activities::Activities>,
    pub(crate) readings: crate::status::Shared,
    /// The controls pressed on the player's row, with what they take, which the session's loop
    /// asks the player for: the player is a service beside the one that publishes the rows.
    pub(crate) pressed: std::cell::RefCell<Vec<(String, Map<String, Value>)>>,
    /// The cards this device stands over its windows.
    pub(crate) cards: std::cell::RefCell<Cards>,
    /// The cards the apps draw themselves: a row whose app presented one holds its place.
    pub(crate) drawn: crate::cards::Cards,
    /// The rows the person kept off this device's bar, by key.
    pub(crate) collapsed: std::cell::RefCell<std::collections::BTreeSet<String>>,
    /// Grows when the person keeps a row off the bar or brings it back.
    pub(crate) revision: std::cell::Cell<u64>,
    /// The session's services, once they are mounted: what names a live resource where its own
    /// view has no room, as a narrow bar's button and a screen reader say it.
    pub(crate) services:
        std::rc::Rc<std::cell::OnceCell<std::rc::Rc<std::cell::RefCell<tilo_service::Services>>>>,
}

impl Sources {
    /// Hold the place of every card its app draws itself where the shell's card keeps it, as
    /// `at` finds that block (named "{what it is} in {its app}", as the shell's card
    /// names it): the app is told to stand its card's window there.
    pub(crate) fn place_drawn(&self, at: impl Fn(&str) -> Option<(f64, f64, f64, f64)>) {
        let mut slots = std::collections::BTreeMap::new();
        for (app, summary, _) in self.activities.summaries() {
            if self.drawn.size(&app, &summary.id).is_none() {
                continue;
            }
            let source = if summary.source.is_empty() {
                crate::app_names::app_name(&app)
            } else {
                summary.source.clone()
            };
            let name = if summary.title.is_empty() {
                &source
            } else {
                &summary.title
            };
            if let Some((x, y, width, height)) = at(&format!("{name} in {source}")) {
                slots.insert(
                    (app, summary.id),
                    ui::vello::kurbo::Rect::new(x, y, x + width, y + height),
                );
            }
        }
        self.drawn.place(slots);
    }

    /// What makes the rows: a row that came or went, an app's activity, or what plays.
    pub(crate) fn revision(&self) -> (u64, u64, u64) {
        let playing = self.readings.borrow().playing().map_or(0, |playing| {
            use std::hash::{Hash as _, Hasher as _};
            let mut hasher = std::collections::hash_map::DefaultHasher::new();
            (
                &playing.player,
                &playing.title,
                &playing.subtitle,
                playing.playing,
            )
                .hash(&mut hasher);
            hasher.finish().max(1)
        });
        let drawn = {
            use std::hash::{Hash as _, Hasher as _};
            let mut hasher = std::collections::hash_map::DefaultHasher::new();
            for card in self.drawn.presented() {
                let size = card.window_size();
                (
                    &card.app,
                    &card.activity,
                    size.width.to_bits(),
                    size.height.to_bits(),
                )
                    .hash(&mut hasher);
            }
            hasher.finish()
        };
        (
            self.rows.revision().wrapping_add(self.revision.get()),
            self.activities.board().revision().wrapping_add(drawn),
            playing,
        )
    }

    /// The rows now.
    pub(crate) fn read(&self) -> Live {
        let live = self.rows.rows();
        // Each live resource's own name, as its app publishes it.
        let resources = if live.is_empty() {
            Vec::new()
        } else {
            self.services
                .get()
                .and_then(|services| services.try_borrow().ok().map(|held| held.resources()))
                .unwrap_or_default()
        };
        let mut rows: Vec<LiveItem> = live
            .into_iter()
            .filter_map(|row| {
                let (resource, app, published) = located(&resources, &row)?;
                let here = row.device.is_empty() || self.rows.is_here(&row.device);
                let title = published.map(|spec| spec.label.clone()).unwrap_or_default();
                Some(LiveItem {
                    key: resource.clone(),
                    title,
                    source: crate::app_names::app_name(&app),
                    resource,
                    app,
                    here,
                    device: if here { String::new() } else { row.device_name },
                    since: row.since,
                    ..LiveItem::default()
                })
            })
            .collect();
        if let Some(playing) = self.readings.borrow().playing() {
            let mut controls = Vec::new();
            if playing.can_previous {
                controls.push(control("previous", "Previous", "skip-back"));
            }
            if playing.can_toggle {
                controls.push(if playing.playing {
                    control("play-pause", "Pause", "pause")
                } else {
                    control("play-pause", "Play", "play")
                });
            }
            if playing.can_next {
                controls.push(control("next", "Next", "skip-forward"));
            }
            let (progress, elapsed, remaining) =
                along(playing.position_ms / 1000.0, playing.duration_ms / 1000.0);
            rows.push(LiveItem {
                kind: "music".to_owned(),
                progress,
                elapsed,
                remaining,
                key: PLAYER_KEY.to_owned(),
                app: playing.app.clone().unwrap_or_default(),
                source: playing.player.clone(),
                here: true,
                title: playing.title,
                subtitle: playing.subtitle,
                icon: "music".to_owned(),
                playing: playing.playing,
                controls,
                ..LiveItem::default()
            });
        }
        for (app, summary, floats) in self.activities.summaries() {
            let (progress, elapsed, remaining) = match (summary.progress, summary.length) {
                (Some(progress), Some(length)) => {
                    let length = length as f64;
                    along(f64::from(progress) * length, length)
                }
                (Some(progress), None) => (Some(f64::from(progress)), String::new(), String::new()),
                _ => (None, String::new(), String::new()),
            };
            let drawn = self.drawn.size(&app, &summary.id);
            rows.push(LiveItem {
                drawn: drawn.is_some(),
                drawn_width: drawn.map_or(0.0, |size| size.width),
                drawn_height: drawn.map_or(0.0, |size| size.height),
                kind: kind_name(summary.kind).to_owned(),
                progress,
                elapsed,
                remaining,
                artwork: summary.artwork,
                floats,
                key: activity_key(&app, &summary.id),
                source: if summary.source.is_empty() {
                    crate::app_names::app_name(&app)
                } else {
                    summary.source.clone()
                },
                app,
                here: true,
                title: summary.title,
                subtitle: summary.subtitle,
                icon: summary.icon,
                value: summary.value,
                playing: matches!(
                    summary.kind,
                    ui::live_activity::Kind::Music | ui::live_activity::Kind::Call
                ),
                controls: summary
                    .controls
                    .into_iter()
                    .map(|c| LiveControl {
                        ends: c.kind == ui::live_activity::ControlKind::Close,
                        id: c.id,
                        label: c.label,
                        icon: c.icon,
                    })
                    .collect(),
                ..LiveItem::default()
            });
        }
        let mut collapsed = self.collapsed.borrow_mut();
        // A row that went is forgotten: the next thing live under its key is on the bar again.
        collapsed.retain(|key| rows.iter().any(|row| &row.key == key));
        for row in &mut rows {
            row.collapsed = collapsed.contains(&row.key);
        }
        drop(collapsed);
        self.cards.borrow_mut().mark(&mut rows);
        Live { rows }
    }

    /// Do what `effect` asks of the rows.
    pub(crate) fn act(&self, effect: LiveEffect) -> Result<Started, ApiError> {
        match effect {
            LiveEffect::PressLive { key, control } => self.press(&key, &control),
            LiveEffect::EndLive { key } => self.close(&key),
            LiveEffect::KeepOffBar { key, off } => self.collapse(&key, off),
            LiveEffect::ExpandLive { key } => self.arrange(&key, |cards, key| {
                cards.folded.remove(key);
                cards.expanded.insert(key.to_owned());
                cards.touch(key);
            }),
            LiveEffect::FoldLive { key } => self.arrange(&key, |cards, key| {
                cards.expanded.remove(key);
                cards.loose.remove(key);
                cards.folded.insert(key.to_owned());
            }),
            LiveEffect::MoveLive { key, at } => self.arrange(&key, |cards, key| {
                match at.as_deref() {
                    Some(&[x, y]) => cards.loose.insert(key.to_owned(), (x, y)),
                    _ => cards.loose.remove(key),
                };
                cards.touch(key);
            }),
            LiveEffect::SeekLive { key, fraction } => self.seek(&key, fraction),
        }
    }

    /// Change how the row `key`'s card stands, as `change` says.
    fn arrange(
        &self,
        key: &str,
        change: impl FnOnce(&mut Cards, &str),
    ) -> Result<Started, ApiError> {
        if !self.read().rows.iter().any(|row| row.key == key) {
            return Err(ApiError::InvalidArguments(format!(
                "nothing live is called `{key}`"
            )));
        }
        change(&mut self.cards.borrow_mut(), key);
        self.revision.set(self.revision.get().wrapping_add(1));
        Ok(Started::Done(tilo_actions::ActionOutput::default()))
    }

    /// Take the row `key`, one this machine plays or runs, to `fraction` along it.
    fn seek(&self, key: &str, fraction: f64) -> Result<Started, ApiError> {
        if !(0.0..=1.0).contains(&fraction) {
            return Err(ApiError::InvalidArguments(format!(
                "a seek goes from 0 to 1, not {fraction}"
            )));
        }
        let row = self.read().rows.into_iter().find(|row| row.key == key);
        let Some(row) = row else {
            return Err(ApiError::InvalidArguments(format!(
                "nothing live is called `{key}`"
            )));
        };
        if row.elapsed.is_empty() {
            return Err(ApiError::InvalidArguments(format!(
                "`{key}` says no length to take it along"
            )));
        }
        self.cards.borrow_mut().touch(key);
        if key == PLAYER_KEY {
            let mut args = Map::new();
            args.insert("fraction".to_owned(), Value::from(fraction));
            self.pressed.borrow_mut().push(("seek".to_owned(), args));
        } else if let Some((app, activity)) = key
            .strip_prefix("activity:")
            .and_then(|rest| rest.split_once(':'))
        {
            self.activities.seek(app, activity, fraction);
        }
        Ok(Started::Done(tilo_actions::ActionOutput::default()))
    }

    /// End the row `key`, one this machine runs.
    fn close(&self, key: &str) -> Result<Started, ApiError> {
        let Some((app, activity)) = key
            .strip_prefix("activity:")
            .and_then(|rest| rest.split_once(':'))
        else {
            return Err(ApiError::InvalidArguments(format!(
                "`{key}` ends by its own app: a live resource by its type's actions, the player \
                 here in the app that plays"
            )));
        };
        self.activities.close(app, activity);
        self.cards
            .borrow_mut()
            .touched
            .retain(|touched| touched != key);
        Ok(Started::Done(tilo_actions::ActionOutput::default()))
    }

    /// Keep the row `key` off this device's bar (`collapsed`, the default), or bring it back.
    fn collapse(&self, key: &str, collapsed: Option<bool>) -> Result<Started, ApiError> {
        if !self.read().rows.iter().any(|row| row.key == key) {
            return Err(ApiError::InvalidArguments(format!(
                "nothing live is called `{key}`"
            )));
        }
        if collapsed.unwrap_or(true) {
            self.collapsed.borrow_mut().insert(key.to_owned());
        } else {
            self.collapsed.borrow_mut().remove(key);
        }
        self.revision.set(self.revision.get().wrapping_add(1));
        Ok(Started::Done(tilo_actions::ActionOutput::default()))
    }

    /// Press `control` of the row `key`, one this machine runs.
    pub(crate) fn press(&self, key: &str, control: &str) -> Result<Started, ApiError> {
        if key == PLAYER_KEY {
            if !matches!(control, "play-pause" | "next" | "previous") {
                return Err(ApiError::InvalidArguments(format!(
                    "the player here takes play-pause, next or previous, not `{control}`"
                )));
            }
            self.pressed
                .borrow_mut()
                .push((control.to_owned(), Map::new()));
            return Ok(Started::Done(tilo_actions::ActionOutput::default()));
        }
        let Some((app, activity)) = key
            .strip_prefix("activity:")
            .and_then(|rest| rest.split_once(':'))
        else {
            return Err(ApiError::InvalidArguments(format!(
                "`{key}` is a live resource: its own actions act on it"
            )));
        };
        self.activities.press(app, activity, control);
        self.cards.borrow_mut().touch(key);
        Ok(Started::Done(tilo_actions::ActionOutput::default()))
    }
}

/// Ask the player here for what was pressed on its row since the last pass.
pub(crate) fn press_player(sources: &Sources, services: &mut tilo_service::Services) {
    let pressed = std::mem::take(&mut *sources.pressed.borrow_mut());
    for (control, args) in pressed {
        match services.invoke(crate::MEDIA, &control, &args) {
            Some(Ok(_)) => {}
            Some(Err(error)) => tracing::warn!(%error, control, "the live row's player control"),
            None => tracing::debug!("nothing publishes the player"),
        }
    }
}

/// How far along `at` seconds is of `length`, and the two ends of its bar as the person reads
/// them ("1:12", "-2:48"); nothing where the length is unknown.
fn along(at: f64, length: f64) -> (Option<f64>, String, String) {
    if !(length > 0.0) {
        return (None, String::new(), String::new());
    }
    let at = at.clamp(0.0, length);
    (
        Some(at / length),
        clock(at),
        format!("-{}", clock(length - at)),
    )
}

/// `seconds` as a player's clock says it: "4:05", "1:02:09".
#[expect(
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss,
    reason = "a clamped, non-negative count of seconds far inside u64"
)]
fn clock(seconds: f64) -> String {
    let whole = seconds.max(0.0).floor() as u64;
    ui::format::timecode(whole)
}

/// The name a kind of activity goes by in the reading.
fn kind_name(kind: ui::live_activity::Kind) -> &'static str {
    use ui::live_activity::Kind;
    match kind {
        Kind::Call => "call",
        Kind::Navigation => "navigation",
        Kind::Timer => "timer",
        Kind::Stopwatch => "stopwatch",
        Kind::Music => "music",
        Kind::PictureInPicture => "picture",
        Kind::Other => "other",
    }
}

fn control(id: &str, label: &str, icon: &str) -> LiveControl {
    LiveControl {
        id: id.to_owned(),
        label: label.to_owned(),
        icon: icon.to_owned(),
        ends: false,
    }
}

/// The action a media key or the lock screen's transport means (`play-pause`, `next`,
/// `previous`), as a type declares it: `play_pause`, `next`, `previous`. Where the type declares
/// no `play_pause`, a playing resource's `pause` and a paused one's `play` stand for it.
pub(crate) fn media_action(asked: &str) -> &'static [&'static str] {
    match asked {
        "play-pause" => &["play_pause", "toggle", "pause", "play"],
        "next" => &["next"],
        "previous" => &["previous"],
        _ => &[],
    }
}

/// The newest live resource whose type takes the transport (`play_pause`, or `pause` and
/// `play`), with its address, what it declares, and its reading.
fn transport_resource(
    rows: &tilo_workspace_service::live::LiveRows,
    services: &tilo_service::Services,
) -> Option<(String, tilo_actions::ResourceSpec, Option<Value>)> {
    // With nothing live there is nothing to look up: what the services publish is not read.
    let rows = rows.rows();
    if rows.is_empty() {
        return None;
    }
    let resources = services.resources();
    rows.into_iter().rev().find_map(|row| {
        let (address, _, spec) = located(&resources, &row)?;
        let spec = spec?;
        let declares = |name: &str| spec.actions.iter().any(|a| a.id == name);
        (declares("play_pause") || declares("toggle") || declares("pause") || declares("play"))
            .then(|| {
                let reading = services.read(&address);
                (address, spec.clone(), reading)
            })
    })
}

/// What the lock screen's transport shows of the newest live resource that takes it: its name
/// and whether it plays, with the buttons its type declares. `None` where no live resource here
/// takes a transport, and the lock screen shows the player here instead.
pub(crate) fn lock_media(
    rows: &tilo_workspace_service::live::LiveRows,
    services: &tilo_service::Services,
) -> Option<tilo_sheets::lock::Media> {
    let (_, spec, reading) = transport_resource(rows, services)?;
    let declares = |name: &str| spec.actions.iter().any(|a| a.id == name);
    Some(tilo_sheets::lock::Media {
        title: spec.label.clone(),
        subtitle: crate::app_names::app_name(spec.app().unwrap_or_default()),
        playing: reading.as_ref().is_some_and(reads_as_playing),
        can_toggle: declares("play_pause")
            || declares("toggle")
            || declares("pause")
            || declares("play"),
        can_previous: declares("previous"),
        can_next: declares("next"),
        artwork: String::new(),
    })
}

/// Ask the newest live resource whose type declares what `asked` means (`play-pause`, `next`,
/// `previous`) to do it, through its own published action. `None` when no live resource here
/// takes it, so the caller asks the player instead.
pub(crate) fn invoke_media(
    rows: &tilo_workspace_service::live::LiveRows,
    asked: &str,
    services: &mut tilo_service::Services,
) -> Option<Result<Started, ApiError>> {
    let wanted = media_action(asked);
    if wanted.is_empty() {
        return None;
    }
    let resources = services.resources();
    for row in rows.rows().into_iter().rev() {
        let Some((address, _, Some(spec))) = located(&resources, &row) else {
            continue;
        };
        let declared: Vec<&str> = spec.actions.iter().map(|a| a.id.as_str()).collect();
        // `pause` and `play` each stand for a press of play-pause only on the resource that can
        // take it now: its reading says whether it plays, by the field the type calls `state`
        // or `playing`.
        let Some(action) = wanted.iter().copied().find(|name| declared.contains(name)) else {
            continue;
        };
        let action = if matches!(action, "pause" | "play") {
            let playing = services
                .read(&address)
                .as_ref()
                .is_some_and(reads_as_playing);
            match (
                playing,
                declared.contains(&"pause"),
                declared.contains(&"play"),
            ) {
                (true, true, _) => "pause",
                (false, _, true) => "play",
                _ => continue,
            }
        } else {
            action
        };
        return services.invoke(&address, action, &Map::new());
    }
    None
}

/// Whether a resource's reading says it plays now: a `playing` field that is true, or a `state`
/// that reads `playing`.
fn reads_as_playing(reading: &Value) -> bool {
    let fields = reading.get("fields").unwrap_or(reading);
    fields.get("playing").and_then(Value::as_bool) == Some(true)
        || fields
            .get("state")
            .and_then(Value::as_str)
            .is_some_and(|state| state == "playing")
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use ui::live_activity::{Control, ControlKind, Kind, Published, Summary};

    use super::*;

    const CLOCKS: &str = ui::apps::CLOCKS;

    /// The rows of a session with nothing live in a workspace, beside `board`'s activities.
    fn sources(board: &Arc<runtime_collab::LiveActivities>) -> Sources {
        Sources {
            rows: tilo_workspace_service::live::LiveRows::default(),
            activities: std::rc::Rc::new(crate::shell::activities::Activities::new(Arc::clone(
                board,
            ))),
            readings: crate::status::Shared::default(),
            pressed: std::cell::RefCell::default(),
            cards: std::cell::RefCell::default(),
            drawn: crate::cards::Cards::new(),
            collapsed: std::cell::RefCell::default(),
            revision: std::cell::Cell::default(),
            services: std::rc::Rc::default(),
        }
    }

    /// Clocks' running timer, with the pause and the end every timer offers.
    fn timer(board: &runtime_collab::LiveActivities) {
        let summary = Summary::new(Kind::Timer, "Tea", "1:30")
            .with_id("timer-1")
            .with_control(Control::new("toggle", "Pause", ControlKind::Primary))
            .with_control(Control::new("close", "End", ControlKind::Close));
        board.show(
            CLOCKS,
            Some(serde_json::to_value(Published::of(summary)).expect("JSON")),
        );
    }

    /// A Rust app's timer is a row of `device.live`: its control and its end reach the app through
    /// the row's own actions, keeping it off the bar holds on this device until the row goes, and a
    /// live resource is acted on by its own actions, never the session's.
    #[tokio::test]
    async fn an_apps_activity_is_pressed_ended_and_kept_off_the_bar_through_the_live_rows() {
        let board = Arc::new(runtime_collab::LiveActivities::default());
        let clocks = runtime_collab::live_activity::LiveActivityServer::new(
            CLOCKS,
            Arc::clone(&board),
            Arc::new(|| Ok(())),
        );
        let live = sources(&board);
        timer(&board);
        let rows = live.read().rows;
        assert_eq!(rows.len(), 1, "{rows:?}");
        let key = rows[0].key.clone();
        assert_eq!(rows[0].title, "Tea");
        assert_eq!(
            rows[0]
                .controls
                .iter()
                .map(|c| c.id.as_str())
                .collect::<Vec<_>>(),
            ["toggle", "close"]
        );
        assert!(!rows[0].collapsed);

        // The row's control is the app's.
        live.act(LiveEffect::PressLive {
            key: key.clone(),
            control: "toggle".into(),
        })
        .expect("pressed");
        let (serial, pressed) = clocks.controls(0).await.expect("Clocks takes the press");
        assert_eq!(
            serde_json::from_slice::<Value>(&pressed).expect("JSON"),
            serde_json::json!([{ "command": "toggle", "activity_id": "timer-1" }])
        );

        // Off the bar: the reading says so, and the shell keeps it to quick settings.
        let before = live.revision();
        live.act(LiveEffect::KeepOffBar {
            key: key.clone(),
            off: None,
        })
        .expect("kept off the bar");
        assert!(live.read().rows[0].collapsed);
        assert_ne!(live.revision(), before, "the shell hears of it");
        live.act(LiveEffect::KeepOffBar {
            key: key.clone(),
            off: Some(false),
        })
        .expect("back on the bar");
        assert!(!live.read().rows[0].collapsed);

        // Ending it asks the app, which offers an end.
        live.act(LiveEffect::EndLive { key: key.clone() })
            .expect("ended");
        let (_, ended) = clocks.controls(serial).await.expect("Clocks takes the end");
        assert_eq!(
            serde_json::from_slice::<Value>(&ended).expect("JSON"),
            serde_json::json!([{ "command": "close", "activity_id": "timer-1" }])
        );

        // Nothing else is called that, and a live resource is ended by its type.
        assert!(matches!(
            live.act(LiveEffect::KeepOffBar {
                key: "activity:so.tilo.music:none".into(),
                off: None,
            }),
            Err(ApiError::InvalidArguments(_))
        ));
        assert!(matches!(
            live.act(LiveEffect::EndLive {
                key: "tl:so.tilo.tube:video-1".into(),
            }),
            Err(ApiError::InvalidArguments(_))
        ));
    }

    /// An activity of `app` called `title`, which asks to stand over the windows when `floats`,
    /// with a length when `length` says one.
    fn show(
        board: &runtime_collab::LiveActivities,
        app: &str,
        kind: Kind,
        title: &str,
        floats: bool,
        length: Option<u64>,
    ) {
        let mut summary = Summary::new(kind, title, "")
            .with_id(format!("{title}-1"))
            .with_control(Control::new("toggle", "Pause", ControlKind::Primary));
        if let Some(length) = length {
            summary = summary.with_progress(0.25).with_length(length);
        }
        let published = Published {
            summary: Some(summary),
            overlay: floats,
            announce: None,
        };
        board.show(app, Some(serde_json::to_value(published).expect("JSON")));
    }

    fn cards(live: &Sources) -> Vec<String> {
        live.read()
            .rows
            .into_iter()
            .filter(|row| row.card)
            .map(|row| row.title)
            .collect()
    }

    /// What asks to float is a card; what the person pulls out of the pill becomes one, and folds
    /// back; at most three stand, the ones touched last; a card the person moves keeps its place
    /// until it is docked again.
    #[test]
    fn cards_stand_as_their_apps_ask_and_as_the_person_leaves_them() {
        let board = Arc::new(runtime_collab::LiveActivities::default());
        let live = sources(&board);
        show(&board, "so.tilo.calls", Kind::Call, "Ada", true, None);
        show(
            &board,
            "so.tilo.music",
            Kind::Music,
            "Song",
            false,
            Some(200),
        );
        show(&board, CLOCKS, Kind::Timer, "Tea", false, None);
        show(
            &board,
            "so.tilo.maps",
            Kind::Navigation,
            "Route",
            false,
            None,
        );
        assert_eq!(cards(&live), ["Ada"], "only the call asks to float");

        let key = |title: &str| {
            live.read()
                .rows
                .into_iter()
                .find(|row| row.title == title)
                .expect("a row")
                .key
        };
        for title in ["Song", "Tea", "Route"] {
            live.act(LiveEffect::ExpandLive { key: key(title) })
                .expect("pulled out");
        }
        let standing = cards(&live);
        assert_eq!(standing.len(), MAX_CARDS, "{standing:?}");
        assert!(
            !standing.contains(&"Ada".to_owned()),
            "the card touched longest ago folds into the pill: {standing:?}"
        );
        assert!(
            live.read()
                .rows
                .iter()
                .any(|row| row.title == "Route" && row.expanded)
        );

        // Pressing a control on the call touches it: it stands again, and the oldest goes.
        live.act(LiveEffect::PressLive {
            key: key("Ada"),
            control: "toggle".into(),
        })
        .expect("pressed");
        let standing = cards(&live);
        assert!(standing.contains(&"Ada".to_owned()), "{standing:?}");
        assert!(!standing.contains(&"Song".to_owned()), "{standing:?}");

        // Folded back, it is a row of the pill again, until pulled out.
        live.act(LiveEffect::FoldLive { key: key("Route") })
            .expect("folded");
        assert!(!cards(&live).contains(&"Route".to_owned()));
        assert!(
            !live
                .read()
                .rows
                .iter()
                .any(|row| row.title == "Route" && row.expanded)
        );

        // A moved card keeps its place; docked again, it has none.
        live.act(LiveEffect::MoveLive {
            key: key("Tea"),
            at: Some(vec![40, 120]),
        })
        .expect("moved");
        let tea = live.read().rows.into_iter().find(|row| row.title == "Tea");
        assert_eq!(tea.map(|row| (row.x, row.y)), Some((Some(40), Some(120))));
        live.act(LiveEffect::MoveLive {
            key: key("Tea"),
            at: None,
        })
        .expect("docked");
        let tea = live.read().rows.into_iter().find(|row| row.title == "Tea");
        assert_eq!(tea.map(|row| (row.x, row.y)), Some((None, None)));

        // The song has a length: the card says where it is, and a seek reaches its app.
        let song = live
            .read()
            .rows
            .into_iter()
            .find(|row| row.title == "Song")
            .expect("song");
        assert_eq!(
            (song.elapsed.as_str(), song.remaining.as_str()),
            ("0:50", "-2:30")
        );
        assert_eq!(song.progress, Some(0.25));
        live.act(LiveEffect::SeekLive {
            key: song.key.clone(),
            fraction: 0.5,
        })
        .expect("sought");
        assert!(
            matches!(
                live.act(LiveEffect::SeekLive {
                    key: key("Tea"),
                    fraction: 0.5
                }),
                Err(ApiError::InvalidArguments(_))
            ),
            "a timer says no length to take it along"
        );
        assert!(matches!(
            live.act(LiveEffect::SeekLive {
                key: song.key,
                fraction: 1.5
            }),
            Err(ApiError::InvalidArguments(_))
        ));
    }

    /// An activity its app cannot end from outside goes away when the person puts it away, until
    /// the app shows another one.
    #[test]
    fn what_cannot_be_ended_is_put_away_until_it_changes() {
        let board = Arc::new(runtime_collab::LiveActivities::default());
        let live = sources(&board);
        show(
            &board,
            "so.tilo.maps",
            Kind::Navigation,
            "Route",
            true,
            None,
        );
        let key = live.read().rows[0].key.clone();
        live.act(LiveEffect::EndLive { key }).expect("put away");
        assert!(live.read().rows.is_empty());
        show(&board, "so.tilo.maps", Kind::Navigation, "Home", true, None);
        assert_eq!(cards(&live), ["Home"]);
    }

    /// The card of this device's call, which the system stands over the windows for any app's
    /// call while the person looks elsewhere.
    #[cfg(target_os = "linux")]
    mod call_card {
        use std::sync::Arc;
        use std::time::{Duration, Instant};

        use runtime_collab::native_media::{Command, Input, Issue, Request, Snapshot, State};
        use tilo_native_media::{
            CallTarget, Caller, MediaFactory, MediaSession, NativeMediaService, Prepare,
        };

        use super::sources;
        use crate::call_card::{CallCard, Open};

        const APP: &str = "so.tilo.chat";
        const PLACE: &str = "voice/lounge";

        /// A carrier that puts this device into any channel at once, with one microphone.
        struct Carrier;

        impl MediaFactory for Carrier {
            fn prepare(&self, _target: CallTarget) -> Prepare {
                Box::pin(async { Ok(Box::new(Call) as Box<dyn MediaSession>) })
            }
        }

        struct Call;

        impl MediaSession for Call {
            fn tick(&mut self, _now: Instant) -> Result<State, Issue> {
                Ok(State::Live)
            }

            fn inputs(&self) -> Vec<Input> {
                vec![Input {
                    id: "mic".to_owned(),
                    label: "Microphone".to_owned(),
                }]
            }

            fn start_capture(&mut self, _input: &str) -> Result<(), Issue> {
                Ok(())
            }
        }

        /// What the app's own connection is to the call service.
        fn chat() -> Caller {
            Caller::connected(7, APP, "the-workspace", "chat-record", Arc::new(|| Ok(())))
        }

        /// One step the app takes, with the intent its own counter gives it.
        fn step(media: &NativeMediaService, intent: u64, command: Command) -> Snapshot {
            media
                .command_for(
                    chat(),
                    Request {
                        version: 1,
                        intent,
                        command,
                    },
                )
                .expect("the app's own step is taken")
        }

        async fn until_live(media: &NativeMediaService) {
            for _ in 0..400 {
                if media
                    .own_call()
                    .is_some_and(|call| call.state == State::Live)
                {
                    return;
                }
                tokio::time::sleep(Duration::from_millis(5)).await;
            }
            panic!("the call never became live");
        }

        /// The call's row, and the ids of its controls.
        fn row(live: &super::Sources) -> Option<(super::LiveItem, Vec<String>)> {
            live.read()
                .rows
                .into_iter()
                .find(|row| row.kind == "call")
                .map(|row| {
                    let controls = row.controls.iter().map(|c| c.id.clone()).collect();
                    (row, controls)
                })
        }

        #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
        async fn the_card_mutes_leaves_and_opens_a_call_without_breaking_the_apps_own_steps() {
            let board = Arc::new(runtime_collab::LiveActivities::default());
            let live = sources(&board);
            let media = NativeMediaService::new();
            assert!(media.install_factory(Arc::new(Carrier)));
            let mut card = CallCard::default();

            // Chat joins and unmutes at once: the call is not ended for it, and the person is
            // heard once it is live.
            step(
                &media,
                1,
                Command::Join {
                    channel_id: PLACE.to_owned(),
                },
            );
            let asked = step(&media, 2, Command::SetMuted { muted: false });
            assert_eq!(
                asked.state,
                State::Joining,
                "unmuting on the way in keeps the call"
            );
            until_live(&media).await;
            assert!(
                !media.own_call().expect("in the call").muted,
                "the unmute asked on the way in is carried out once the call is live"
            );

            // While the person looks at Chat, no card stands over it.
            assert_eq!(card.pass(&board, &media, "Lounge", |_| true), None);
            assert!(row(&live).is_none(), "no card over the call's own window");

            // Looking elsewhere, the call's card stands over the windows with its controls.
            card.pass(&board, &media, "Lounge", |_| false);
            let (shown, controls) = row(&live).expect("the call's card");
            assert_eq!(shown.title, "Lounge");
            assert_eq!(shown.source, "Chat");
            assert!(shown.floats && shown.card, "it stands over the windows");
            assert_eq!(controls, ["mute", "deafen", "leave", "open"]);

            // Mute on the card mutes the call, and says so.
            live.press(&shown.key, "mute").expect("the card's mute");
            card.pass(&board, &media, "Lounge", |_| false);
            assert!(media.own_call().expect("in the call").muted);
            let (_, controls) = row(&live).expect("the call's card");
            assert_eq!(controls[0], "unmute", "the card offers to unmute");

            // The app's own next step is taken: the card did not move its count.
            let heard = step(&media, 3, Command::SetMuted { muted: false });
            assert!(
                !heard.muted && heard.capture_active,
                "the app unmutes as it asked"
            );
            let muted = step(&media, 4, Command::SetMuted { muted: true });
            assert!(muted.muted);

            // Open names the call's place, in the app that holds it.
            live.press(&shown.key, "open").expect("the card's open");
            assert_eq!(
                card.pass(&board, &media, "Lounge", |_| false),
                Some(Open {
                    workspace: "the-workspace".to_owned(),
                    app: APP.to_owned(),
                    record: "chat-record".to_owned(),
                    resource: format!("tl:{APP}:{PLACE}"),
                })
            );

            // Leave ends the call, and the card goes with it.
            card.pass(&board, &media, "Lounge", |_| false);
            let (shown, _) = row(&live).expect("the call's card");
            live.press(&shown.key, "leave").expect("the card's leave");
            card.pass(&board, &media, "Lounge", |_| false);
            assert!(media.own_call().is_none(), "the call is over");
            assert!(row(&live).is_none(), "and its card is gone");
        }
    }
}
