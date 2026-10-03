#![allow(
    clippy::doc_markdown,
    reason = "migration stack lint debt is scoped to this target"
)]

//! Native input-delivery fixture, not a displayed-FPS qualification workload.
//! The ordinary button callback supplies independent evidence that scheduled
//! clicks reached a real control. Run with TILO_NATIVE_SCENARIO and TILO_PROFILE_OUT.

fn main() {
    tilo_ui::i18n::I18nManager::init("en-US");
    if std::env::args().any(|arg| arg == "--scroll") {
        use std::{cell::Cell, rc::Rc};
        use tilo_ui::widget::{Column, Scrollable, Text};
        let offset = Rc::new(Cell::new((0.0_f32, 256.0_f32)));
        let widget_offset = offset.clone();
        let mut app = tilo_ui::App::from_factory(tilo_ui::State::new(), move || {
            let mut column = Column::new().with_intrinsic_height();
            for i in 0..200 {
                column = column.push(Box::new(Text::new(&format!(
                    "Row {i:>3} — the quick brown fox jumps over the lazy dog, then does it again"
                ))));
            }
            Box::new(
                Scrollable::new_vertical()
                    .with_persisted_offset(widget_offset)
                    .with_child(Box::new(column)),
            )
        });
        app.set_native_input_probe(move || {
            let (x, y) = offset.get();
            serde_json::json!({"offset_x": x, "offset_y": y})
        });
        app.run();
        return;
    }
    tilo_ui::App::from_factory(tilo_ui::State::new(), || {
        let mut activations = 0_u64;
        Box::new(
            tilo_ui::widget::Button::new("native-probe", "Native input probe", None).on_click(
                move || {
                    activations = activations.saturating_add(1);
                    eprintln!(
                        "tilo-native-effect {}",
                        serde_json::json!({
                            "event": "button_activated", "count": activations,
                        })
                    );
                },
            ),
        )
    })
    .run();
}
