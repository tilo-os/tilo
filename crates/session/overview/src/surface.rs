//! The overview on screen: the desktop's overview widget, following the
//! shell that hosts it, and carrying out what the person and anyone acting
//! for them chose.

use std::rc::Rc;

use tilo_desktop::overview::RoomCard;
use tilo_desktop::overview_panel::{OverviewDesk, OverviewPanel, OverviewPick};
use tilo_desktop::strip_switcher::valid_strip_name;
use tilo_desktop_model::{StripId, TabId, WorkspaceId};
use tilo_sheets::{Phase, Seat as _};
use ui::framework::{
    Dispatcher, EffectAnswer, EffectCx, EffectStatus, Effected, LiveRow, ResourceSurface, SurfaceCx,
};
use ui::presence::PresenceCtx;
use ui::text_render::TextRenderer;
use ui::theme::Theme;
use ui::vello::Scene;
use ui::vello::kurbo::Affine;
use ui::widget::{AppModel, Decorator, EventCtx, ModelPath, Widget};
use ui::winit::event::WindowEvent;

use crate::seat::OverviewSeat;
use crate::{Overview, OverviewEffect, fixture, t, t_args};

/// The overview widget over the desktop the shell lends it, or the fixed
/// one when there is no shell.
pub struct OverviewSurface {
    panel: OverviewPanel,
    seat: Option<OverviewSeat>,
    /// The desktop shown when no shell lends one.
    own: OverviewDesk,
    /// The generation of the shell's desktop shown.
    shown: u64,
    /// The shell's opening shown; `None` before the first.
    opening: Option<u64>,
    dispatch: Option<Dispatcher<Overview>>,
}

impl OverviewSurface {
    pub fn new(seat: Option<OverviewSeat>, own: OverviewDesk) -> Self {
        let mut panel = OverviewPanel::new();
        if let Some(seat) = &seat {
            panel.set_flight(seat.flight().clone());
        }
        Self {
            panel,
            seat,
            own,
            shown: 0,
            opening: None,
            dispatch: None,
        }
    }

    /// The overview widget.
    pub fn panel(&self) -> &OverviewPanel {
        &self.panel
    }

    pub fn panel_mut(&mut self) -> &mut OverviewPanel {
        &mut self.panel
    }

    /// What the overview knows of the desktop now.
    fn desk(&self) -> OverviewDesk {
        match &self.seat {
            Some(seat) => seat.desk().1,
            None => self.own.clone(),
        }
    }

    /// Follow the shell: open on a new opening, close when it took the
    /// overview down, show what changed. With no shell (a capture, a test)
    /// the overview is up from the first frame.
    fn follow(&mut self) {
        let Some(seat) = self.seat.clone() else {
            if self.opening.is_none() {
                self.opening = Some(0);
                self.panel.open_on(true, self.own.clone());
            }
            return;
        };
        let presentation = seat.presentation();
        match presentation.phase() {
            Phase::Open if self.opening != Some(presentation.openings()) => {
                self.opening = Some(presentation.openings());
                let (generation, desk) = seat.desk();
                self.shown = generation;
                self.panel.open_on(true, desk);
            }
            Phase::Leaving | Phase::Closed if self.panel.is_open() => {
                self.panel.open_on(false, OverviewDesk::default());
            }
            _ => {}
        }
        if seat.desk_generation() != self.shown {
            let (generation, desk) = seat.desk();
            self.shown = generation;
            self.panel.set_desk(desk);
        }
        // Without pictures to fly back, nothing is left on screen once it
        // leaves; the shell says when the pictures it flies have landed.
        if presentation.phase() == Phase::Leaving && !self.panel.is_drawn() {
            presentation.gone();
        }
    }

    /// Carry out what the person chose. Their own press or key is their
    /// decision; anyone acting for them goes through the door.
    fn settle(&mut self) {
        while let Some(pick) = self.panel.take_pick() {
            let effect = effect_of(&pick, &self.desk());
            match (&self.dispatch, effect) {
                (Some(dispatch), Some(effect)) if !dispatch.is_own_gesture(&effect) => {
                    dispatch.effect(&effect);
                }
                _ => self.carry_out(pick),
            }
        }
    }

