use iced::widget::{button, container, row, column, text, text_input, Space, stack, scrollable};
use iced::{Alignment, Element, Length, Theme, keyboard, Subscription, Event as IcedEvent, Color};
use iced::futures::SinkExt;
use tokio::sync::mpsc;
use once_cell::sync::Lazy;
use std::sync::Mutex;
use std::time::{Duration, Instant};

mod config;
mod security;
pub mod network;
pub mod ui;
mod workspace;

use ui::canvas::CanvasMessage;
use workspace::{WorkspaceState, Point as WorkspacePoint, Deck, Tab, Size as WorkspaceSize, UserCursor, TabStatus, Workspace, Rectangle as WorkspaceRect};
use security::container::Container;
use network::wayland::{WaylandServer, WaylandEvent, ClientState, WaylandInput};
use network::p2p::{P2PClient, NetworkEvent};
use uuid::Uuid;
use std::sync::Arc;

static P2P_CHANNEL: Lazy<(mpsc::UnboundedSender<NetworkEvent>, Mutex<Option<mpsc::UnboundedReceiver<NetworkEvent>>>)> = Lazy::new(|| {
    let (tx, rx) = mpsc::unbounded_channel();
    (tx, Mutex::new(Some(rx)))
});

static WAYLAND_INPUT_CHANNEL: Lazy<(mpsc::UnboundedSender<WaylandInput>, Mutex<Option<mpsc::UnboundedReceiver<WaylandInput>>>)> = Lazy::new(|| {
    let (tx, rx) = mpsc::unbounded_channel();
    (tx, Mutex::new(Some(rx)))
});

pub fn main() -> iced::Result {
    tracing_subscriber::fmt::init();
    iced::application("Quick Spatial Desktop", SpatialDesktop::update, SpatialDesktop::view)
        .theme(|_| Theme::Dark)
        .subscription(SpatialDesktop::subscription)
        .run()
}

struct SpatialDesktop {
    workspace_state: WorkspaceState,
    runner_input: String,
    runner_mode: RunnerMode,
    inner_wayland_socket: Option<String>,
    my_id: Uuid,
    spawn_times: std::collections::HashMap<Uuid, Instant>,
    current_time: String,
}

#[derive(Debug, Clone, PartialEq)]
enum RunnerMode { Hidden, LaunchApp, NewTab(Uuid) }

#[derive(Debug, Clone)]
enum Message {
    Canvas(CanvasMessage),
    InputChanged(String),
    SubmitCommand,
    ToggleOverview,
    ToggleSidebar,
    ToggleHelp,
    NewTabShortcut,
    NewDeckShortcut,
    CloseTabShortcut,
    Wayland(WaylandEvent),
    WaylandServerStarted(String),
    Network(NetworkEvent),
    Tick(Instant),
    IcedEvent(IcedEvent),
}

const SNAP_THRESHOLD: f32 = 40.0;
const PULL_AWAY_THRESHOLD: f32 = 100.0;
const GAP: f32 = 4.0;
const LERP_FACTOR: f32 = 0.2;
const TAB_BAR_HEIGHT: f32 = 40.0; 

impl SpatialDesktop {
    fn new() -> Self {
        Self {
            workspace_state: WorkspaceState::new(),
            runner_input: String::new(),
            runner_mode: RunnerMode::Hidden,
            inner_wayland_socket: None,
            my_id: Uuid::new_v4(),
            spawn_times: std::collections::HashMap::new(),
            current_time: String::new(),
        }
    }

