use crate::TerminalWidget;
use ui::{AppKind, AppWidget, IconDescriptor};

impl AppWidget for TerminalWidget {
    fn app_kind(&self) -> AppKind {
        AppKind::Terminal
    }

    fn app_title(&self) -> &str {
        self.title()
    }

    fn app_icon(&self) -> IconDescriptor {
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
