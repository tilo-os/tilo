use iced::widget::canvas::{
    self, Cache, Canvas, Geometry, Path, Program, Stroke, Text, Image,
};
use iced::mouse::{self, Cursor};
use iced::{Color, Element, Length, Point, Rectangle, Size, Theme, Font};
use crate::workspace::{WorkspaceState, Point as WorkspacePoint};
use uuid::Uuid;

#[derive(Debug, Clone)]
pub enum CanvasMessage {
    CursorMoved(Point, Option<u32>),
    ClickDeck(Uuid, Point, Option<u32>),
    MoveDeck(Uuid, WorkspacePoint),
    MoveMap(WorkspacePoint),
    SwitchTab(Uuid, usize),
    SelectDeck(Uuid),
    // Drag and Drop actions
    MergeDecks { source: Uuid, target: Uuid },
    MoveTab { source_deck: Uuid, tab_idx: usize, target_deck: Uuid },
    ExtractTab { source_deck: Uuid, tab_idx: usize, map_pos: WorkspacePoint },
}

pub struct WorkspaceCanvas {
    state: WorkspaceState,
    cache: Cache,
}

#[derive(Default)]
pub struct CanvasState {
    drag_mode: DragMode,
    last_cursor_pos: Option<Point>,
    drop_target: Option<Uuid>,
}

#[derive(Debug, Clone, PartialEq, Default)]
enum DragMode {
    #[default]
    None,
    PanningMap,
    DraggingDeck(Uuid),
    DraggingTab(Uuid, usize), // Deck ID, Tab Index
}

const TAB_BAR_HEIGHT: f32 = 40.0;
const OVERVIEW_SCALE: f32 = 0.4;

impl WorkspaceCanvas {
    pub fn new(state: WorkspaceState) -> Self {
        Self { state, cache: Cache::default() }
    }

    fn get_deck_rect(&self, deck: &crate::workspace::Deck, bounds_size: Size) -> Rectangle {
        let scale = if self.state.is_overview { OVERVIEW_SCALE } else { 1.0 };
        
        let (origin_x, origin_y) = if self.state.is_overview {
            let center_x = bounds_size.width / 2.0;
            let center_y = bounds_size.height / 2.0;
            (
                center_x + (deck.map_position.x + self.state.map_offset.x) * scale,
                center_y + (deck.map_position.y + self.state.map_offset.y) * scale
            )
        } else {
            (
                deck.map_position.x + self.state.map_offset.x,
                deck.map_position.y + self.state.map_offset.y
            )
        };

        Rectangle::new(
            Point::new(origin_x, origin_y),
            Size::new(deck.size.width * scale, deck.size.height * scale)
        )
    }
}

pub fn view(state: &WorkspaceState) -> Element<'_, CanvasMessage> {
    Canvas::new(WorkspaceCanvas::new(state.clone()))
        .width(Length::Fill)
        .height(Length::Fill)
        .into()
}

impl Program<CanvasMessage> for WorkspaceCanvas {
    type State = CanvasState;

