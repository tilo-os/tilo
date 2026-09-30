use std::collections::HashMap;
use ui_core::widget::{AppModel, ModelPath};

use tilo_app as ui;
use ui::control_custody::{CustodyAttachment, RawControlEmission, RawControlSlot};
use ui::taffy::{NodeId, TaffyTree};
use ui::tilo_actions::{ActionAddress, ControlInvocation};
use ui::vello::{
    Scene,
    kurbo::{Affine, Point},
};
use ui::widget::{Text, Widget};
use ui::winit::event::WindowEvent;
use ui::{PresenceCtx, TextRenderer, Theme};

struct ExternalRawWidget {
    slot: RawControlSlot,
    content: Text,
}

impl ExternalRawWidget {
    fn new() -> Self {
        Self {
            slot: RawControlSlot::replace_one(),
            content: Text::new("External facade fixture"),
        }
    }
}

impl Widget for ExternalRawWidget {
    ui_core::provides!(dyn ui_core::widget::controls::ActionControls);

    fn sync(&mut self, state: &mut AppModel, path: &ModelPath) {
        self.content.sync(state, path);
    }

    fn build_layout(
        &mut self,
        taffy: &mut TaffyTree<()>,
        theme: &Theme,
        node_map: &mut HashMap<NodeId, *mut dyn Widget>,
    ) -> NodeId {
        self.content.build_layout(taffy, theme, node_map)
    }

    fn apply_layout(&mut self, taffy: &TaffyTree<()>, node: NodeId, origin: Point) {
        self.content.apply_layout(taffy, node, origin);
    }

    fn draw(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        text_renderer: &mut TextRenderer,
        theme: &Theme,
        presence: &PresenceCtx,
    ) {
        self.content
            .draw(scene, transform, text_renderer, theme, presence);
    }

    fn handle_event(&mut self, event: &WindowEvent, ctx: &mut ui_core::widget::EventCtx<'_>) {
        self.content.handle_event(event, ctx);
    }

    fn name(&self) -> &'static str {
        "ExternalRawWidget"
    }

    fn get_rect(&self) -> ui::kurbo::Rect {
        self.content.get_rect()
    }
}

impl ui_core::widget::controls::ActionControls for ExternalRawWidget {
    fn install_control_custody(
        &mut self,
        domain: &ui::ControlDomain,
        attachment: CustodyAttachment,
    ) {
        let _ = self.slot.install_custody(domain, attachment);
        ui_core::widget::controls::install_control_custody(&mut self.content, domain, attachment);
    }
}

fn invocation(resource: &str) -> ControlInvocation {
    ControlInvocation {
        address: ActionAddress::new(resource, "apply"),
        arguments: serde_json::Map::default(),
    }
}

#[test]
fn public_facade_exposes_raw_holder_and_widget_custody_override() {
    fn assert_widget<T: Widget>() {}
    let mut widget = ExternalRawWidget::new();
    let expected = invocation("resource-b");
    assert_eq!(
        widget.slot.emit(invocation("resource-a")),
        Ok(RawControlEmission::LegacyPending)
    );
    assert_eq!(
        widget.slot.emit(expected.clone()),
        Ok(RawControlEmission::LegacyPending)
    );
    assert_eq!(widget.slot.take_legacy_compatibility(), Some(expected));
    assert_eq!(widget.slot.take_legacy_compatibility(), None);

    assert_widget::<ExternalRawWidget>();
}
