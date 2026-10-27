//! The terminal as a host mounts it: the [`TerminalWidget`] with the keys
//! that open the window bar's search on its scrollback.
//!
//! The terminal itself paints the grid and answers [`ui::widget::Findable`];
//! the window bar's search finds in it. Shift+Cmd+F (Ctrl+Shift+F) and the
//! context menu's Find open that search, since a plain Ctrl+F belongs to the
//! program the terminal runs. A host (the shell's tile, the standalone
//! window, the code editor's panel) holds a [`TerminalView`] and reaches the
//! terminal through it.

use std::ops::{Deref, DerefMut};

use ui::widget::{AppModel, EventCtx, ModelPath, Widget};
use ui::window_toolbar::SearchOpening;
use ui::{AppKind, AppWidget, IconDescriptor};
use winit::event::WindowEvent;

use crate::{CommandBuilder, HostedProcessServices, HostedTerminalError, TerminalWidget};

/// A terminal whose scrollback the window bar's search finds in.
/// Dereferences to the [`TerminalWidget`] it hosts.
pub struct TerminalView {
    terminal: TerminalWidget,
}

impl TerminalView {
    /// Host `terminal`.
    #[must_use]
    pub fn new(terminal: TerminalWidget) -> Self {
        Self { terminal }
    }

    /// [`TerminalWidget::spawn_default`], hosted.
    ///
    /// # Errors
    /// As [`TerminalWidget::spawn_default`].
    pub fn spawn_default() -> anyhow::Result<Self> {
        TerminalWidget::spawn_default().map(Self::new)
    }

    /// [`TerminalWidget::spawn_headless`], hosted.
    ///
    /// # Errors
    /// As [`TerminalWidget::spawn_headless`].
    pub fn spawn_headless() -> anyhow::Result<Self> {
        TerminalWidget::spawn_headless().map(Self::new)
    }

    /// [`TerminalWidget::spawn_command`], hosted.
    ///
    /// # Errors
    /// As [`TerminalWidget::spawn_command`].
    pub fn spawn_command(command: CommandBuilder) -> anyhow::Result<Self> {
        TerminalWidget::spawn_command(command).map(Self::new)
    }

    /// [`TerminalWidget::spawn_hosted`], hosted.
    ///
    /// # Errors
    /// As [`TerminalWidget::spawn_hosted`].
    pub fn spawn_hosted(services: HostedProcessServices) -> Result<Self, HostedTerminalError> {
        TerminalWidget::spawn_hosted(services).map(Self::new)
    }

    /// The terminal this view hosts.
    #[must_use]
    pub fn terminal(&self) -> &TerminalWidget {
        &self.terminal
    }

    /// The terminal this view hosts, mutably.
    pub fn terminal_mut(&mut self) -> &mut TerminalWidget {
        &mut self.terminal
    }

    /// Open the window bar's search on the scrollback. This does not
    /// publish the query, the result positions, or a command to
    /// collaborators or agents: scrollback may contain credentials and find
    /// is only a viewport aid.
    pub fn open_find() {
        ui::window_toolbar::open_search(SearchOpening::Find);
    }

    /// Show the scrollback's matches of `query` as the window bar's search
    /// does, the first revealed: a capture's staged find.
    pub fn set_find_query(&mut self, query: &str) {
        let options = ui::widget::FindOptions::default();
        if !ui::widget::Findable::find(&mut self.terminal, query, options).is_empty() {
            ui::widget::Findable::reveal(&mut self.terminal, 0);
        }
    }

    /// The context menu's Find entry opens the window bar's search.
    fn serve_find_request(&mut self) {
        if self.terminal.take_find_request() {
            Self::open_find();
        }
    }

    /// Whether `event` is the terminal's Find chord: Shift+Cmd+F
    /// (Ctrl+Shift+F), which no program the terminal runs receives.
    fn is_find_chord(&self, event: &WindowEvent, ctx: &EventCtx<'_>) -> bool {
        let WindowEvent::KeyboardInput {
            event:
                winit::event::KeyEvent {
                    logical_key: winit::keyboard::Key::Character(c),
                    state: winit::event::ElementState::Pressed,
                    ..
                },
            ..
        } = event
        else {
            return false;
        };
        let state = ctx.modifiers.state();
        c.eq_ignore_ascii_case("f")
            && self.terminal.conventions().command_modifier_down(state)
            && state.shift_key()
            && !state.alt_key()
    }
}

impl Deref for TerminalView {
    type Target = TerminalWidget;

    fn deref(&self) -> &TerminalWidget {
        self.terminal()
    }
}

impl DerefMut for TerminalView {
    fn deref_mut(&mut self) -> &mut TerminalWidget {
        self.terminal_mut()
    }
}

impl ui::widget::Decorator for TerminalView {
    type Inner = TerminalWidget;

    fn inner(&self) -> &TerminalWidget {
        &self.terminal
    }

    fn inner_mut(&mut self) -> &mut TerminalWidget {
        &mut self.terminal
    }

    fn handle_event(&mut self, event: &WindowEvent, ctx: &mut EventCtx<'_>) {
        if self.is_find_chord(event, ctx) {
            Self::open_find();
            *ctx.handled = true;
            return;
        }
        self.terminal.handle_event(event, ctx);
        self.serve_find_request();
    }

    fn perform_semantic_action(
        &mut self,
        target: &str,
        action: &ui::a11y::Action,
        state: &mut ui::widget::Edits<'_>,
        path: &ModelPath,
    ) -> bool {
        let handled = self
            .terminal
            .perform_semantic_action(target, action, state, path);
        self.serve_find_request();
        handled
    }

    fn sync(&mut self, state: &AppModel, path: &ModelPath) {
        self.serve_find_request();
        self.terminal.sync(state, path);
    }

    fn build_layout(
        &mut self,
        taffy: &mut taffy::TaffyTree<()>,
        theme: &ui::Theme,
        node_map: &mut std::collections::HashMap<taffy::NodeId, *mut dyn Widget>,
    ) -> taffy::NodeId {
        // A request made since the last sync (the menu's Find row, chosen
        // through assistive technology) is in this layout already.
        self.serve_find_request();
        self.terminal.build_layout(taffy, theme, node_map)
    }
}

impl AppWidget for TerminalView {
    fn device_shell_operation(
        &mut self,
        operation: &ui::floating_surface::DeviceShellOperation,
    ) -> Result<serde_json::Value, String> {
        TerminalWidget::device_shell_operation(self.terminal_mut(), operation)
    }

    fn request_device_shell(&mut self) -> bool {
        TerminalWidget::request_device_shell(self.terminal_mut())
    }

    fn app_kind(&self) -> AppKind {
        self.terminal().app_kind()
    }

    fn app_title(&self) -> &str {
        self.terminal().app_title()
    }

    fn app_icon(&self) -> IconDescriptor {
        self.terminal().app_icon()
    }

    fn permission_hint(&self) -> Option<AppKind> {
        self.terminal().permission_hint()
    }

    fn take_host_intent(&mut self) -> Option<ui::widget::AppHostIntent> {
        self.terminal_mut().take_host_intent()
    }
}
