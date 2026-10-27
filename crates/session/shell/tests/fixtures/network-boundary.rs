//! Compile-time native acceptance app. It uses the product shim and normal
//! App construction, with an explicit service pump in place of a GPU loop.
#[cfg(target_os = "linux")]
#[derive(Default)]
struct SdkObserver {
    content: ui::widget::Container,
}

#[cfg(target_os = "linux")]
impl ui::widget::Widget for SdkObserver {
    ui::provides!(dyn ui::AppWidget; inner = content);

    fn build_layout(
        &mut self,
        tree: &mut ui::taffy::TaffyTree<()>,
        theme: &ui::Theme,
        nodes: &mut std::collections::HashMap<ui::taffy::NodeId, *mut dyn ui::widget::Widget>,
    ) -> ui::taffy::NodeId {
        self.content.build_layout(tree, theme, nodes)
    }

    fn apply_layout(
        &mut self,
        tree: &ui::taffy::TaffyTree<()>,
        node: ui::taffy::NodeId,
        origin: ui::vello::kurbo::Point,
    ) {
        self.content.apply_layout(tree, node, origin);
    }

    fn draw(
        &mut self,
        scene: &mut ui::vello::Scene,
        transform: ui::vello::kurbo::Affine,
        text: &mut ui::text_render::TextRenderer,
        theme: &ui::Theme,
        presence: &ui::presence::PresenceCtx,
    ) {
        self.content.draw(scene, transform, text, theme, presence);
    }

    fn name(&self) -> &'static str {
        "Network SDK observer"
    }

    fn get_rect(&self) -> ui::vello::kurbo::Rect {
        self.content.get_rect()
    }
}

#[cfg(target_os = "linux")]
impl ui::AppWidget for SdkObserver {
    fn app_kind(&self) -> ui::AppKind {
        ui::AppKind::Settings
    }

    fn app_title(&self) -> &'static str {
        "Network SDK observer"
    }

    fn app_icon(&self) -> ui::IconDescriptor {
        ui::IconDescriptor::Glyph("N")
    }

    fn device_shell_operation(
        &mut self,
        operation: &ui::floating_surface::DeviceShellOperation,
    ) -> Result<serde_json::Value, String> {
        if !matches!(
            operation,
            ui::floating_surface::DeviceShellOperation::Inspect
        ) {
            return Err("The test observer supports only inspection".into());
        }
        // Read the public SDK after preceding Appearance frames have passed
        // through the real App pump. The inherited channel orders the reply.
        let status = tilo_net::status::current();
        Ok(serde_json::json!({
            "metered": status.metered,
            "online": status.online(),
        }))
    }
}

#[cfg(target_os = "linux")]
fn main() {
    let args = std::env::args().skip(1).collect::<Vec<_>>();
    if let Some((command, arguments)) = args.split_first()
        && command == "__sandbox-exec"
    {
        launcher::landlock_shim::run_sandbox_exec(arguments);
    }
    let mut app = ui::App::from_factory(ui::State::new(), || Box::new(SdkObserver::default()));
    // The test's sandbox bounds this loop's wall time.
    let mut last = None;
    loop {
        let connected = app.pump_native_host_for_test();
        let status = tilo_net::status::current();
        let observation = (connected, status);
        if last != Some(observation) {
            println!(
                "SDK {}",
                serde_json::json!({
                    "connected": connected,
                    "metered": status.metered,
                    "online": status.online(),
                })
            );
            last = Some(observation);
        }
        if !connected {
            // Loss is terminal for this launch. A second pump must stay local.
            assert!(!app.pump_native_host_for_test());
            assert_ne!(tilo_net::status::current().metered, Some(false));
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(5));
    }
}

#[cfg(not(target_os = "linux"))]
fn main() {
    panic!("the sandbox network boundary fixture requires Linux");
}