    /// Hand `pick` to the shell, and leave when it takes the overview
    /// away.
    fn carry_out(&mut self, pick: OverviewPick) {
        let closes = pick.closes();
        if let Some(seat) = &self.seat {
            if pick != OverviewPick::Dismiss {
                seat.ask(pick);
            }
            if closes {
                seat.presentation().dismissed();
            }
        }
        if closes {
            self.panel.open_on(false, OverviewDesk::default());
        }
    }

    /// Stage `scenario`, as the person would get there.
    fn stage_scenario(&mut self, scenario: Option<&str>) {
        self.follow();
        match scenario {
            Some("keyboard") => self.panel.select_window(102),
            Some("naming") => {
                self.panel.rename_in_place(fixture::READING);
            }
            Some("carrying") => {
                if let Some(target) = self.panel.room_rect(1) {
                    self.panel.carry(104, target.center());
                }
            }
            Some("crowded") => {
                // At the end of the list, as a person who went there.
                self.panel.press_key(
                    &ui::winit::keyboard::Key::Named(ui::winit::keyboard::NamedKey::End),
                    ui::winit::keyboard::ModifiersState::empty(),
                );
            }
            _ => {}
        }
    }
}

/// The effect a pick is, as the door names it.
fn effect_of(pick: &OverviewPick, desk: &OverviewDesk) -> Option<OverviewEffect> {
    let workspace = |id: WorkspaceId| id.raw().to_string();
    let strip = |id: StripId| id.raw().to_string();
    Some(match pick {
        OverviewPick::Window(tab) => OverviewEffect::FocusWindow {
            window: tab.to_string(),
        },
        OverviewPick::Workspace(id) => OverviewEffect::SwitchWorkspace {
            workspace: workspace(*id),
        },
        OverviewPick::NewWorkspace => OverviewEffect::CreateWorkspace,
        OverviewPick::JoinWorkspace => OverviewEffect::JoinWorkspace,
        OverviewPick::LeaveWorkspace(id) => OverviewEffect::LeaveWorkspace {
            workspace: workspace(*id),
        },
        OverviewPick::CloseWindow(tab) => OverviewEffect::CloseWindow {
            window: tab.to_string(),
        },
        OverviewPick::MoveWindow { tab, to } => OverviewEffect::MoveWindow {
            window: tab.to_string(),
            workspace: workspace(*to),
        },
        OverviewPick::MoveWindowToStrip { tab, strip: to } => OverviewEffect::MoveWindowToStrip {
            window: tab.to_string(),
            strip: strip(*to),
        },
        OverviewPick::RenameStrip { strip: id, name } => OverviewEffect::RenameStrip {
            strip: strip(*id),
            name: name.clone(),
        },
        OverviewPick::MoveStrip { strip: id, before } => {
            let order: Vec<StripId> = desk
                .cards
                .iter()
                .find(|card| card.is_current)?
                .strips
                .iter()
                .map(|strip| strip.id)
                .collect();
            let at = order.iter().position(|s| s == id)?;
            let rest: Vec<StripId> = order.iter().copied().filter(|s| s != id).collect();
            let to = match before {
                Some(before) => rest.iter().position(|s| s == before)?,
                None => rest.len(),
            };
            let by = i64::try_from(to)
                .ok()?
                .checked_sub(i64::try_from(at).ok()?)?;
            OverviewEffect::MoveStrip {
                strip: strip(*id),
                by,
            }
        }
        OverviewPick::Dismiss => return None,
    })
}

/// The workspace `name` names: its id, or its name (any case).
fn find_workspace<'a>(desk: &'a OverviewDesk, name: &str) -> Result<&'a RoomCard, String> {
    let name = name.trim();
    desk.cards
        .iter()
        .find(|card| name.parse::<u64>().is_ok_and(|id| card.id.raw() == id))
        .or_else(|| {
            desk.cards
                .iter()
                .find(|card| card.name.eq_ignore_ascii_case(name))
        })
        .ok_or_else(|| t_args("overview-app-no-workspace", &[("name", name.into())]))
}

