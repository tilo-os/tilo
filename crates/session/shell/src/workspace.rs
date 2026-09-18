use serde::{Deserialize, Serialize};
use uuid::Uuid;

#[derive(Debug, Clone, Copy, Serialize, Deserialize, Default, PartialEq)]
pub struct Point {
    pub x: f32,
    pub y: f32,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, Default, PartialEq)]
pub struct Size {
    pub width: f32,
    pub height: f32,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, Default, PartialEq)]
pub struct Rectangle {
    pub x: f32,
    pub y: f32,
    pub width: f32,
    pub height: f32,
}

impl Rectangle {
    #[must_use]
    pub fn expand(&self, padding: f32) -> Self {
        Self {
            x: self.x - padding,
            y: self.y - padding,
            width: self.width + padding * 2.0,
            height: self.height + padding * 2.0,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Tab {
    pub id: Uuid,
    pub title: String,
    pub command: String,
    pub status: TabStatus,
    pub associated_surface_id: Option<u32>,
    #[serde(skip)]
    pub render_offset: Point,
    #[serde(skip)]
    pub buffer: Option<Vec<u8>>,
    #[serde(skip)]
    pub buffer_size: Size,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum TabStatus {
    Loading,
    Running,
    Failed(String),
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Deck {
    pub id: Uuid,
    pub map_position: Point,
    pub size: Size,
    pub active_tab: usize,
    pub tabs: Vec<Tab>,
    pub workspace_id: Uuid,
    pub target_map_position: Point,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Workspace {
    pub id: Uuid,
    pub name: String,
    pub color: [f32; 3],
    pub current_bounds: Rectangle,
    pub target_bounds: Rectangle,
    pub opacity: f32,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct UserCursor {
    pub id: Uuid,
    pub name: String,
    pub position: Point,
    pub color: [f32; 3],
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WorkspaceState {
    pub decks: Vec<Deck>,
    pub workspaces: Vec<Workspace>,
    pub users: Vec<UserCursor>,
    pub map_offset: Point,
    pub focused_deck: Option<Uuid>,
    pub is_overview: bool,
    pub is_help_open: bool,
    pub is_sidebar_open: bool,
}

impl Default for WorkspaceState {
    fn default() -> Self {
        Self::new()
    }
}

impl WorkspaceState {
    pub fn new() -> Self {
        let ws_id = Uuid::new_v4();
        Self {
            decks: Vec::new(),
            workspaces: vec![Workspace {
                id: ws_id,
                name: "Main Workspace".into(),
                color: [0.3, 0.4, 0.9],
                current_bounds: Rectangle::default(),
                target_bounds: Rectangle::default(),
                opacity: 0.0,
            }],
            users: Vec::new(),
            map_offset: Point::default(),
            focused_deck: None,
            is_overview: false,
            is_help_open: false,
            is_sidebar_open: false,
        }
    }

    pub fn find_workspace(&self, id: Uuid) -> Option<&Workspace> {
        self.workspaces.iter().find(|w| w.id == id)
    }
}
