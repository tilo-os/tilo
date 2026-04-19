use vello::kurbo::Point;

#[derive(Clone, Debug)]
pub enum DragPayload {
    Tab { group: u64, tab: u64 },
    File { path: String },
    Text { value: String },
    Custom { kind: String, blob: Vec<u8> },
}

#[derive(Clone, Debug)]
pub struct DragState {
    pub payload: DragPayload,
    pub origin: Point,
    pub current: Point,
}

#[derive(Default)]
pub struct DragDropManager {
    pub active: Option<DragState>,
}

impl DragDropManager {
    pub fn new() -> Self { Self::default() }
    pub fn start(&mut self, payload: DragPayload, origin: Point) {
        self.active = Some(DragState { payload, origin, current: origin });
    }
    pub fn update_position(&mut self, p: Point) {
        if let Some(s) = &mut self.active { s.current = p; }
    }
    pub fn end(&mut self) -> Option<DragState> { self.active.take() }
    pub fn is_active(&self) -> bool { self.active.is_some() }
}