    fn update(
        &self,
        state: &mut Self::State,
        event: canvas::Event,
        bounds: Rectangle,
        cursor: Cursor,
    ) -> (iced::widget::canvas::event::Status, Option<CanvasMessage>) {
        let cursor_pos = cursor.position_in(bounds);

        match event {
            canvas::Event::Mouse(mouse::Event::ButtonPressed(button)) => {
                if let Some(pos) = cursor_pos {
                    state.last_cursor_pos = Some(pos);
                    
                    if button == mouse::Button::Right {
                        state.drag_mode = DragMode::PanningMap;
                        return (iced::widget::canvas::event::Status::Captured, None);
                    }

                    if button == mouse::Button::Left {
                        for deck in self.state.decks.iter().rev() {
                            let rect = self.get_deck_rect(deck, bounds.size());
                            let full_rect = Rectangle::new(Point::new(rect.x, rect.y - TAB_BAR_HEIGHT), Size::new(rect.width, rect.height + TAB_BAR_HEIGHT));
                            
                            if full_rect.contains(pos) {
                                if self.state.is_overview {
                                    return (iced::widget::canvas::event::Status::Captured, Some(CanvasMessage::SelectDeck(deck.id)));
                                }

                                let local_pos = Point::new(pos.x - rect.x, pos.y - rect.y);
                                
                                // Tab Bar Interaction
                                if local_pos.y < 0.0 && local_pos.y > -TAB_BAR_HEIGHT {
                                    let tab_width = deck.size.width / deck.tabs.len().max(1) as f32;
                                    let tab_idx = (local_pos.x / tab_width) as usize;
                                    if tab_idx < deck.tabs.len() {
                                        // Start dragging individual tab
                                        state.drag_mode = DragMode::DraggingTab(deck.id, tab_idx);
                                        return (iced::widget::canvas::event::Status::Captured, Some(CanvasMessage::SwitchTab(deck.id, tab_idx)));
                                    }
                                }

                                // Header/Window Drag
                                state.drag_mode = DragMode::DraggingDeck(deck.id);
                                let surface_id = deck.tabs.get(deck.active_tab).and_then(|t| t.associated_surface_id);
                                return (iced::widget::canvas::event::Status::Captured, Some(CanvasMessage::ClickDeck(deck.id, local_pos, surface_id)));
                            }
                        }
                        state.drag_mode = DragMode::PanningMap;
                    }
                }
            }
            canvas::Event::Mouse(mouse::Event::ButtonReleased(mouse::Button::Left)) => {
                let mut message = None;
                if let Some(target_id) = state.drop_target {
                    match state.drag_mode {
                        DragMode::DraggingDeck(source_id) if source_id != target_id => {
                            message = Some(CanvasMessage::MergeDecks { source: source_id, target: target_id });
                        }
                        DragMode::DraggingTab(source_deck, tab_idx) => {
                            message = Some(CanvasMessage::MoveTab { source_deck, tab_idx, target_deck: target_id });
                        }
                        _ => {}
                    }
                } else if let DragMode::DraggingTab(source_deck, tab_idx) = state.drag_mode {
                    // Extract tab to new deck if dropped on empty space
                    if let Some(pos) = state.last_cursor_pos {
                        let map_pos = WorkspacePoint { x: pos.x - self.state.map_offset.x, y: pos.y - self.state.map_offset.y };
                        message = Some(CanvasMessage::ExtractTab { source_deck, tab_idx, map_pos });
                    }
                }
                
                state.drag_mode = DragMode::None;
                state.drop_target = None;
                if message.is_some() { return (iced::widget::canvas::event::Status::Captured, message); }
            }
            canvas::Event::Mouse(mouse::Event::CursorMoved { .. }) => {
                if let Some(pos) = cursor_pos {
                    let mut message = None;
                    if let Some(last_pos) = state.last_cursor_pos {
                        let delta = WorkspacePoint { x: pos.x - last_pos.x, y: pos.y - last_pos.y };
                        
                        match state.drag_mode {
                            DragMode::PanningMap => message = Some(CanvasMessage::MoveMap(delta)),
                            DragMode::DraggingDeck(id) => {
                                message = Some(CanvasMessage::MoveDeck(id, delta));
                                // Find potential merge target
                                state.drop_target = self.state.decks.iter()
                                    .find(|d| d.id != id && self.get_deck_rect(d, bounds.size()).contains(pos))
                                    .map(|d| d.id);
                            },
                            DragMode::DraggingTab(source_deck, _idx) => {
                                // Find potential drop target deck
                                state.drop_target = self.state.decks.iter()
                                    .find(|d| d.id != source_deck && self.get_deck_rect(d, bounds.size()).contains(pos))
                                    .map(|d| d.id);
                            }
                            DragMode::None => {
                                if !self.state.is_overview {
                                    for deck in self.state.decks.iter().rev() {
                                        let rect = self.get_deck_rect(deck, bounds.size());
                                        if rect.contains(pos) {
                                            let local_pos = Point::new(pos.x - rect.x, pos.y - rect.y);
                                            let surface_id = deck.tabs.get(deck.active_tab).and_then(|t| t.associated_surface_id);
                                            message = Some(CanvasMessage::CursorMoved(local_pos, surface_id));
                                            break;
                                        }
                                    }
                                }
                            }
                        }
                    }
                    state.last_cursor_pos = Some(pos);
                    if message.is_some() { return (iced::widget::canvas::event::Status::Captured, message); }
                }
            }
            _ => {}
        }

        (iced::widget::canvas::event::Status::Ignored, None)
    }