    fn subscription(&self) -> Subscription<Message> {
        let wayland_sub = Subscription::run(|| {
            iced::stream::channel(100, |mut output| async move {
                let (tx, mut rx) = mpsc::unbounded_channel();
                let (started_tx, mut started_rx) = tokio::sync::mpsc::channel(1);
                let mut input_rx = WAYLAND_INPUT_CHANNEL.1.lock().unwrap().take().expect("Wayland Input Receiver already taken");
                std::thread::spawn(move || {
                    use smithay::reexports::wayland_server::{Display, ListeningSocket};
                    use smithay::reexports::calloop::{EventLoop, Interest, Mode as CalloopMode};
                    use smithay::reexports::calloop::generic::Generic;
                    let mut event_loop = EventLoop::<WaylandServer>::try_new().unwrap();
                    let mut display = Display::<WaylandServer>::new().unwrap();
                    let socket = ListeningSocket::bind_auto("wayland", 0..32).unwrap();
                    let env_name = socket.socket_name().unwrap().to_string_lossy().into_owned();
                    let handle = display.handle();
                    event_loop.handle().insert_source(Generic::new(socket, Interest::READ, CalloopMode::Level), move |_, socket, _state| {
                        if let Some(stream) = socket.accept().expect("Failed to accept") { let mut h = handle.clone(); let _ = h.insert_client(stream, Arc::new(ClientState { compositor_state: smithay::wayland::compositor::CompositorClientState::default() })); }
                        Ok(smithay::reexports::calloop::PostAction::Continue)
                    }).expect("Failed to insert socket into loop");
                    let _ = started_tx.blocking_send(env_name);
                    let mut server_state = WaylandServer::new(&display.handle(), tx);
                    loop {
                        while let Ok(input) = input_rx.try_recv() { server_state.handle_input(input); }
                        let _ = display.dispatch_clients(&mut server_state);
                        let _ = display.flush_clients();
                        event_loop.dispatch(std::time::Duration::from_millis(16), &mut server_state).unwrap();
                    }
                });
                if let Some(socket_name) = started_rx.recv().await { let _ = output.send(Message::WaylandServerStarted(socket_name)).await; }
                while let Some(event) = rx.recv().await { let _ = output.send(Message::Wayland(event)).await; }
            })
        });
        let event_sub = iced::event::listen().map(Message::IcedEvent);
        let tick_sub = iced::time::every(Duration::from_millis(16)).map(Message::Tick);
        let p2p_sub = Subscription::run(|| {
            iced::stream::channel(100, |mut output| async move {
                let mut p2p_rx = P2P_CHANNEL.1.lock().unwrap().take().expect("P2P Receiver already taken");
                let mut client = P2PClient::new().await.expect("Failed to start P2P");
                loop {
                    tokio::select! {
                        Some(event) = client.next_event() => { let _ = output.send(Message::Network(event)).await; }
                        Some(broadcast_event) = p2p_rx.recv() => { client.broadcast(broadcast_event); }
                    }
                }
            })
        });
        Subscription::batch(vec![wayland_sub, p2p_sub, event_sub, tick_sub])
    }

