//! The terminal as a host mounts it: the [`TerminalWidget`] inside the
//! toolkit's [`FindScope`].
//!
//! The terminal itself paints the grid and answers [`ui::widget::Findable`];
//! the scope owns the find bar, as a band over the terminal, and its keys:
//! Shift+Cmd+F (Ctrl+Shift+F) opens it, since a plain Ctrl+F belongs to the
//! program the terminal runs. A host (the shell's tile, the standalone
//! window, the code editor's panel) holds a [`TerminalView`] and reaches the
//! terminal through it.

use std::ops::{Deref, DerefMut};

use ui::platform::Shortcut;
use ui::widget::{AppModel, EventCtx, FindScope, ModelPath, Widget};
use ui::{AppKind, AppWidget, IconDescriptor};
use winit::event::WindowEvent;

use crate::{CommandBuilder, HostedProcessServices, HostedTerminalError, TerminalWidget};

/// A terminal with find in its scrollback. Dereferences to the
/// [`TerminalWidget`] it hosts.
pub struct TerminalView {
    scope: FindScope,
}

impl TerminalView {
    /// Host `terminal` in a find scope.
    #[must_use]
    pub fn new(terminal: TerminalWidget) -> Self {
        Self {
            scope: FindScope::new(Box::new(terminal)).with_shortcut(Shortcut::command("F").shift()),
        }
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

    /// [`TerminalWidget::spawn_hosted`], hosted in a find scope.
    ///
    /// # Errors
    /// As [`TerminalWidget::spawn_hosted`].
    pub fn spawn_hosted(services: HostedProcessServices) -> Result<Self, HostedTerminalError> {
        TerminalWidget::spawn_hosted(services).map(Self::new)
    }

    /// The terminal the scope hosts.
    ///
    /// # Panics
    /// Never: the scope's content is the terminal this view was built on.
    #[must_use]
    pub fn terminal(&self) -> &TerminalWidget {
        #[allow(
            clippy::expect_used,
            reason = "the scope's content is the TerminalWidget `new` boxed; nothing replaces it"
        )]
        self.scope
            .content()
            .as_any()
            .and_then(|any| any.downcast_ref())
            .expect("a terminal view hosts a terminal")
    }

    /// The terminal the scope hosts, mutably.
    ///
    /// # Panics
    /// Never: see [`Self::terminal`].
    pub fn terminal_mut(&mut self) -> &mut TerminalWidget {
        #[allow(
            clippy::expect_used,
            reason = "the scope's content is the TerminalWidget `new` boxed; nothing replaces it"
        )]
        self.scope
            .content_mut()
            .as_any_mut()
            .and_then(|any| any.downcast_mut())
            .expect("a terminal view hosts a terminal")
    }

    /// Open the find bar and focus its query. This does not publish the
    /// query, the result positions, or a command to collaborators or agents:
    /// scrollback may contain credentials and find is only a viewport aid.
    pub fn open_find(&mut self) {
        self.scope.open();
    }

    /// Find `query` in the scrollback, as typing it into the bar would.
    pub fn set_find_query(&mut self, query: &str) {
        self.scope.find(query);
    }

    /// Close the find bar and stop painting matches.
    pub fn close_find(&mut self) {
        self.scope.close();
    }

    /// Whether the find bar is open.
    #[must_use]
    pub fn find_is_open(&self) -> bool {
        self.scope.is_open()
    }

    /// The find bar's query and its `(current, total)` counter while it is
    /// open.
    #[must_use]
    pub fn find_counter(&self) -> Option<(String, Option<(usize, usize)>)> {
        self.scope.is_open().then(|| {
            let bar = self.scope.bar();
            (bar.query().to_owned(), bar.position())
        })
    }

    /// The find band's rect while the bar is open and laid out.
    #[must_use]
    pub fn find_band(&self) -> Option<vello::kurbo::Rect> {
        self.scope
            .is_open()
            .then(|| Widget::get_rect(self.scope.bar()))
    }

    /// The context menu's Find entry asks the scope for its bar.
    fn serve_find_request(&mut self) {
        if self.terminal_mut().take_find_request() {
            self.scope.open();
        }
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
    type Inner = FindScope;

    fn inner(&self) -> &FindScope {
        &self.scope
    }

    fn inner_mut(&mut self) -> &mut FindScope {
        &mut self.scope
    }

    fn handle_event(&mut self, event: &WindowEvent, ctx: &mut EventCtx<'_>) {
        self.scope.handle_event(event, ctx);
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
            .scope
            .perform_semantic_action(target, action, state, path);
        self.serve_find_request();
        handled
    }

    fn sync(&mut self, state: &AppModel, path: &ModelPath) {
        self.serve_find_request();
        self.scope.sync(state, path);
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
        self.scope.build_layout(taffy, theme, node_map)
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