/// The window `name` names, in any workspace: its id, or its title (any
/// case), the workspace you are in first.
fn find_window(desk: &OverviewDesk, name: &str) -> Result<TabId, String> {
    let name = name.trim();
    let mut cards: Vec<&RoomCard> = desk.cards.iter().collect();
    cards.sort_by_key(|card| !card.is_current);
    let tabs = || {
        cards
            .iter()
            .flat_map(|card| &card.strips)
            .flat_map(|strip| &strip.groups)
            .flat_map(|group| &group.tabs)
    };
    tabs()
        .find(|tab| name.parse::<u64>().is_ok_and(|id| tab.id == id))
        .or_else(|| tabs().find(|tab| tab.title.eq_ignore_ascii_case(name)))
        .map(|tab| tab.id)
        .ok_or_else(|| t_args("overview-app-no-window", &[("name", name.into())]))
}

/// The strip of the workspace you are in that `name` names: its id, or its
/// name (any case), with its place and every strip's id in order.
fn find_strip(desk: &OverviewDesk, name: &str) -> Result<(usize, Vec<StripId>), String> {
    let name = name.trim();
    let missing = || t_args("overview-app-no-strip", &[("name", name.into())]);
    let here = desk
        .cards
        .iter()
        .find(|card| card.is_current)
        .ok_or_else(missing)?;
    let order: Vec<StripId> = here.strips.iter().map(|strip| strip.id).collect();
    let at = here
        .strips
        .iter()
        .position(|strip| name.parse::<u64>().is_ok_and(|id| strip.id.raw() == id))
        .or_else(|| {
            here.strips
                .iter()
                .position(|strip| strip.name.eq_ignore_ascii_case(name))
        })
        .ok_or_else(missing)?;
    Ok((at, order))
}

/// The pick `effect` asks for, against `desk`.
fn pick_of(effect: &OverviewEffect, desk: &OverviewDesk) -> Result<OverviewPick, String> {
    Ok(match effect {
        OverviewEffect::Open | OverviewEffect::Close => OverviewPick::Dismiss,
        OverviewEffect::SwitchWorkspace { workspace } => {
            OverviewPick::Workspace(find_workspace(desk, workspace)?.id)
        }
        OverviewEffect::FocusWindow { window } => OverviewPick::Window(find_window(desk, window)?),
        OverviewEffect::MoveWindow { window, workspace } => {
            let tab = find_window(desk, window)?;
            let card = find_workspace(desk, workspace)?;
            let there = card
                .strips
                .iter()
                .flat_map(|strip| &strip.groups)
                .any(|group| group.tabs.iter().any(|t| t.id == tab));
            if there {
                return Err(t_args(
                    "overview-app-already-here",
                    &[("name", card.name.clone().into())],
                ));
            }
            OverviewPick::MoveWindow { tab, to: card.id }
        }
        OverviewEffect::MoveWindowToStrip { window, strip } => {
            let tab = find_window(desk, window)?;
            let (at, order) = find_strip(desk, strip)?;
            let strip = order
                .get(at)
                .copied()
                .ok_or_else(|| t("overview-app-strip-there"))?;
            OverviewPick::MoveWindowToStrip { tab, strip }
        }
        OverviewEffect::CloseWindow { window } => {
            OverviewPick::CloseWindow(find_window(desk, window)?)
        }
        OverviewEffect::CreateWorkspace => {
            if !desk.can_add {
                return Err(t("overview-app-cannot-add"));
            }
            OverviewPick::NewWorkspace
        }
        OverviewEffect::JoinWorkspace => {
            if !desk.can_join {
                return Err(t("overview-app-cannot-join"));
            }
            OverviewPick::JoinWorkspace
        }
        OverviewEffect::LeaveWorkspace { workspace } => {
            let card = find_workspace(desk, workspace)?;
            if !desk.leavable.contains(&card.id) {
                return Err(t_args(
                    "overview-app-cannot-leave",
                    &[("name", card.name.clone().into())],
                ));
            }
            OverviewPick::LeaveWorkspace(card.id)
        }
        OverviewEffect::RenameStrip { strip, name } => {
            let (at, order) = find_strip(desk, strip)?;
            let id = order
                .get(at)
                .copied()
                .ok_or_else(|| t("overview-app-strip-there"))?;
            if !desk.renamable.contains(&id) {
                return Err(t("overview-app-not-renamable"));
            }
            let name = valid_strip_name(name).ok_or_else(|| t("overview-app-bad-strip-name"))?;
            OverviewPick::RenameStrip {
                strip: id,
                name: name.to_owned(),
            }
        }
        OverviewEffect::MoveStrip { strip, by } => {
            let (at, order) = find_strip(desk, strip)?;
            let id = order
                .get(at)
                .copied()
                .ok_or_else(|| t("overview-app-strip-there"))?;
            if !desk.renamable.contains(&id) {
                return Err(t("overview-app-not-renamable"));
            }
            let last = i64::try_from(order.len().saturating_sub(1)).unwrap_or(0);
            let from = i64::try_from(at).unwrap_or(0);
            let to = from.saturating_add(*by).clamp(0, last);
            if to == from {
                return Err(t("overview-app-strip-there"));
            }
            let rest: Vec<StripId> = order.iter().copied().filter(|s| *s != id).collect();
            let before = usize::try_from(to)
                .ok()
                .and_then(|to| rest.get(to))
                .copied();
            OverviewPick::MoveStrip { strip: id, before }
        }
    })
}