    fn update(&mut self, message: Message) -> iced::Task<Message> {
        match message {
            Message::IcedEvent(event) => {
                if let IcedEvent::Keyboard(keyboard::Event::KeyPressed { key, modifiers, .. }) = event {
                    if modifiers.alt() || modifiers.command() || modifiers.control() {
                        match key {
                            keyboard::Key::Named(keyboard::key::Named::Enter) => return iced::Task::done(Message::NewDeckShortcut),
                            keyboard::Key::Named(keyboard::key::Named::Backspace) => {
                                if let Some(did) = self.workspace_state.focused_deck {
                                    self.workspace_state.decks.retain(|d| d.id != did);
                                    self.workspace_state.focused_deck = None;
                                }
                            },
                            keyboard::Key::Character(c) if c.to_lowercase() == "t" => return iced::Task::done(Message::NewTabShortcut),
                            keyboard::Key::Character(c) if c.to_lowercase() == "d" => return iced::Task::done(Message::NewDeckShortcut),
                            keyboard::Key::Character(c) if c.to_lowercase() == "w" => return iced::Task::done(Message::CloseTabShortcut),
                            keyboard::Key::Character(c) if c.to_lowercase() == "s" => return iced::Task::done(Message::ToggleOverview),
                            keyboard::Key::Character(c) if c.to_lowercase() == "h" => return iced::Task::done(Message::ToggleHelp),
                            keyboard::Key::Character(c) if c.to_lowercase() == "i" => return iced::Task::done(Message::ToggleSidebar),
                            _ => {}
                        }
                    }
                }
            }
            Message::Tick(_) => {
                self.current_time = chrono::Local::now().format("%H:%M:%S").to_string();
                
                let mut ws_bounds = std::collections::HashMap::new();
                for deck in &mut self.workspace_state.decks {
                    deck.map_position.x += (deck.target_map_position.x - deck.map_position.x) * LERP_FACTOR;
                    deck.map_position.y += (deck.target_map_position.y - deck.map_position.y) * LERP_FACTOR;
                    
                    let deck_top = deck.map_position.y - TAB_BAR_HEIGHT;
                    let deck_height = deck.size.height + TAB_BAR_HEIGHT;

                    let bounds = ws_bounds.entry(deck.workspace_id).or_insert(WorkspaceRect {
                        x: deck.map_position.x,
                        y: deck_top,
                        width: deck.size.width,
                        height: deck_height,
                    });
                    
                    let min_x = bounds.x.min(deck.map_position.x);
                    let min_y = bounds.y.min(deck_top);
                    let max_x = (bounds.x + bounds.width).max(deck.map_position.x + deck.size.width);
                    let max_y = (bounds.y + bounds.height).max(deck_top + deck_height);
                    
                    bounds.x = min_x; bounds.y = min_y;
                    bounds.width = max_x - min_x; bounds.height = max_y - min_y;
                }

                for ws in &mut self.workspace_state.workspaces {
                    if let Some(target) = ws_bounds.get(&ws.id) {
                        ws.target_bounds = target.expand(40.0);
                        ws.opacity += (1.0 - ws.opacity) * LERP_FACTOR;
                    } else {
                        ws.target_bounds = WorkspaceRect { 
                            x: ws.current_bounds.x + ws.current_bounds.width/2.0, 
                            y: ws.current_bounds.y + ws.current_bounds.height/2.0, 
                            width: 0.0, height: 0.0 
                        };
                        ws.opacity += (0.0 - ws.opacity) * LERP_FACTOR;
                    }
                    ws.current_bounds.x += (ws.target_bounds.x - ws.current_bounds.x) * LERP_FACTOR;
                    ws.current_bounds.y += (ws.target_bounds.y - ws.current_bounds.y) * LERP_FACTOR;
                    ws.current_bounds.width += (ws.target_bounds.width - ws.current_bounds.width) * LERP_FACTOR;
                    ws.current_bounds.height += (ws.target_bounds.height - ws.current_bounds.height) * LERP_FACTOR;
                }
                self.workspace_state.workspaces.retain(|ws| ws.opacity > 0.01 || ws_bounds.contains_key(&ws.id));
            }
            Message::ToggleOverview => { self.workspace_state.is_overview = !self.workspace_state.is_overview; }
            Message::ToggleSidebar => { self.workspace_state.is_sidebar_open = !self.workspace_state.is_sidebar_open; }
            Message::ToggleHelp => { self.workspace_state.is_help_open = !self.workspace_state.is_help_open; }
            Message::WaylandServerStarted(name) => { self.inner_wayland_socket = Some(name); }
            Message::Network(event) => {
                match event {
                    NetworkEvent::CursorUpdate(cursor) => {
                        if cursor.id == self.my_id { return iced::Task::none(); }
                        if let Some(existing) = self.workspace_state.users.iter_mut().find(|u| u.id == cursor.id) { existing.position = cursor.position; }
                        else { self.workspace_state.users.push(cursor); }
                    }
                    _ => {}
                }
            }
            Message::Wayland(event) => {
                match event {
                    WaylandEvent::Frame { surface_id, app_id, pixels, width, height } => {
                        for deck in &mut self.workspace_state.decks {
                            let mut found_match_idx = None;
                            for (idx, tab) in deck.tabs.iter_mut().enumerate() {
                                if tab.associated_surface_id == Some(surface_id) || (tab.associated_surface_id.is_none() && tab.status == TabStatus::Loading && (app_id == "unknown" || tab.command.contains(&app_id.to_lowercase()))) {
                                    tab.associated_surface_id = Some(surface_id);
                                    tab.buffer = Some(pixels.clone());
                                    tab.buffer_size = WorkspaceSize { width: width as f32, height: height as f32 };
                                    tab.status = TabStatus::Running;
                                    if tab.title == tab.command && !app_id.is_empty() && app_id != "unknown" { tab.title = app_id.clone(); }
                                    found_match_idx = Some(idx);
                                }
                            }
                            if let Some(idx) = found_match_idx {
                                if idx == deck.active_tab || deck.tabs[deck.active_tab].buffer.is_none() {
                                    if width > 200 && height > 200 { 
                                        deck.size = WorkspaceSize { width: width as f32, height: height as f32 }; 
                                        deck.active_tab = idx;
                                    }
                                }
                            }
                        }
                    }
                    _ => {}
                }
            }
            Message::Canvas(msg) => {
                match msg {
                    CanvasMessage::SelectDeck(id) => {
                        if let Some(deck) = self.workspace_state.decks.iter().find(|d| d.id == id) {
                            self.workspace_state.map_offset = WorkspacePoint { x: 400.0 - deck.map_position.x, y: 300.0 - deck.map_position.y };
                            self.workspace_state.is_overview = false;
                            self.workspace_state.focused_deck = Some(id);
                        }
                    }
                    CanvasMessage::CursorMoved(pos, surface_id) => {
                        let (sw, sh) = (800.0, 600.0);
                        let _ = WAYLAND_INPUT_CHANNEL.0.send(WaylandInput::Motion { x: pos.x as f64, y: pos.y as f64, surface_id, surface_width: sw, surface_height: sh });
                    }
                    CanvasMessage::ClickDeck(id, local_pos, surface_id) => {
                        self.workspace_state.focused_deck = Some(id);
                        let (sw, sh) = (800.0, 600.0);
                        let _ = WAYLAND_INPUT_CHANNEL.0.send(WaylandInput::Motion { x: local_pos.x as f64, y: local_pos.y as f64, surface_id, surface_width: sw, surface_height: sh });
                        let _ = WAYLAND_INPUT_CHANNEL.0.send(WaylandInput::Button { button: 0x110, state: smithay::backend::input::ButtonState::Pressed, surface_id, surface_width: sw, surface_height: sh });
                    }
                    CanvasMessage::MoveDeck(id, delta) => {
                        if let Some(deck_idx) = self.workspace_state.decks.iter().position(|d| d.id == id) {
                            let mut new_pos = self.workspace_state.decks[deck_idx].target_map_position;
                            new_pos.x += delta.x;
                            new_pos.y += delta.y;
                            
                            let mut snap_x = None;
                            let mut snap_y = None;
                            let mut snap_target_ws = None;
                            
                            for (i, other) in self.workspace_state.decks.iter().enumerate() {
                                if i == deck_idx { continue; }
                                
                                let dx_left = (new_pos.x - (other.target_map_position.x + other.size.width + GAP)).abs();
                                let dx_right = (new_pos.x + self.workspace_state.decks[deck_idx].size.width + GAP - other.target_map_position.x).abs();
                                
                                if dx_left < SNAP_THRESHOLD { snap_x = Some(other.target_map_position.x + other.size.width + GAP); snap_target_ws = Some(other.workspace_id); }
                                if dx_right < SNAP_THRESHOLD { snap_x = Some(other.target_map_position.x - self.workspace_state.decks[deck_idx].size.width - GAP); snap_target_ws = Some(other.workspace_id); }
                                
                                let dy_top = (new_pos.y - (other.target_map_position.y + other.size.height + GAP)).abs();
                                let dy_bottom = (new_pos.y + self.workspace_state.decks[deck_idx].size.height + GAP - other.target_map_position.y).abs();
                                
                                if dy_top < SNAP_THRESHOLD { snap_y = Some(other.target_map_position.y + other.size.height + GAP); snap_target_ws = Some(other.workspace_id); }
                                if dy_bottom < SNAP_THRESHOLD { snap_y = Some(other.target_map_position.y - self.workspace_state.decks[deck_idx].size.height - GAP); snap_target_ws = Some(other.workspace_id); }
                                
                                if (new_pos.y - other.target_map_position.y).abs() < SNAP_THRESHOLD { snap_y = Some(other.target_map_position.y); }
                                if (new_pos.x - other.target_map_position.x).abs() < SNAP_THRESHOLD { snap_x = Some(other.target_map_position.x); }
                            }
                            
                            if let Some(sx) = snap_x { new_pos.x = sx; }
                            if let Some(sy) = snap_y { new_pos.y = sy; }
                            
                            self.workspace_state.decks[deck_idx].target_map_position = new_pos;
                            
                            if let Some(ws_id) = snap_target_ws {
                                self.workspace_state.decks[deck_idx].workspace_id = ws_id;
                            } else {
                                let current_ws_id = self.workspace_state.decks[deck_idx].workspace_id;
                                if let Some(ws) = self.workspace_state.workspaces.iter().find(|w| w.id == current_ws_id) {
                                    let dist_to_ws = (new_pos.x - ws.current_bounds.x).abs().max((new_pos.y - ws.current_bounds.y).abs());
                                    if dist_to_ws > PULL_AWAY_THRESHOLD {
                                        let new_id = Uuid::new_v4();
                                        self.workspace_state.workspaces.push(Workspace {
                                            id: new_id,
                                            name: "Cluster".into(),
                                            color: [rand::random::<f32>(), rand::random::<f32>(), rand::random::<f32>()],
                                            current_bounds: WorkspaceRect { x: new_pos.x, y: new_pos.y - TAB_BAR_HEIGHT, width: 0.0, height: 0.0 },
                                            target_bounds: WorkspaceRect::default(),
                                            opacity: 0.0,
                                        });
                                        self.workspace_state.decks[deck_idx].workspace_id = new_id;
                                    }
                                }
                            }
                        }
                    }
                    CanvasMessage::MoveMap(delta) => { self.workspace_state.map_offset.x += delta.x; self.workspace_state.map_offset.y += delta.y; }
                    CanvasMessage::SwitchTab(deck_id, tab_idx) => { if let Some(deck) = self.workspace_state.decks.iter_mut().find(|d| d.id == deck_id) { deck.active_tab = tab_idx; } }
                    CanvasMessage::MergeDecks { source, target } => {
                        if let Some(source_pos) = self.workspace_state.decks.iter().position(|d| d.id == source) {
                            let mut source_deck = self.workspace_state.decks.remove(source_pos);
                            if let Some(target_deck) = self.workspace_state.decks.iter_mut().find(|d| d.id == target) {
                                target_deck.tabs.append(&mut source_deck.tabs);
                                target_deck.active_tab = target_deck.tabs.len() - 1;
                            }
                        }
                    }
                    CanvasMessage::MoveTab { source_deck, tab_idx, target_deck } => {
                        let mut tab = None;
                        let mut should_remove_source = false;
                        if let Some(s_deck) = self.workspace_state.decks.iter_mut().find(|d| d.id == source_deck) {
                            if tab_idx < s_deck.tabs.len() {
                                tab = Some(s_deck.tabs.remove(tab_idx));
                                if s_deck.tabs.is_empty() { should_remove_source = true; }
                                else { s_deck.active_tab = s_deck.active_tab.min(s_deck.tabs.len() - 1); }
                            }
                        }
                        if should_remove_source { self.workspace_state.decks.retain(|d| d.id != source_deck); }
                        if let (Some(t), Some(t_deck)) = (tab, self.workspace_state.decks.iter_mut().find(|d| d.id == target_deck)) {
                            t_deck.tabs.push(t);
                            t_deck.active_tab = t_deck.tabs.len() - 1;
                        }
                    }
                    CanvasMessage::ExtractTab { source_deck, tab_idx, map_pos } => {
                        let mut tab = None;
                        let mut should_remove_source = false;
                        let mut ws_id = Uuid::new_v4();
                        if let Some(s_deck) = self.workspace_state.decks.iter_mut().find(|d| d.id == source_deck) {
                            if tab_idx < s_deck.tabs.len() {
                                ws_id = s_deck.workspace_id;
                                tab = Some(s_deck.tabs.remove(tab_idx));
                                if s_deck.tabs.is_empty() { should_remove_source = true; }
                                else { s_deck.active_tab = s_deck.active_tab.min(s_deck.tabs.len() - 1); }
                            }
                        }
                        if should_remove_source { self.workspace_state.decks.retain(|d| d.id != source_deck); }
                        if let Some(t) = tab {
                            let new_deck = Deck { id: Uuid::new_v4(), map_position: map_pos, target_map_position: map_pos, size: WorkspaceSize { width: 800.0, height: 600.0 }, active_tab: 0, tabs: vec![t], workspace_id: ws_id };
                            self.workspace_state.decks.push(new_deck);
                        }
                    }
                }
            }
            Message::InputChanged(val) => { self.runner_input = val; }
            Message::SubmitCommand => { if !self.runner_input.is_empty() { let cmd = self.runner_input.clone(); self.spawn_app(cmd); self.runner_input.clear(); self.runner_mode = RunnerMode::Hidden; } }
            Message::NewTabShortcut => { if let Some(deck_id) = self.workspace_state.focused_deck { self.runner_mode = RunnerMode::NewTab(deck_id); } else { self.runner_mode = RunnerMode::LaunchApp; } self.runner_input.clear(); }
            Message::NewDeckShortcut => { self.runner_mode = RunnerMode::LaunchApp; self.runner_input.clear(); }
            Message::CloseTabShortcut => {
                if let Some(deck_id) = self.workspace_state.focused_deck {
                    if let Some(pos) = self.workspace_state.decks.iter().position(|d| d.id == deck_id) {
                        let deck = &mut self.workspace_state.decks[pos];
                        deck.tabs.remove(deck.active_tab);
                        if deck.tabs.is_empty() { self.workspace_state.decks.remove(pos); self.workspace_state.focused_deck = None; }
                        else { deck.active_tab = deck.active_tab.min(deck.tabs.len() - 1); }
                    }
                }
            }
            _ => {}
        }
        iced::Task::none()
    }

