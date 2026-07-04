use std::collections::HashMap;

#[derive(Clone, Debug)]
pub struct RemoteCursor {
    pub x: f32,
    pub y: f32,
    pub color: vello::peniko::Color,
    pub name: String,
}

pub struct CollaborationState {
    pub cursors: HashMap<String, RemoteCursor>,
}

impl CollaborationState {
    pub fn new() -> Self {
        Self {
            cursors: HashMap::new(),
        }
    }
}