impl Decorator for OverviewSurface {
    type Inner = OverviewPanel;

    fn inner(&self) -> &OverviewPanel {
        &self.panel
    }

    fn inner_mut(&mut self) -> &mut OverviewPanel {
        &mut self.panel
    }

    fn sync(&mut self, state: &AppModel, path: &ModelPath) {
        self.follow();
        self.panel.sync(state, path);
    }

    fn draw(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        text_renderer: &mut TextRenderer,
        theme: &Theme,
        presence: &PresenceCtx,
    ) {
        self.follow();
        self.panel
            .draw(scene, transform, text_renderer, theme, presence);
        self.follow();
    }

    fn handle_event(&mut self, event: &WindowEvent, ctx: &mut EventCtx<'_>) {
        // A key that follows the opening at once is the open overview's,
        // drawn or not yet.
        self.follow();
        if !self.panel.is_open() {
            return;
        }
        self.panel.handle_event(event, ctx);
        // The overview is modal over its display: no press, wheel or
        // finger reaches what is under it. Keys it does not use (a shell
        // chord) and the pointer's moves go on.
        if matches!(
            event,
            WindowEvent::MouseInput { .. } | WindowEvent::MouseWheel { .. } | WindowEvent::Touch(_)
        ) {
            *ctx.handled = true;
        }
        self.settle();
        if let Some(seat) = &self.seat {
            seat.set_holds_keyboard(self.panel.is_naming());
        }
    }

    fn perform_semantic_action(
        &mut self,
        target: &str,
        action: &ui::a11y::Action,
        state: &mut ui::widget::Edits<'_>,
        path: &ModelPath,
    ) -> bool {
        let done = self
            .panel
            .perform_semantic_action(target, action, state, path);
        if done {
            self.settle();
        }
        done
    }
}

impl ResourceSurface<Overview, OverviewEffect> for OverviewSurface {
    fn present(&mut self, _state: &Rc<Overview>) {}

    fn attach(&mut self, cx: SurfaceCx<Overview>) {
        self.dispatch = Some(cx.dispatcher());
        if let Some(seat) = &self.seat {
            seat.presentation().wake_with(cx.waker());
        }
    }

    fn stage(&mut self, scenario: Option<&str>) {
        self.stage_scenario(scenario);
    }

    fn effect(
        &mut self,
        effect: OverviewEffect,
        _cx: &mut EffectCx<Overview, Self>,
    ) -> EffectAnswer {
        match effect {
            OverviewEffect::Open => {
                if let Some(seat) = &self.seat {
                    seat.ask_open();
                }
            }
            OverviewEffect::Close => self.carry_out(OverviewPick::Dismiss),
            other => {
                let pick = pick_of(&other, &self.desk())?;
                if self.seat.is_none() {
                    return Err("no desktop to act on here".to_owned());
                }
                self.carry_out(pick);
            }
        }
        Ok(Effected::done())
    }

    fn effects(&self) -> Vec<EffectStatus> {
        let desk = self.desk();
        let mut statuses = Vec::new();
        if !desk.can_add {
            statuses.push(EffectStatus::unavailable(
                OverviewEffect::CREATE_WORKSPACE,
                "not-offered",
                t("overview-app-cannot-add"),
            ));
        }
        if !desk.can_join {
            statuses.push(EffectStatus::unavailable(
                OverviewEffect::JOIN_WORKSPACE,
                "not-offered",
                t("overview-app-cannot-join"),
            ));
        }
        statuses
    }

