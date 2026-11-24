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
use tilo_resource::Effect;
use tilo_service::Started;

/// The resource the rows are published under.
pub(crate) const RESOURCE: &str = "session:live";

/// The rows as a view of another language reads them (`device.live`).
#[must_use]
pub fn surface() -> tilo_resource::surface::Surface {
    tilo_resource::surface::reading_surface::<Live, LiveEffect>("live", "Live")
}

/// What is live now, the one live longest first.
#[derive(
    Clone,
    Debug,
    Default,
    PartialEq,
    Eq,
    serde::Serialize,
    serde::Deserialize,
    tilo_resource::JsonSchema,
)]
pub(crate) struct Live {
    pub rows: Vec<LiveItem>,
}

/// One live thing.
#[derive(
    Clone,
    Debug,
    Default,
    PartialEq,
    Eq,
    serde::Serialize,
    serde::Deserialize,
    tilo_resource::JsonSchema,
)]
pub(crate) struct LiveItem {
    /// What names the row while it is live: the resource's address, or the app and its activity.
    pub key: String,
    /// The resource that is live, by the address the session publishes it under
    /// (`tl:<app>:<id>`); empty for what this machine plays or runs that no resource holds.
    pub resource: String,
    /// The app it is of, by id.
    pub app: String,
    /// The app as the person knows it ("Music", "Clocks").
    pub source: String,
    /// It is live on this device.
    pub here: bool,
    /// What the device it is live on calls itself, when that is another device: where a track
    /// plays or a timer runs.
    pub device: String,
    /// When it went live, ISO-8601 UTC; empty where that is not known.
    pub since: String,
    /// What it is, for a row the shell draws itself (no resource of it): the song, the timer.
    pub title: String,
    /// The line under it: the artist, who speaks.
    pub subtitle: String,
    /// The app's glyph, by lucide name.
    pub icon: String,
    /// What it reads now: "4:32", "muted".
    pub value: String,
    /// It plays now (a player, a call): a play-pause button shows pause.
    pub playing: bool,
    /// What the person can do to it from the shell, for a row the shell draws itself.
    pub controls: Vec<LiveControl>,
    /// The person kept it off this device's bar: quick settings still shows it.
    pub collapsed: bool,
}

/// A control of a row the shell draws itself.
#[derive(
    Clone,
    Debug,
    Default,
    PartialEq,
    Eq,
    serde::Serialize,
    serde::Deserialize,
    tilo_resource::JsonSchema,
)]
pub(crate) struct LiveControl {
    /// What its app calls it, pressed back to it as it is.
    pub id: String,
    /// What the button says.
    pub label: String,
    /// The glyph it shows, by lucide name.
    pub icon: String,
}

/// What the rows offer beside their reading.
#[derive(Clone, Debug, PartialEq, Effect)]
#[effect(operation = "so.tilo.session.live")]
pub(crate) enum LiveEffect {
    /// Press a control of something live here that no resource holds: a player's play or pause,
    /// a Rust app's timer's stop
    #[effect(kind = "outside")]
    #[effect(row, label = "session-live-press")]
    PressLive {
        /// The row's key, as the reading lists it.
        key: String,
        /// The control's id.
        control: String,
    },
    /// End something live here that no resource holds: an app that offers a close is asked to
    /// end it, and one that cannot be stopped from outside is put away until it shows something
    /// else. A live resource ends by its type's own action.
    #[effect(kind = "outside")]
    #[effect(row, label = "session-live-close")]
    EndLive {
        /// The row's key, as the reading lists it.
        key: String,
    },
    /// Keep a row off this device's bar, or bring it back: quick settings shows it either way.
    #[effect(kind = "outside")]
    #[effect(row, label = "session-live-collapse")]
    KeepOffBar {
        /// The row's key, as the reading lists it.
        key: String,
        /// Whether it stays off the bar; it does when not said.
        off: Option<bool>,
    },
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
    /// The controls pressed on the player's row, which the session's loop asks the player for:
    /// the player is a service beside the one that publishes the rows.
    pub(crate) pressed: std::cell::RefCell<Vec<String>>,
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
        (
            self.rows.revision().wrapping_add(self.revision.get()),
            self.activities.board().revision(),
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
            rows.push(LiveItem {
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
        for (app, summary) in self.activities.summaries() {
            rows.push(LiveItem {
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
        Live { rows }
    }

    /// Do what `effect` asks of the rows.
    pub(crate) fn act(&self, effect: LiveEffect) -> Result<Started, ApiError> {
        match effect {
            LiveEffect::PressLive { key, control } => self.press(&key, &control),
            LiveEffect::EndLive { key } => self.close(&key),
            LiveEffect::KeepOffBar { key, off } => self.collapse(&key, off),
        }
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
            self.pressed.borrow_mut().push(control.to_owned());
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
        Ok(Started::Done(tilo_actions::ActionOutput::default()))
    }
}

/// Ask the player here for what was pressed on its row since the last pass.
pub(crate) fn press_player(sources: &Sources, services: &mut tilo_service::Services) {
    let pressed = std::mem::take(&mut *sources.pressed.borrow_mut());
    for control in pressed {
        match services.invoke(crate::MEDIA, &control, &Map::new()) {
            Some(Ok(_)) => {}
            Some(Err(error)) => tracing::warn!(%error, control, "the live row's player control"),
            None => tracing::debug!("nothing publishes the player"),
        }
    }
}

fn control(id: &str, label: &str, icon: &str) -> LiveControl {
    LiveControl {
        id: id.to_owned(),
        label: label.to_owned(),
        icon: icon.to_owned(),
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
    let resources = services.resources();
    rows.rows().into_iter().rev().find_map(|row| {
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

    const CLOCKS: &str = crate::shell::activities::CLOCKS_APP;

    /// The rows of a session with nothing live in a workspace, beside `board`'s activities.
    fn sources(board: &Arc<runtime_collab::LiveActivities>) -> Sources {
        Sources {
            rows: tilo_workspace_service::live::LiveRows::default(),
            activities: std::rc::Rc::new(crate::shell::activities::Activities::new(
                Arc::clone(board),
                crate::cards::Cards::new(),
            )),
            readings: crate::status::Shared::default(),
            pressed: std::cell::RefCell::default(),
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
}