    fn draw(
        &self,
        state: &Self::State,
        renderer: &iced::Renderer,
        _theme: &Theme,
        bounds: Rectangle,
        _cursor: Cursor,
    ) -> Vec<Geometry> {
        let content = self.cache.draw(renderer, bounds.size(), |frame| {
            frame.fill_rectangle(Point::ORIGIN, bounds.size(), Color::from_rgb8(15, 15, 20));

            let scale = if self.state.is_overview { OVERVIEW_SCALE } else { 1.0 };

            // Draw Workspaces
            for ws in &self.state.workspaces {
                if ws.opacity <= 0.01 { continue; }
                let b = ws.current_bounds;
                let origin_x = if self.state.is_overview { bounds.size().width / 2.0 + (b.x + self.state.map_offset.x) * scale } else { b.x + self.state.map_offset.x };
                let origin_y = if self.state.is_overview { bounds.size().height / 2.0 + (b.y + self.state.map_offset.y) * scale } else { b.y + self.state.map_offset.y };

                let hull_rect = Rectangle::new(Point::new(origin_x, origin_y), Size::new(b.width * scale, b.height * scale));
                let path = Path::rounded_rectangle(hull_rect.position(), hull_rect.size(), (40.0 * scale).into());
                frame.fill(&path, Color::from_rgba(ws.color[0], ws.color[1], ws.color[2], 0.15 * ws.opacity));
                frame.stroke(&path, Stroke::default().with_color(Color::from_rgba(ws.color[0], ws.color[1], ws.color[2], 0.4 * ws.opacity)).with_width(2.0 * scale));
            }

            // Draw Decks
            for deck in &self.state.decks {
                let rect = self.get_deck_rect(deck, bounds.size());
                let is_drop_target = state.drop_target == Some(deck.id);
                
                // Merge Target Glow
                if is_drop_target {
                    frame.fill_rectangle(Point::new(rect.x - 10.0, rect.y - 10.0), Size::new(rect.width + 20.0, rect.height + 20.0), Color::from_rgba(0.4, 0.4, 1.0, 0.2));
                }

                // TAB BAR
                let tab_bar_rect = Rectangle::new(Point::new(rect.x, rect.y - TAB_BAR_HEIGHT * scale), Size::new(rect.width, TAB_BAR_HEIGHT * scale));
                frame.fill_rectangle(tab_bar_rect.position(), tab_bar_rect.size(), Color::from_rgb8(30, 30, 45));
                
                let tab_width = rect.width / deck.tabs.len().max(1) as f32;
                for (i, tab) in deck.tabs.iter().enumerate() {
                    let is_active = deck.active_tab == i;
                    let is_dragging_this_tab = state.drag_mode == DragMode::DraggingTab(deck.id, i);
                    
                    if is_dragging_this_tab { continue; } // Don't draw in original position

                    let tab_x = rect.x + i as f32 * tab_width;
                    let tab_rect = Rectangle::new(Point::new(tab_x, rect.y - TAB_BAR_HEIGHT * scale), Size::new(tab_width, TAB_BAR_HEIGHT * scale));
                    
                    if is_active {
                        frame.fill_rectangle(tab_rect.position(), tab_rect.size(), Color::from_rgb8(50, 50, 80));
                        frame.fill_rectangle(Point::new(tab_x, rect.y - 2.0 * scale), Size::new(tab_width, 2.0 * scale), Color::from_rgb(0.5, 0.5, 1.0));
                    }
                    
                    if scale > 0.3 {
                        frame.fill_text(Text {
                            content: tab.title.clone(),
                            position: Point::new(tab_x + 10.0 * scale, rect.y - (TAB_BAR_HEIGHT / 2.0 + 6.0) * scale),
                            color: if is_active { Color::WHITE } else { Color::from_rgb(0.6, 0.6, 0.6) },
                            size: (12.0 * scale).into(),
                            ..Default::default()
                        });
                    }
                }

                // Deck Content
                let is_focused_deck = self.state.focused_deck == Some(deck.id);
                if let Some(tab) = deck.tabs.get(deck.active_tab) {
                    if let (Some(pixels), size) = (&tab.buffer, tab.buffer_size) {
                        let draw_pos = Point::new(rect.x - tab.render_offset.x * scale, rect.y - tab.render_offset.y * scale);
                        frame.draw_image(Rectangle::new(draw_pos, Size::new(size.width * scale, size.height * scale)), Image::new(iced::widget::image::Handle::from_rgba(size.width as u32, size.height as u32, pixels.clone())));
                    } else {
                        frame.fill_rectangle(rect.position(), rect.size(), Color::from_rgb8(20, 20, 25));
                    }
                }

                let border_color = if is_focused_deck { Color::from_rgb(0.6, 0.6, 1.0) } else { Color::from_rgb(0.3, 0.3, 0.3) };
                frame.stroke(&Path::rectangle(rect.position(), rect.size()), Stroke::default().with_color(border_color).with_width(if is_focused_deck { 2.0 } else { 1.0 }));
            }

            // Draw Dragging Ghost Tab
            if let DragMode::DraggingTab(deck_id, idx) = state.drag_mode {
                if let Some(pos) = state.last_cursor_pos {
                    if let Some(deck) = self.state.decks.iter().find(|d| d.id == deck_id) {
                        if let Some(tab) = deck.tabs.get(idx) {
                            let ghost_rect = Rectangle::new(Point::new(pos.x - 50.0, pos.y - 15.0), Size::new(100.0, 30.0));
                            frame.fill_rectangle(ghost_rect.position(), ghost_rect.size(), Color::from_rgba(0.5, 0.5, 1.0, 0.6));
                            frame.fill_text(Text { content: tab.title.clone(), position: Point::new(ghost_rect.x + 5.0, ghost_rect.y + 10.0), color: Color::WHITE, size: 11.0.into(), ..Default::default() });
                        }
                    }
                }
            }
        });

        vec![content]
    }
}