    /// The workspaces and the windows of the one you are in, each reached
    /// through its effect: what an agent reads to find where to go.
    fn live_rows(&self) -> Vec<LiveRow> {
        let desk = self.desk();
        let mut rows = Vec::new();
        for card in &desk.cards {
            rows.push(
                LiveRow::new(
                    format!("overview.desktop/workspace/{}", card.id.raw()),
                    "overview.workspace",
                    card.name.clone(),
                )
                .effect(&OverviewEffect::SwitchWorkspace {
                    workspace: card.id.raw().to_string(),
                }),
            );
            if !card.is_current {
                continue;
            }
            for tab in card
                .strips
                .iter()
                .flat_map(|strip| &strip.groups)
                .flat_map(|group| &group.tabs)
            {
                rows.push(
                    LiveRow::new(
                        format!("overview.desktop/window/{}", tab.id),
                        "overview.window",
                        tab.title.clone(),
                    )
                    .effect(&OverviewEffect::FocusWindow {
                        window: tab.id.to_string(),
                    }),
                );
            }
        }
        rows
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn workspaces_windows_and_strips_are_found_by_name_or_id() {
        let desk = fixture::desk();
        assert_eq!(
            find_workspace(&desk, "research").map(|c| c.id),
            Ok(WorkspaceId(2))
        );
        assert_eq!(find_workspace(&desk, "3").map(|c| c.id), Ok(WorkspaceId(3)));
        assert!(find_workspace(&desk, "Atlantis").is_err());
        assert_eq!(find_window(&desk, "week 44"), Ok(106));
        assert_eq!(find_window(&desk, "201"), Ok(201));
        assert_eq!(find_strip(&desk, "Reading").map(|(at, _)| at), Ok(1));
    }

    #[test]
    fn a_strip_moves_by_places_and_never_past_the_ends() {
        let desk = fixture::desk();
        let down = OverviewEffect::MoveStrip {
            strip: "Writing".into(),
            by: 1,
        };
        assert_eq!(
            pick_of(&down, &desk),
            Ok(OverviewPick::MoveStrip {
                strip: fixture::WORK,
                before: None
            })
        );
        let up = OverviewEffect::MoveStrip {
            strip: "Reading".into(),
            by: -3,
        };
        assert_eq!(
            pick_of(&up, &desk),
            Ok(OverviewPick::MoveStrip {
                strip: fixture::READING,
                before: Some(fixture::WORK)
            })
        );
        let stay = OverviewEffect::MoveStrip {
            strip: "Writing".into(),
            by: -1,
        };
        assert!(pick_of(&stay, &desk).is_err());
    }

    #[test]
    fn what_cannot_be_done_here_says_why() {
        let mut desk = fixture::desk();
        desk.renamable.clear();
        let rename = OverviewEffect::RenameStrip {
            strip: "Writing".into(),
            name: "Drafts".into(),
        };
        assert!(pick_of(&rename, &desk).is_err());
        let leave = OverviewEffect::LeaveWorkspace {
            workspace: "Research".into(),
        };
        assert!(
            pick_of(&leave, &desk).is_err(),
            "only a site's workspace is left here"
        );
        let moved = OverviewEffect::MoveWindow {
            window: "Week 44".into(),
            workspace: "Launch".into(),
        };
        assert!(pick_of(&moved, &desk).is_err(), "it is already there");
    }

    #[test]
    fn a_pick_the_person_made_is_the_effect_an_agent_would_invoke() {
        let desk = fixture::desk();
        for pick in [
            OverviewPick::Window(106),
            OverviewPick::Workspace(WorkspaceId(2)),
            OverviewPick::NewWorkspace,
            OverviewPick::JoinWorkspace,
            OverviewPick::CloseWindow(104),
            OverviewPick::MoveWindow {
                tab: 104,
                to: WorkspaceId(2),
            },
            OverviewPick::RenameStrip {
                strip: fixture::WORK,
                name: "Drafts".into(),
            },
            OverviewPick::MoveStrip {
                strip: fixture::WORK,
                before: None,
            },
            OverviewPick::MoveStrip {
                strip: fixture::READING,
                before: Some(fixture::WORK),
            },
        ] {
            let effect = effect_of(&pick, &desk).unwrap();
            assert_eq!(pick_of(&effect, &desk), Ok(pick));
        }
    }
}
