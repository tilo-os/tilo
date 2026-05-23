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

impl DragDropManager {
    pub fn distance_moved(&self) -> f64 {
        if let Some(s) = &self.active {
            let dx = s.current.x - s.origin.x;
            let dy = s.current.y - s.origin.y;
            (dx * dx + dy * dy).sqrt()
        } else { 0.0 }
    }
}

/// Threshold in logical pixels after which a pointer press-and-move is promoted
/// to a drag gesture. Matches macOS / GTK conventions.
pub const DRAG_START_THRESHOLD_PX: f64 = 4.0;