    fn spawn_app(&mut self, cmd: String) {
        let socket = self.inner_wayland_socket.clone().unwrap_or_else(|| "wayland-0".into());
        let tab_id = Uuid::new_v4();
        match self.runner_mode {
            RunnerMode::LaunchApp | RunnerMode::Hidden => {
                let ws_id = self.workspace_state.workspaces.first().map(|w| w.id).unwrap_or_else(|| {
                    let id = Uuid::new_v4();
                    self.workspace_state.workspaces.push(Workspace { id, name: "Home".into(), color: [0.3, 0.5, 0.9], current_bounds: WorkspaceRect::default(), target_bounds: WorkspaceRect::default(), opacity: 0.0 });
                    id
                });
                let pos = WorkspacePoint { x: 100.0 - self.workspace_state.map_offset.x, y: 100.0 - self.workspace_state.map_offset.y };
                let _ = Container::new("/tmp").spawn(&cmd, &socket, tab_id);
                let deck = Deck { id: Uuid::new_v4(), map_position: pos, target_map_position: pos, size: WorkspaceSize { width: 800.0, height: 600.0 }, active_tab: 0, tabs: vec![Tab { id: tab_id, title: cmd.clone(), command: cmd, status: TabStatus::Loading, associated_surface_id: None, render_offset: WorkspacePoint::default(), buffer: None, buffer_size: WorkspaceSize::default() }], workspace_id: ws_id };
                self.workspace_state.decks.push(deck);
            }
            RunnerMode::NewTab(deck_id) => {
                if let Some(deck) = self.workspace_state.decks.iter_mut().find(|d| d.id == deck_id) {
                    let _ = Container::new("/tmp").spawn(&cmd, &socket, tab_id);
                    deck.tabs.push(Tab { id: tab_id, title: cmd.clone(), command: cmd, status: TabStatus::Loading, associated_surface_id: None, render_offset: WorkspacePoint::default(), buffer: None, buffer_size: WorkspaceSize::default() });
                }
            }
        }
    }

