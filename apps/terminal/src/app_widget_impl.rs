use crate::TerminalWidget;
use ui::{AppKind, AppWidget, IconDescriptor};

impl AppWidget for TerminalWidget {
    fn app_kind(&self) -> AppKind {
        AppKind::Terminal
    }

    fn app_title(&self) -> &str {
        // Reflect an OSC 0/2 window title when the running program set one,
        // falling back to "Terminal". `title()` returns the session-recorded
        // value so a collaborator sees the same title.
        self.title()
    }

    fn app_icon(&self) -> IconDescriptor {
        // Nerd Font terminal glyph — matches the icon used by the iced
        // file-explorer's "Open Terminal" button for visual consistency.
        IconDescriptor::Glyph("󰆍")
    }

    fn permission_hint(&self) -> Option<AppKind> {
        Some(AppKind::Terminal)
    }

    fn device_shell_operation(
        &mut self,
        operation: &ui::floating_surface::DeviceShellOperation,
    ) -> Result<serde_json::Value, String> {
        TerminalWidget::device_shell_operation(self, operation)
    }

    fn request_device_shell(&mut self) -> bool {
        TerminalWidget::request_device_shell(self)
    }

    fn take_host_intent(&mut self) -> Option<ui::widget::AppHostIntent> {
        self.take_session_intent()
    }
}
