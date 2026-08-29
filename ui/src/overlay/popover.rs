use super::scrim::Scrim;
use vello::kurbo::Point;

#[derive(Clone, Debug)]
pub struct Popover {
    pub id_tag: String,
    pub anchor: Point,
    pub scrim: Scrim,
}

impl Popover {
    pub fn new(id_tag: &str, anchor: Point) -> Self {
        Self {
            id_tag: id_tag.into(),
            anchor,
            scrim: Scrim::transparent_click_catcher(),
        }
    }
}
