use quick_ui::{App, State, Theme};
use quick_ui::widget::{Button, Column, Toggle, Checkbox, Slider, TextInput, Widget};
use quick_ui::state::RemoteCursor;
use quick_ui::i18n::I18nManager;
use vello::peniko::Color;
use automerge::{AutoCommit, ObjId};
use vello::Scene;
use vello::kurbo::Affine;
use quick_ui::text_render::TextRenderer;

// A custom label widget that demonstrates i18n with arguments
struct Label {
    key: String,
    text: String,
    rect: vello::kurbo::Rect,
}

impl Label {
    fn new(key: &str) -> Self {
        Self {
            key: key.to_string(),
            text: String::new(),
            rect: vello::kurbo::Rect::ZERO,
        }
    }
}

impl Widget for Label {
    fn sync(&mut self, state: &AutoCommit, root: &ObjId) {
        use automerge::ReadDoc;
        if self.key == "current_theme" {
            if let Ok(Some((automerge::Value::Scalar(s), _))) = state.get(root, "current_theme_name") {
                self.text = s.to_string();
            }
        }
    }
    
    fn build_layout(&mut self, taffy: &mut taffy::TaffyTree<()>, _theme: &Theme) -> taffy::NodeId {
        taffy.new_leaf(taffy::Style {
            size: taffy::Size {
                width: taffy::Dimension::percent(1.0),
                height: taffy::Dimension::length(30.0),
            },
            ..Default::default()
        }).unwrap()
    }

    fn apply_layout(&mut self, taffy: &taffy::TaffyTree<()>, node: taffy::NodeId, origin: vello::kurbo::Point) {
        let layout = taffy.layout(node).unwrap();
        self.rect = vello::kurbo::Rect::new(
            origin.x as f64 + layout.location.x as f64,
            origin.y as f64 + layout.location.y as f64,
            origin.x as f64 + layout.location.x as f64 + layout.size.width as f64,
            origin.y as f64 + layout.location.y as f64 + layout.size.height as f64,
        );
    }

    fn draw(&mut self, scene: &mut Scene, transform: Affine, text_renderer: &mut TextRenderer, theme: &Theme) {
        let label = if self.key == "current_theme" {
            format!("Current Theme: {}", self.text)
        } else {
            quick_ui::i18n!(&self.key, "count" => 42)
        };
        
        text_renderer.draw_text(
            scene,
            &label,
            16.0,
            theme.text_primary,
            transform * Affine::translate((self.rect.x0, self.rect.y0 + 20.0))
        );
    }

    fn handle_event(&mut self, _event: &winit::event::WindowEvent, _scale_factor: f64, _state: &mut AutoCommit, _path: &ObjId, _handled: &mut bool) {}
}

fn main() {
    // 1. Initialize i18n
    I18nManager::init("en-US");
    I18nManager::add_resource("en-US", "btn_1 = Click Me\ncounter = Count is { $count }".to_string());
    I18nManager::add_resource("de-DE", "btn_1 = Klick Mich\ncounter = Zähler ist { $count }".to_string());

    let mut state = State::new();
    
    // 2. Initialize CRDT Document State
    {
        use automerge::transaction::Transactable;
        let _ = state.doc.put(&state.root, "toggle_1", true);
        let _ = state.doc.put(&state.root, "check_1", false);
        let _ = state.doc.put(&state.root, "slider_1", 0.75);
        let text_id = state.doc.put_object(&state.root, "text_1", automerge::ObjType::Text).unwrap();
        let _ = state.doc.splice_text(&text_id, 0, 0, "Hello Gallery!");
        let _ = state.doc.put(&state.root, "request_theme_cycle", false);
    }
    
    // Add a fake remote user cursor
    state.collab.cursors.insert("user_2".into(), RemoteCursor {
        x: 100.0,
        y: 100.0,
        color: Color::rgb8(255, 100, 100),
        name: "Alice".into()
    });

    // 3. Build the Widget Tree
    let root_widget = Box::new(
        Column::new()
            .push(Box::new(Button::new("btn_1", "btn_1", Some("counter")))) // Uses i18n key "btn_1"
            .push(Box::new(Label::new("counter")))
            .push(Box::new(Label::new("current_theme")))
            .push(Box::new(Button::new("btn_cycle", "Cycle Theme", Some("request_theme_cycle"))))
            .push(Box::new(Toggle::new("tog_1", "toggle_1")))
            .push(Box::new(Checkbox::new("chk_1", "check_1")))
            .push(Box::new(Slider::new("sld_1", "slider_1")))
            .push(Box::new(TextInput::new("txt_1", "text_1")))
    );
    
    // 4. Run the Application
    let app = App::new(state, root_widget);
    
    // Demonstrate switching language/theme (In a real app, this would be reactive)
    // app.theme = Theme::light();
    // I18nManager::set_locale("de-DE");

    app.run();
}
