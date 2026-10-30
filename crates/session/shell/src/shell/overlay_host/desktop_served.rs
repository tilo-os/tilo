//! The desktop's layout actions, served to agents
//! ([`crate::shell::desktop_door`]): each runs here, on the thread that
//! draws the desktop, through [`crate::shell::desktop_layout`], and a
//! window opened at a resource is opened there once it is up.

#[allow(
    clippy::wildcard_imports,
    reason = "private implementation module shares its enclosing host types"
)]
use super::*;
use serde_json::{Value, json};
use tilo_action_broker::services::HostStepOutcome;

use crate::shell::desktop_door::{Command, Parked, Served, refused};
use crate::shell::desktop_layout::{self, Actor};
use crate::shell::state::{GroupId, RoomId, WindowWidth};

/// A value as an action answers it.
#[allow(
    clippy::unnecessary_wraps,
    reason = "every arm of a served step answers the step's outcome"
)]
fn answered(value: Value) -> HostStepOutcome {
    Ok(tilo_actions::ActionOutput {
        value: Some(value),
        resources: Vec::new(),
    })
}

/// Ids cross to JavaScript as decimal text.
fn id(raw: u64) -> Value {
    Value::String(raw.to_string())
}

fn widths_value(widths: &[(GroupId, WindowWidth)]) -> Value {
    Value::Array(
        widths
            .iter()
            .map(|(group, width)| json!({ "group": id(*group), "width": width.permille() }))
            .collect(),
    )
}

impl ShellOverlayHost {
    /// Run what the broker asked of the desktop's layout, then open the
    /// windows waiting to be opened at a resource whose app may be up now.
    pub(crate) fn serve_desktop_jobs(&mut self) {
        for job in crate::shell::desktop_door::take_jobs() {
            job(self);
        }
        self.open_windows_at_their_resources();
        self.answer_opened_windows();
    }

