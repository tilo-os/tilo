//! The overview on screen: the desktop's overview widget, following the
//! shell that hosts it, and carrying out what the person and anyone acting
//! for them chose.

use std::rc::Rc;

use tilo_desktop::overview_panel::{OverviewDesk, OverviewPanel, OverviewPick};
use tilo_sheets::{Phase, Seat as _};
use ui::framework::{EffectAnswer, EffectCx, Effected, ResourceSurface, SurfaceCx};
use ui::presence::PresenceCtx;
use ui::text_render::TextRenderer;
use ui::theme::Theme;
use ui::vello::Scene;
use ui::vello::kurbo::Affine;
use ui::widget::{AppModel, Decorator, EventCtx, ModelPath, Widget};
use ui::winit::event::WindowEvent;

use crate::seat::OverviewSeat;
use crate::{Overview, OverviewEffect, fixture};

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
        }
    }

    /// The overview widget.
    pub fn panel(&self) -> &OverviewPanel {
        &self.panel
    }

    pub fn panel_mut(&mut self) -> &mut OverviewPanel {
        &mut self.panel
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

    /// Carry out what the person chose: their press or key is their
    /// decision, and the desktop's own verbs carry it out.
    fn settle(&mut self) {
        while let Some(pick) = self.panel.take_pick() {
            self.carry_out(pick);
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
                if let Some(target) = self.panel.workspace_rect(1) {
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
        }
        Ok(Effected::done())
    }
}