    fn view(&self) -> Element<'_, Message> {
        let canvas = ui::canvas::view(&self.workspace_state).map(Message::Canvas);
        let top_bar = self.render_top_bar();
        let dock = self.render_dock();
        let runner = self.render_runner();
        let help = self.render_help_panel();
        let sidebar = self.render_sidebar();
        
        stack![
            canvas,
            container(top_bar).width(Length::Fill).height(Length::Fill).align_x(Alignment::Center).align_y(Alignment::Start),
            container(sidebar).width(Length::Fill).height(Length::Fill).align_x(Alignment::End).padding(20),
            container(help).width(Length::Fill).height(Length::Fill).align_x(Alignment::Center).align_y(Alignment::Center),
            container(dock).width(Length::Fill).height(Length::Fill).align_x(Alignment::Center).align_y(Alignment::End).padding(20),
            container(runner).width(Length::Fill).height(Length::Fill).align_x(Alignment::Center).align_y(Alignment::End).padding(100),
        ].into()
    }

    fn render_top_bar(&self) -> Element<'_, Message> {
        container(row![
            button(text("🗺 Overview")).on_press(Message::ToggleOverview).padding(5).style(button::secondary),
            Space::with_width(Length::Fill),
            text(&self.current_time).size(14).style(|_| text::Style { color: Some(Color::from_rgb(0.8, 0.8, 0.8)) }),
            Space::with_width(20),
            button(text("⚙ Settings")).on_press(Message::ToggleSidebar).padding(5).style(button::secondary),
            button(text("❓ Help")).on_press(Message::ToggleHelp).padding(5).style(button::secondary),
        ].padding(10).align_y(Alignment::Center).spacing(10))
        .width(Length::Fill).height(40).style(|_| container::Style { background: Some(iced::Background::Color(Color::from_rgba8(20, 20, 25, 0.9))), ..Default::default() }).into()
    }

    fn render_help_panel(&self) -> Element<'_, Message> {
        if !self.workspace_state.is_help_open { return Space::with_height(0).into(); }
        container(column![
            text("Shortcuts").size(20).style(|_| text::Style { color: Some(Color::from_rgb8(100, 100, 255)) }),
            Space::with_height(15),
            shortcut_row("Alt+Enter", "New Deck"),
            shortcut_row("Alt+T", "New Tab"),
            shortcut_row("Alt+S", "Overview"),
            shortcut_row("Alt+W", "Close"),
            shortcut_row("Alt+I", "Settings"),
            shortcut_row("R-Click Drag", "Pan Map"),
            shortcut_row("L-Click Header", "Move window (Snaps to neighbors)"),
        ].spacing(10).padding(20)).width(300).style(|_| container::Style { background: Some(iced::Background::Color(Color::from_rgba8(30, 30, 35, 0.98))), border: iced::Border { radius: 12.0.into(), width: 1.0, color: Color::from_rgb8(60, 60, 80) }, ..Default::default() }).into()
    }

    fn render_sidebar(&self) -> Element<'_, Message> {
        if !self.workspace_state.is_sidebar_open { return Space::with_height(0).into(); }
        container(column![
            text("Settings").size(24).style(|_| text::Style { color: Some(Color::from_rgb8(100, 100, 255)) }),
            Space::with_height(20),
            text("Active Peer ID:").size(14),
            text(format!("{}", self.my_id)).size(10).style(|_| text::Style { color: Some(Color::from_rgb(0.6, 0.6, 0.6)) }),
            Space::with_height(20),
            button(text("Reset View")).on_press(Message::Canvas(CanvasMessage::MoveMap(WorkspacePoint { x: -self.workspace_state.map_offset.x, y: -self.workspace_state.map_offset.y }))).padding(10).style(button::secondary),
        ].spacing(10)).width(300).height(Length::Fill).padding(20).style(|_| container::Style { background: Some(iced::Background::Color(Color::from_rgba8(20, 20, 20, 0.95))), border: iced::Border { radius: 0.0.into(), width: 1.0, color: Color::from_rgb8(40, 40, 40) }, ..Default::default() }).into()
    }

    fn render_dock(&self) -> Element<'_, Message> {
        if self.workspace_state.is_overview || self.workspace_state.is_help_open { return Space::with_height(0).into(); }
        container(row![
            button(text("Terminal")).on_press(Message::InputChanged("quick-terminal".into())).on_press(Message::SubmitCommand).padding(10).style(button::secondary),
            button(text("Files")).on_press(Message::InputChanged("file-explorer".into())).on_press(Message::SubmitCommand).padding(10).style(button::secondary),
            button(text("Browser")).on_press(Message::InputChanged("adwaita-1-demo".into())).on_press(Message::SubmitCommand).padding(10).style(button::secondary),
        ].spacing(15)).padding(10).style(|_| container::Style { background: Some(iced::Background::Color(Color::from_rgba8(30, 30, 40, 0.8))), border: iced::Border { radius: 12.0.into(), width: 1.0, color: Color::from_rgba8(60, 60, 80, 0.5) }, ..Default::default() }).into()
    }

    fn render_runner(&self) -> Element<'_, Message> {
        if self.runner_mode == RunnerMode::Hidden || self.workspace_state.is_overview { return Space::with_height(0).into(); }
        let prompt = match self.runner_mode { RunnerMode::NewTab(_) => "New Tab...", RunnerMode::LaunchApp => "New Deck...", _ => "" };
        container(column![
            row![
                text_input(prompt, &self.runner_input).on_input(Message::InputChanged).on_submit(Message::SubmitCommand).padding(10).width(400),
                button(text("Run")).on_press(Message::SubmitCommand).padding(10)
            ].spacing(10).align_y(Alignment::Center),
        ].spacing(5)).padding(15).style(|_| container::Style { background: Some(iced::Background::Color(Color::from_rgb8(40, 40, 40))), border: iced::Border { radius: 8.0.into(), width: 1.0, color: Color::from_rgb8(60, 60, 60) }, ..Default::default() }).into()
    }
}

fn shortcut_row(keys: &'static str, desc: &'static str) -> Element<'static, Message> {
    row![
        container(text(keys).size(11).style(|_| text::Style { color: Some(Color::from_rgb8(200, 200, 255)) })).padding(4).style(|_| container::Style { background: Some(iced::Background::Color(Color::from_rgb8(50, 50, 60))), border: iced::Border { radius: 4.0.into(), width: 1.0, color: Color::from_rgb8(80, 80, 100) }, ..Default::default() }),
        text(desc).size(13),
    ].spacing(15).align_y(Alignment::Center).into()
}

impl Default for SpatialDesktop { fn default() -> Self { Self::new() } }