    /// Answer each `open` whose window is up: it published its actions and,
    /// opened for a resource, was opened at it. One whose place closed or
    /// whose app did not start is answered with why.
    #[allow(
        clippy::significant_drop_tightening,
        reason = "the tree guard borrows from the chrome guard; both end with the block that reads the window"
    )]
    fn answer_opened_windows(&self) {
        let parked = crate::shell::desktop_door::take_parked();
        if parked.is_empty() {
            return;
        }
        let door = self.shared_window_follow.door();
        for Parked {
            tab,
            mut value,
            answer,
        } in parked
        {
            let Some(chrome) = self.chrome.as_ref() else {
                answer.complete(Err(refused("this session shows no desktop")));
                continue;
            };
            let (present, failed, instance, opening) = {
                let state = chrome.borrow();
                let opening = state
                    .desktop_layout
                    .openings_pending()
                    .any(|waiting| waiting == tab);
                let tree = state.tree();
                let shown = tree
                    .desktop
                    .workspaces
                    .iter()
                    .flat_map(|workspace| &workspace.strips)
                    .flat_map(|strip| &strip.window_groups)
                    .flat_map(|group| &group.tabs)
                    .find(|candidate| candidate.id == tab);
                let failed = shown.and_then(|shown| match &shown.tile {
                    crate::shell::state::TileContent::LocalApp { note, .. }
                        if !note.is_empty() && note != crate::shell::peer_windows::OPENING =>
                    {
                        Some(note.clone())
                    }
                    _ => None,
                });
                let instance = tree.local_app_window(tab).map(|(_, _, instance)| instance);
                (shown.is_some(), failed, instance, opening)
            };
            if !present {
                answer.complete(Err(refused("the window closed before it was up")));
                continue;
            }
            let provider = instance
                .zip(door.as_ref())
                .and_then(|(instance, door)| door.window_peer(instance));
            match (provider, failed) {
                (Some(provider), _) if !opening => {
                    if let Some(fields) = value.as_object_mut() {
                        fields.insert("provider".into(), Value::String(provider.to_string()));
                    }
                    answer.complete(answered(value));
                }
                (None, Some(why)) => {
                    answer.complete(Err(refused(format!("the window did not open: {why}"))));
                }
                _ => crate::shell::desktop_door::park(Parked { tab, value, answer }),
            }
        }
    }

    /// Run `command` for `actor` in `room`; its answer.
    #[allow(
        clippy::too_many_lines,
        reason = "one arm per layout action, each a call and its answer"
    )]
    pub(crate) fn serve_desktop(
        &mut self,
        room: RoomId,
        actor: &Actor,
        command: Command,
    ) -> Served {
        let Some(chrome) = self.chrome.clone() else {
            return Served::Now(Err(refused("this session shows no desktop")));
        };
        if let Command::Open {
            target,
            place,
            background,
        } = command
        {
            let door = self.shared_window_follow.door();
            let room_text = room.to_string();
            let publisher = |resource: &str| door.as_ref()?.publisher_of(&room_text, resource);
            let opened = desktop_layout::open(
                &mut chrome.borrow_mut(),
                room,
                &target,
                place,
                background,
                actor,
                &publisher,
            );
            return match opened {
                Ok(opened) => Served::WhenUp {
                    tab: opened.tab,
                    value: json!({
                        "window": id(opened.tab),
                        "group": id(opened.group),
                        "strip": id(opened.strip.raw()),
                        "app": opened.app,
                    }),
                },
                Err(error) => Served::Now(Err(refused(error))),
            };
        }
        Served::Now(self.serve_layout(&chrome, room, actor, command))
    }

    /// Run a layout `command` that answers at once.
    fn serve_layout(
        &mut self,
        chrome: &crate::shell::chrome_state::Chrome,
        room: RoomId,
        actor: &Actor,
        command: Command,
    ) -> HostStepOutcome {
        match command {
            Command::Open { .. } => Err(refused("open answers once its window is up")),
            Command::CreateStrip { name } => {
                let strip = desktop_layout::create_strip(
                    &mut chrome.borrow_mut(),
                    room,
                    &name,
                    actor,
                    !actor.is_agent,
                )
                .map_err(refused)?;
                answered(json!({ "strip": id(strip.raw()), "name": name.trim() }))
            }
            Command::RenameStrip { strip, name } => {
                desktop_layout::rename_strip(&mut chrome.borrow_mut(), room, strip, &name, actor)
                    .map_err(refused)?;
                answered(json!({ "strip": id(strip.raw()), "name": name.trim() }))
            }
            Command::MoveGroup {
                group,
                strip,
                index,
            } => {
                let moved = desktop_layout::move_group(
                    &mut chrome.borrow_mut(),
                    room,
                    group,
                    strip,
                    index,
                    actor,
                )
                .map_err(refused)?;
                answered(json!({ "moved": moved, "group": id(group), "strip": id(strip.raw()) }))
            }
            Command::Arrange {
                strip,
                layout,
                order,
                widths,
            } => {
                let set = desktop_layout::arrange(
                    &mut chrome.borrow_mut(),
                    room,
                    strip,
                    layout,
                    (order.as_deref(), widths.as_deref()),
                    actor,
                )
                .map_err(refused)?;
                self.show_group_widths(&set);
                answered(json!({ "strip": id(strip.raw()), "widths": widths_value(&set) }))
            }
            Command::ListWindows => {
                let toolbars = &self.window_toolbars;
                let shows = |tab| {
                    toolbars
                        .toolbar(tab)
                        .and_then(|toolbar| toolbar.location.as_ref())
                        .and_then(|location| location.anchor.clone())
                        .filter(|anchor| !anchor.is_empty())
                };
                let windows = desktop_layout::list_windows(&chrome.borrow(), room, &shows)
                    .map_err(refused)?;
                answered(json!({
                    "windows": windows
                        .into_iter()
                        .map(|window| json!({
                            "id": id(window.tab),
                            "app": window.app,
                            "resource": window.resource,
                            "title": window.title,
                            "group": id(window.group),
                            "strip": id(window.strip.raw()),
                            "strip_name": window.strip_name,
                            "shown": window.shown,
                        }))
                        .collect::<Vec<_>>(),
                }))
            }
            Command::Undo { run } => {
                let (undone, widths) =
                    desktop_layout::undo(&mut chrome.borrow_mut(), room, run.as_deref(), actor)
                        .map_err(refused)?;
                self.show_group_widths(&widths);
                answered(json!({
                    "undone": undone.changes,
                    "kept": undone.kept,
                    "run": undone.run,
                }))
            }
        }
    }

    /// Lay out the groups of `widths` at their widths now: the tree holds
    /// them already, the strip rows draw from their own.
    #[allow(
        clippy::needless_pass_by_ref_mut,
        reason = "the rows are mutated through this host's owned GroupRow pointers; the exclusive borrow keeps that mutation unaliased"
    )]
    fn show_group_widths(&mut self, widths: &[(GroupId, WindowWidth)]) {
        if widths.is_empty() {
            return;
        }
        for &ptr in self.group_row_ptrs.values() {
            // SAFETY: see field doc on `group_row_ptrs`.
            let row: &mut GroupRow = unsafe { &mut *ptr };
            for group in &mut row.groups {
                if let Some((_, width)) = widths.iter().find(|(id, _)| *id == group.id) {
                    group.set_width(match *width {
                        WindowWidth::Narrow => ui::widget::WindowWidth::Narrow,
                        WindowWidth::Normal => ui::widget::WindowWidth::Normal,
                        WindowWidth::Wide => ui::widget::WindowWidth::Wide,
                        WindowWidth::Fraction(permille) => {
                            ui::widget::WindowWidth::Fraction(permille)
                        }
                    });
                }
            }
        }
    }

    /// Open each window an agent opened at a resource there, through the
    /// window's own `open`, once it has published one; a window that
    /// closed meanwhile is let go.
    #[allow(
        clippy::significant_drop_tightening,
        reason = "the tree guard borrows from the chrome guard; both end with the block that reads the window"
    )]
    fn open_windows_at_their_resources(&self) {
        let Some(chrome) = self.chrome.clone() else {
            return;
        };
        let Some(door) = self.shared_window_follow.door() else {
            return;
        };
        let due = chrome
            .borrow_mut()
            .desktop_layout
            .openings_due(door.surfaces_generation());
        for opening in due {
            let instance = {
                let state = chrome.borrow();
                let tree = state.tree();
                let present = tree.desktop.workspaces.iter().any(|workspace| {
                    workspace
                        .strips
                        .iter()
                        .flat_map(|strip| &strip.window_groups)
                        .any(|group| group.tabs.iter().any(|tab| tab.id == opening.tab))
                });
                let instance = tree
                    .local_app_window(opening.tab)
                    .map(|(_, _, instance)| instance);
                drop(tree);
                (present, instance)
            };
            let done = match instance {
                (false, _) => true,
                (true, None) => false,
                (true, Some(instance)) => {
                    match crate::shell::search::providers::app_content::open_in_window(
                        &door,
                        &chrome,
                        (&opening.app, &opening.room.to_string()),
                        instance,
                        &opening.resource,
                    ) {
                        Ok(opened) => opened,
                        Err(error) => {
                            tracing::info!(
                                tab = opening.tab,
                                resource = %opening.resource,
                                ?error,
                                "a window an agent opened could not be opened at its resource"
                            );
                            true
                        }
                    }
                }
            };
            if done {
                chrome.borrow_mut().desktop_layout.opened_at(opening.tab);
            }
        }
    }
}
