use vello::peniko::Color;

#[derive(Clone, Debug)]
pub struct Scrim { pub color: Color, pub opacity: f32, pub dismissable: bool }

impl Scrim {
    pub fn dark() -> Self { Self { color: Color::from_rgba8(0, 0, 0, 255), opacity: 0.5, dismissable: true } }
    pub fn transparent_click_catcher() -> Self { Self { color: Color::from_rgba8(0, 0, 0, 0), opacity: 0.0, dismissable: true } }
}
