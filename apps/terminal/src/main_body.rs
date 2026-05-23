use ui::ColorCompat;
use ui::{Theme, widget::Widget, text_render::TextRenderer};
use automerge::{AutoCommit, ObjId, Value, transaction::Transactable, ObjType, ReadDoc};
use vello::Scene;
use vello::kurbo::{Affine, Rect, Point};
use vello::peniko::Color;
use winit::event::{WindowEvent, ElementState, KeyEvent};
use winit::keyboard::{Key, NamedKey};
use taffy::{TaffyTree, NodeId, Style, Size, Dimension};
use portable_pty::{native_pty_system, CommandBuilder, PtySize, MasterPty};
use std::io::{Read, Write};
use std::sync::{Arc, Mutex};
use vte::{Parser, Perform};
use crossbeam_channel::Receiver;
use unicode_width::UnicodeWidthChar;

#[derive(Clone, Copy, PartialEq, Debug)]
pub struct Attr {
    pub fg: u8,
    pub bg: u8,
    pub bold: bool,
    pub is_wide_cont: bool,
}

impl Attr {
    pub fn pack(self) -> [u8; 4] {
        [self.fg, self.bg, if self.bold { 1 } else { 0 }, if self.is_wide_cont { 1 } else { 0 }]
    }
    pub fn unpack(bytes: &[u8]) -> Self {
        if bytes.len() < 4 { return Self::default(); }
        Self {
            fg: bytes[0],
            bg: bytes[1],
            bold: bytes[2] == 1,
            is_wide_cont: bytes[3] == 1,
        }
    }
}

impl Default for Attr {
    fn default() -> Self {
        Self { fg: 7, bg: 0, bold: false, is_wide_cont: false }
    }
}

pub struct TerminalWidget {
    rect: Rect,
    pty_write: Arc<Mutex<Box<dyn Write + Send>>>,
    pty_master: Arc<Mutex<Box<dyn MasterPty + Send>>>,
    output_rx: Receiver<Vec<u8>>,
    parser: Parser,

    grid: Vec<String>,
    grid_attrs: Vec<Vec<Attr>>,
    cursor_x: usize,
    cursor_y: usize,
    width: usize,
    height: usize,

    char_w: f64,
    char_h: f64,
    /// Cached font size, refreshed from `theme.tokens.font_size_base` on every
    /// `draw`. `measure` runs before `draw` and does not receive a theme, so
    /// it reads this cached value instead of hard-coding a size. Seeded with
    /// 0.0 and updated before the first frame is drawn.
    font_size: f32,

    grid_id: Option<ObjId>,
    cursor_id: Option<ObjId>,
    line_ids: Vec<ObjId>,
    last_sync_heads: Vec<automerge::ChangeHash>,

    current_attr: Attr,
    pending_wrap: bool,

    /// Last palette-derived cursor color, captured on every `draw`. Exposed
    /// for behavioural tests that want to verify palette swaps actually
    /// propagate into rendered state without spinning up a GPU scene.
    last_cursor_color: Color,
    /// Last default-fg color drawn, same rationale as `last_cursor_color`.
    last_fg_default: Color,
    /// Last default-bg color drawn, same rationale as `last_cursor_color`.
    last_bg_default: Color,
}

impl TerminalWidget {
    pub fn new(writer: Arc<Mutex<Box<dyn Write + Send>>>, master: Arc<Mutex<Box<dyn MasterPty + Send>>>, output_rx: Receiver<Vec<u8>>) -> Self {
        let width = 80;
        let height = 24;
        let mut grid = Vec::new();
        let mut grid_attrs = Vec::new();
        for _ in 0..height {
            grid.push(" ".repeat(width));
            grid_attrs.push(vec![Attr::default(); width]);
        }

        Self {
            rect: Rect::ZERO,
            pty_write: writer,
            pty_master: master,
            output_rx,
            parser: Parser::new(),
            grid,
            grid_attrs,
            cursor_x: 0,
            cursor_y: 0,
            width,
            height,
            // Seed values only — `draw` overwrites char_w/char_h from the
            // theme-derived `font_size` on the first frame, and `measure`
            // uses the cached `font_size` (seeded to 0.0 so it falls back to
            // these dimensions until the first draw completes).
            char_w: 9.0,
            char_h: 18.0,
            font_size: 0.0,
            grid_id: None,
            cursor_id: None,
            line_ids: Vec::new(),
            last_sync_heads: Vec::new(),
            current_attr: Attr::default(),
            pending_wrap: false,
            last_cursor_color: Color::from_rgba8(0, 0, 0, 0),
            last_fg_default: Color::from_rgba8(0, 0, 0, 0),
            last_bg_default: Color::from_rgba8(0, 0, 0, 0),
        }
    }

    /// Color used for the block cursor on the last `draw`. Test-only accessor
    /// so palette-swap tests can assert the terminal picks up a new theme.
    pub fn last_cursor_color(&self) -> Color { self.last_cursor_color }
    /// Default-fg color last drawn with (sampled from the theme's terminal
    /// palette). Test-only.
    pub fn last_fg_default(&self) -> Color { self.last_fg_default }
    /// Default-bg color last drawn with (sampled from the theme's terminal
    /// palette). Test-only.
    pub fn last_bg_default(&self) -> Color { self.last_bg_default }

    pub(crate) fn init_crdt(&mut self, state: &mut AutoCommit, root: &ObjId) {
        if self.grid_id.is_none() {
            if let Ok(Some((Value::Object(ObjType::List), id))) = state.get(root, "terminal_grid") {
                self.grid_id = Some(id.clone());
            } else {
                let id = state.put_object(root, "terminal_grid", ObjType::List).unwrap();
                self.grid_id = Some(id);
            }
        }

        let grid_id = self.grid_id.as_ref().unwrap();
        let current_len = state.length(grid_id);

        if current_len < self.height {
            for y in current_len..self.height {
                let line_id = state.insert_object(grid_id, y, ObjType::Map).unwrap();
                let text_id = state.put_object(&line_id, "text", ObjType::Text).unwrap();
                let _ = state.splice_text(&text_id, 0, 0, &" ".repeat(self.width));
                let attr_bytes: Vec<u8> = vec![Attr::default().pack(); self.width].concat();
                let _ = state.put(&line_id, "attrs", attr_bytes);
            }
        }

        let new_len = state.length(grid_id);
        if self.line_ids.len() != new_len {
            self.line_ids.clear();
            for i in 0..new_len {
                if let Ok(Some((Value::Object(ObjType::Map), line_id))) = state.get(grid_id, i) {
                    self.line_ids.push(line_id);
                }
            }
        }

        if self.cursor_id.is_none() {
            if let Ok(Some((Value::Object(ObjType::Map), id))) = state.get(root, "cursor") {
                self.cursor_id = Some(id.clone());
                if let Ok(Some((Value::Scalar(v), _))) = state.get(&id, "y") {
                    if let Some(y) = v.to_i64() { 
                        let abs_y = y as usize;
                        let history_offset = self.line_ids.len().saturating_sub(self.height);
                        self.cursor_y = abs_y.saturating_sub(history_offset).min(self.height.saturating_sub(1));
                    }
                }
            } else {
                let cursor_id = state.put_object(root, "cursor", ObjType::Map).unwrap();
                state.put(&cursor_id, "x", 0u64).unwrap();
                state.put(&cursor_id, "y", 0u64).unwrap();
                self.cursor_id = Some(cursor_id);
            }
        }
    }
}

const MAX_HISTORY: usize = 1000;

impl Widget for TerminalWidget {
    fn measure(&mut self, _known: taffy::Size<Option<f32>>, _available: taffy::Size<taffy::AvailableSpace>, text_renderer: &mut TextRenderer) -> taffy::Size<f32> {
        // `measure` does not receive a theme; read the cached font size set
        // by the most recent `draw` (which pulls from
        // `theme.tokens.font_size_base`). Before the first draw the cache is
        // 0.0 and we keep the constructor-seeded char_w/char_h so layout still
        // yields a usable grid.
        if self.font_size > 0.0 {
            let (w, _h) = text_renderer.measure(" ", self.font_size, None);
            self.char_w = (w as f64).max(1.0);
            self.char_h = ((self.font_size as f64) * 1.25f64).ceil().max(1.0);
        }
        taffy::Size::ZERO
    }

    fn sync(&mut self, state: &mut AutoCommit, root: &ObjId) {
        let current_heads = state.get_heads();
        if !self.last_sync_heads.is_empty() && current_heads == self.last_sync_heads {
            return;
        }
        self.last_sync_heads = current_heads;

        self.init_crdt(state, root);
        
        let grid_id = match self.grid_id.as_ref() {
            Some(id) => id.clone(),
            None => return,
        };

        let len = state.length(&grid_id);
        self.grid.resize(len, String::new());
        self.grid_attrs.resize(len, Vec::new());
        
        for i in 0..len {
            if let Ok(Some((Value::Object(ObjType::Text), text_id))) = state.get(&self.line_ids[i], "text") {
                if let Ok(text) = state.text(&text_id) {
                    self.grid[i] = text;
                }
            }
            if let Ok(Some((Value::Scalar(v), _))) = state.get(&self.line_ids[i], "attrs") {
                if let automerge::ScalarValue::Bytes(bytes) = v.as_ref() {
                    self.grid_attrs[i] = bytes.chunks_exact(4).map(Attr::unpack).collect();
                }
            }
        }
        
        if let Some(cursor_id) = self.cursor_id.as_ref() {
            if let Ok(Some((Value::Scalar(v), _))) = state.get(cursor_id, "x") {
                if let Some(x) = v.to_i64() { self.cursor_x = x as usize; }
            }
            if let Ok(Some((Value::Scalar(v), _))) = state.get(cursor_id, "y") {
                if let Some(y) = v.to_i64() { 
                    let abs_y = y as usize;
                    let history_offset = self.line_ids.len().saturating_sub(self.height);
                    self.cursor_y = abs_y.saturating_sub(history_offset).min(self.height.saturating_sub(1));
                }
            }
        }
    }

    fn update(&mut self, state: &mut AutoCommit, root: &ObjId) -> bool {
        self.init_crdt(state, root);
        
        // Clamp cursors in case of resize
        self.cursor_x = self.cursor_x.min(self.width.saturating_sub(1));
        self.cursor_y = self.cursor_y.min(self.height.saturating_sub(1));

        let mut pty_activity = false;
        while let Ok(bytes) = self.output_rx.try_recv() {
            pty_activity = true;
            let mut performer = TerminalPerformer {
                state,
                grid_id: self.grid_id.clone().unwrap(),
                cursor_id: self.cursor_id.clone().unwrap(),
                line_ids: &mut self.line_ids,
                width: self.width,
                height: self.height,
                cursor_x: self.cursor_x,
                cursor_y: self.cursor_y,
                current_attr: self.current_attr,
                pending_wrap: self.pending_wrap,
            };
            // vte 0.15: `advance` takes `&[u8]` instead of one byte at a time.
            self.parser.advance(&mut performer, &bytes);
            self.cursor_x = performer.cursor_x;
            self.cursor_y = performer.cursor_y;
            self.current_attr = performer.current_attr;
            self.pending_wrap = performer.pending_wrap;
        }
        pty_activity
    }

    fn build_layout(&mut self, taffy: &mut TaffyTree<()>, _theme: &Theme, node_map: &mut std::collections::HashMap<NodeId, *mut dyn Widget>) -> NodeId {
        let node = taffy.new_leaf(Style {
            size: Size { width: Dimension::percent(1.0), height: Dimension::percent(1.0) },
            ..Default::default()
        }).unwrap();
        node_map.insert(node, self as *mut dyn Widget);
        node
    }

    fn apply_layout(&mut self, taffy: &TaffyTree, node: NodeId, origin: Point) {
        let layout = taffy.layout(node).unwrap();
        self.rect = Rect::new(
            origin.x + layout.location.x as f64,
            origin.y + layout.location.y as f64,
            origin.x + layout.location.x as f64 + layout.size.width as f64,
            origin.y + layout.location.y as f64 + layout.size.height as f64,
        );
        
        let new_width = (layout.size.width as f64 / self.char_w.max(1.0)).floor() as usize;
        let new_height = (layout.size.height as f64 / self.char_h.max(1.0)).floor() as usize;
        
        // Cap the CRDT-backed grid at a sensible terminal size. The
        // init pass splices `self.height * self.width` characters into
        // a fresh automerge doc; each char is its own column op, and
        // automerge's column-data insert is O(doc_size), so a
        // full-screen-embed (186 × 219 ≈ 40 700 chars) freezes the
        // process for tens of seconds while the doc compacts. The
        // user-visible region can be larger than the CRDT grid —
        // overflow rows simply render empty until a deeper rework
        // makes the grid sparse / lazy. Defaults match a generous
        // terminal: 200 cols × 80 rows.
        let new_width = new_width.min(200);
        let new_height = new_height.min(80);
        if new_width > 0 && new_height > 0 && (new_width != self.width || new_height != self.height) {
            let history_offset = self.line_ids.len().saturating_sub(self.height);
            let absolute_y = history_offset + self.cursor_y;
            
            let new_history_offset = self.line_ids.len().saturating_sub(new_height);
            let new_cursor_y = absolute_y.saturating_sub(new_history_offset).min(new_height.saturating_sub(1));
            
            self.cursor_y = new_cursor_y;
            self.cursor_x = self.cursor_x.min(new_width.saturating_sub(1));

            self.width = new_width;
            self.height = new_height;
            let _ = self.pty_master.lock().unwrap().resize(PtySize {
                rows: self.height as u16,
                cols: self.width as u16,
                pixel_width: layout.size.width as u16,
                pixel_height: layout.size.height as u16,
            });
        }
    }

    fn draw(&mut self, scene: &mut Scene, transform: Affine, text_renderer: &mut TextRenderer, theme: &Theme, _presence: &ui::presence::PresenceCtx) {
        // Pull every visual parameter from design tokens. Nothing in this
        // method may hard-code a color, font size, or padding — the OS owns
        // the theme and swapping it must reflect in the rendered output.
        //
        // TODO(plan-4): once TextRenderer::draw_text accepts a font family
        // parameter, route `theme.tokens.font_mono.family` through it so the
        // terminal actually shapes with the OS-configured mono font.
        let font_size = theme.tokens.font_size_base;
        let palette = &theme.tokens.terminal_palette;
        // Horizontal gutter between the terminal's left edge and the first
        // glyph column. Sourced from the shared spacing scale (`s1` == 4px
        // in the default tokens) so the OS can tune it centrally.
        let padding_x = theme.tokens.spacing.s1 as f64;

        // Cache the theme-derived font size so `measure` (which runs before
        // `draw` each frame and does not receive a theme) can size the grid
        // from the same value.
        self.font_size = font_size;

        scene.fill(vello::peniko::Fill::NonZero, transform, palette.bg_default, None, &self.rect);

        let (w, _h) = text_renderer.measure(" ", font_size, None);
        self.char_w = (w as f64).max(1.0);
        self.char_h = ((font_size as f64) * 1.25f64).ceil().max(1.0);

        let visible_rows = (self.rect.height() / self.char_h).floor() as usize;
        let start_y = if self.grid.len() > visible_rows { self.grid.len() - visible_rows } else { 0 };

        for (y, line_text) in self.grid.iter().skip(start_y).enumerate() {
            let py = (self.rect.y0 + y as f64 * self.char_h).floor();
            let attrs_idx = y + start_y;
            if attrs_idx >= self.grid_attrs.len() { continue; }
            let attrs = &self.grid_attrs[attrs_idx];

            let mut current_run = String::new();
            let mut current_attr: Option<Attr> = None;
            let mut run_start_cell_idx = 0;

            let mut cell_idx = 0;
            for c in line_text.chars() {
                let c_w = c.width().unwrap_or(1);
                if cell_idx >= self.width { break; }
                let attr = if cell_idx < attrs.len() { attrs[cell_idx] } else { Attr::default() };

                if Some(attr) != current_attr {
                    if !current_run.is_empty() {
                        self.draw_run(scene, &current_run, current_attr.unwrap(), py, run_start_cell_idx as f64 * self.char_w + padding_x, text_renderer, transform, font_size, palette);
                    }
                    current_run.clear();
                    current_attr = Some(attr);
                    run_start_cell_idx = cell_idx;
                }
                current_run.push(c);
                cell_idx += c_w;
            }
            if !current_run.is_empty() {
                self.draw_run(scene, &current_run, current_attr.unwrap(), py, run_start_cell_idx as f64 * self.char_w + padding_x, text_renderer, transform, font_size, palette);
            }
        }

        let cx = self.rect.x0 + padding_x + self.cursor_x as f64 * self.char_w;
        let cy_local = if self.cursor_y >= start_y { self.cursor_y - start_y } else { 0 };
        let cy = (self.rect.y0 + cy_local as f64 * self.char_h).floor();

        let cursor_rect = Rect::new(cx, cy, cx + self.char_w, cy + self.char_h);
        // Cursor color is derived from the palette's default foreground at a
        // reduced opacity so it reads as a translucent block. Using the
        // palette makes the cursor follow theme swaps automatically.
        let cursor_color = cursor_from_palette(palette);
        self.last_cursor_color = cursor_color;
        self.last_fg_default = palette.fg_default;
        self.last_bg_default = palette.bg_default;
        scene.fill(vello::peniko::Fill::NonZero, transform, cursor_color, None, &cursor_rect);
    }

    fn handle_event(&mut self, event: &WindowEvent, _scale_factor: f64, modifiers: &winit::event::Modifiers, state: &mut AutoCommit, root: &ObjId, handled: &mut bool, _focus_target: &mut Option<NodeId>, _node_id: NodeId, _taffy: &TaffyTree<()>) {
        self.init_crdt(state, root);

        match event {
            WindowEvent::KeyboardInput { event: KeyEvent { logical_key, text, state: element_state, .. }, .. } => {
                if *element_state == ElementState::Pressed {
                    let mut bytes = self.map_key(logical_key, modifiers);
                    if bytes.is_none() {
                        if let Some(t) = text {
                            if !t.as_str().chars().any(|c| c as u32 == 0x23CE || c as u32 == 0x21B5) {
                                bytes = Some(t.as_str().as_bytes().to_vec());
                            }
                        }
                    }
                    if let Some(b) = bytes {
                        let mut w = self.pty_write.lock().unwrap();
                        let _ = w.write_all(&b);
                        let _ = w.flush();
                        *handled = true;
                    }
                }
            }
            _ => {}
        }
    }

    fn name(&self) -> &str { "Terminal" }
    fn get_rect(&self) -> Rect { self.rect }
    fn is_focusable(&self) -> bool { true }
}

impl TerminalWidget {
    fn draw_run(
        &self,
        scene: &mut Scene,
        text: &str,
        attr: Attr,
        py: f64,
        rx: f64,
        text_renderer: &mut TextRenderer,
        transform: Affine,
        font_size: f32,
        palette: &ui::tokens::TerminalPalette,
    ) {
        let fg = map_ansi_256(attr.fg, palette);
        let bg = if attr.bg != 0 { Some(map_ansi_256(attr.bg, palette)) } else { None };
        let tx = self.rect.x0 + rx;

        if let Some(bg_color) = bg {
            let mut visual_width = 0.0;
            for c in text.chars() {
                visual_width += c.width().unwrap_or(1) as f64 * self.char_w;
            }
            let bg_rect = Rect::new(tx, py, tx + visual_width + 0.5, py + self.char_h + 0.5);
            scene.fill(vello::peniko::Fill::NonZero, transform, bg_color, None, &bg_rect);
        }

        // TODO(plan-4): pass theme.tokens.font_mono.family through draw_text
        // once TextRenderer gains a font-family parameter.
        text_renderer.draw_text(scene, text, font_size, fg, transform * Affine::translate((tx, py)), None);
    }

    fn map_key(&self, key: &Key, modifiers: &winit::event::Modifiers) -> Option<Vec<u8>> {
        let mods = modifiers.state();
        match key {
            Key::Named(named) => match named {
                NamedKey::Enter => Some(vec![b'\r']),
                NamedKey::Backspace => Some(vec![127]),
                NamedKey::Tab => Some(vec![b'\t']),
                NamedKey::Escape => Some(vec![27]),
                NamedKey::ArrowUp => Some(vec![27, b'[', b'A']),
                NamedKey::ArrowDown => Some(vec![27, b'[', b'B']),
                NamedKey::ArrowRight => Some(vec![27, b'[', b'C']),
                NamedKey::ArrowLeft => Some(vec![27, b'[', b'D']),
                _ => None,
            },
            Key::Character(c) if mods.control_key() => {
                match c.as_str() {
                    "c" | "C" => Some(vec![3]),
                    "d" | "D" => Some(vec![4]),
                    "l" | "L" => Some(vec![12]),
                    "u" | "U" => Some(vec![21]),
                    "z" | "Z" => Some(vec![26]),
                    _ => None,
                }
            }
            _ => None,
        }
    }
}

pub struct TerminalPerformer<'a> {
    pub state: &'a mut AutoCommit,
    pub grid_id: ObjId,
    pub cursor_id: ObjId,
    pub line_ids: &'a mut Vec<ObjId>,
    pub width: usize,
    pub height: usize,
    pub cursor_x: usize,
    pub cursor_y: usize,
    pub current_attr: Attr,
    pub pending_wrap: bool,
}

impl<'a> Perform for TerminalPerformer<'a> {
    fn print(&mut self, c: char) {
        let char_w = c.width().unwrap_or(1);
        if char_w == 0 { return; }

        if self.pending_wrap {
            self.cursor_x = 0;
            self.newline();
            self.pending_wrap = false;
        }

        let y = self.absolute_y().min(self.line_ids.len().saturating_sub(1));
        if let Ok(Some((Value::Object(ObjType::Text), text_id))) = self.state.get(&self.line_ids[y], "text") {
            let current_len = self.state.length(&text_id);
            let replacement = format!("{}{}", c, if char_w > 1 { " ".repeat(char_w - 1) } else { "".into() });

            if self.cursor_x < current_len {
                let _ = self.state.splice_text(&text_id, self.cursor_x, char_w as isize, &replacement);
            } else {
                let _ = self.state.splice_text(&text_id, self.cursor_x, 0, &replacement);
            }

            if let Ok(Some((Value::Scalar(v), _))) = self.state.get(&self.line_ids[y], "attrs") {
                if let automerge::ScalarValue::Bytes(bytes) = v.as_ref() {
                    let mut attr_bytes = bytes.clone();
                    let start = self.cursor_x * 4;
                    let end = (self.cursor_x + char_w) * 4;
                    if attr_bytes.len() < end { attr_bytes.resize(end, 0); }
                    
                    attr_bytes[start..start+4].copy_from_slice(&self.current_attr.pack());
                    if char_w > 1 {
                        let mut cont = self.current_attr;
                        cont.is_wide_cont = true;
                        let cont_packed = cont.pack();
                        for i in 1..char_w {
                            let s = (self.cursor_x + i) * 4;
                            if s + 4 <= attr_bytes.len() { attr_bytes[s..s+4].copy_from_slice(&cont_packed); }
                        }
                    }
                    let _ = self.state.put(&self.line_ids[y], "attrs", attr_bytes);
                }
            }

            self.cursor_x += char_w;
            if self.cursor_x >= self.width {
                self.pending_wrap = true;
            }
        }
        self.update_cursor();
    }

    fn execute(&mut self, byte: u8) {
        self.pending_wrap = false;
        match byte {
            b'\n' => self.newline(),
            b'\r' => self.cursor_x = 0,
            8 => if self.cursor_x > 0 { self.cursor_x -= 1; },
            _ => {}
        }
        self.update_cursor();
    }

    fn csi_dispatch(&mut self, params: &vte::Params, _intermediates: &[u8], _ignore: bool, c: char) {
        self.pending_wrap = false;
        match c {
            'A' => { let n = params.iter().next().and_then(|p| p.first()).cloned().unwrap_or(1) as usize; self.cursor_y = self.cursor_y.saturating_sub(n); }
            'B' => { let n = params.iter().next().and_then(|p| p.first()).cloned().unwrap_or(1) as usize; self.cursor_y = (self.cursor_y + n).min(self.height - 1); }
            'C' => { let n = params.iter().next().and_then(|p| p.first()).cloned().unwrap_or(1) as usize; self.cursor_x = (self.cursor_x + n).min(self.width - 1); }
            'D' => { let n = params.iter().next().and_then(|p| p.first()).cloned().unwrap_or(1) as usize; self.cursor_x = self.cursor_x.saturating_sub(n); }
            'H' | 'f' => {
                let mut it = params.iter();
                let y = it.next().and_then(|p| p.first()).cloned().unwrap_or(1) as usize;
                let x = it.next().and_then(|p| p.first()).cloned().unwrap_or(1) as usize;
                self.cursor_y = (y.saturating_sub(1)).min(self.height - 1);
                self.cursor_x = (x.saturating_sub(1)).min(self.width - 1);
            }
            'J' => {
                let mode = params.iter().next().and_then(|p| p.first()).cloned().unwrap_or(0);
                match mode {
                    2 | 3 => {
                        for y in 0..self.height {
                            let abs_y = (self.line_ids.len().saturating_sub(self.height) + y).min(self.line_ids.len().saturating_sub(1));
                            if let Ok(Some((Value::Object(ObjType::Text), text_id))) = self.state.get(&self.line_ids[abs_y], "text") {
                                let len = self.state.length(&text_id);
                                let _ = self.state.splice_text(&text_id, 0, len as isize, &" ".repeat(self.width));
                                let attr_bytes: Vec<u8> = vec![Attr::default().pack(); self.width].concat();
                                let _ = self.state.put(&self.line_ids[abs_y], "attrs", attr_bytes);
                            }
                        }
                    }
                    _ => {}
                }
            }
            'K' => {
                let mode = params.iter().next().and_then(|p| p.first()).cloned().unwrap_or(0);
                let y = self.absolute_y().min(self.line_ids.len().saturating_sub(1));
                if let Ok(Some((Value::Object(ObjType::Text), text_id))) = self.state.get(&self.line_ids[y], "text") {
                    match mode {
                        0 => {
                            let len = self.state.length(&text_id);
                            if self.cursor_x < len {
                                let _ = self.state.splice_text(&text_id, self.cursor_x, (len - self.cursor_x) as isize, &" ".repeat(len - self.cursor_x));
                                if let Ok(Some((Value::Scalar(v), _))) = self.state.get(&self.line_ids[y], "attrs") {
                                    if let automerge::ScalarValue::Bytes(bytes) = v.as_ref() {
                                        let mut attr_bytes = bytes.clone();
                                        let empty_packed = Attr::default().pack();
                                        for i in self.cursor_x..self.width {
                                            let s = i * 4;
                                            if s + 4 <= attr_bytes.len() { attr_bytes[s..s+4].copy_from_slice(&empty_packed); }
                                        }
                                        let _ = self.state.put(&self.line_ids[y], "attrs", attr_bytes);
                                    }
                                }
                            }
                        }
                        1 => { let _ = self.state.splice_text(&text_id, 0, self.cursor_x as isize, &" ".repeat(self.cursor_x)); }
                        2 => { let len = self.state.length(&text_id); let _ = self.state.splice_text(&text_id, 0, len as isize, &" ".repeat(len)); }
                        _ => {}
                    }
                }
            }
            'm' => {
                for param in params {
                    for v in param {
                        match v {
                            0 => self.current_attr = Attr::default(),
                            1 => self.current_attr.bold = true,
                            22 => self.current_attr.bold = false,
                            30..=37 => self.current_attr.fg = (v - 30) as u8,
                            40..=47 => self.current_attr.bg = (v - 40) as u8,
                            90..=97 => self.current_attr.fg = (v - 90 + 8) as u8,
                            100..=107 => self.current_attr.bg = (v - 100 + 8) as u8,
                            _ => {}
                        }
                    }
                }
            }
            _ => {}
        }
        self.update_cursor();
    }
}

impl<'a> TerminalPerformer<'a> {
    fn newline(&mut self) {
        self.cursor_y += 1;
        
        if self.cursor_y >= self.height {
            self.cursor_y = self.height - 1;
            
            let line_id = self.state.insert_object(&self.grid_id, self.line_ids.len(), ObjType::Map).unwrap();
            let text_id = self.state.put_object(&line_id, "text", ObjType::Text).unwrap();
            let _ = self.state.splice_text(&text_id, 0, 0, &" ".repeat(self.width));
            let attr_bytes: Vec<u8> = vec![Attr::default().pack(); self.width].concat();
            let _ = self.state.put(&line_id, "attrs", attr_bytes).unwrap();
            self.line_ids.push(line_id);

            // Prune history
            if self.line_ids.len() > MAX_HISTORY {
                let overflow = self.line_ids.len() - MAX_HISTORY;
                for _ in 0..overflow {
                    let _ = self.state.delete(&self.grid_id, 0);
                    self.line_ids.remove(0);
                }
            }
        }
    }

    fn absolute_y(&self) -> usize {
        let history_offset = self.line_ids.len().saturating_sub(self.height);
        history_offset + self.cursor_y
    }

    fn update_cursor(&mut self) {
        let _ = self.state.put(&self.cursor_id, "x", self.cursor_x as u64);
        let _ = self.state.put(&self.cursor_id, "y", self.absolute_y() as u64);
    }
}

pub(crate) fn map_ansi_256(code: u8, palette: &ui::tokens::TerminalPalette) -> Color {
    if code < 16 {
        palette.ansi[code as usize]
    } else if code < 232 {
        // 6x6x6 cube colors are deterministic ANSI math, not a themable
        // surface — the cube is defined by the ANSI 256 spec, not by the OS
        // theme. The 16 "anchor" colors above come from the palette, so the
        // rest of the surface still flexes with the theme.
        let n = code - 16;
        let r = (n / 36) * 51;
        let g = ((n % 36) / 6) * 51;
        let b = (n % 6) * 51;
        Color::from_rgb8(r, g, b)
    } else {
        let gray = (code - 232) * 10 + 8;
        Color::from_rgb8(gray, gray, gray)
    }
}

/// Derive the block-cursor color from the palette's default foreground.
/// Kept as a free function so the rule "the cursor always tracks the theme"
/// is a single obvious call site and tests can invoke it directly.
pub(crate) fn cursor_from_palette(palette: &ui::tokens::TerminalPalette) -> Color {
    // 59% alpha (150/255) preserves the legacy translucent-block feel while
    // sourcing the hue from the theme rather than a hard-coded grey.
    palette.fg_default.with_alpha_factor(150.0 / 255.0)
}

pub struct TerminalBackend { pub writer: Box<dyn Write + Send>, pub reader: Box<dyn Read + Send>, pub master: Box<dyn MasterPty + Send>, pub shell_name: String }
impl TerminalBackend {
    pub fn new() -> Result<Self, Box<dyn std::error::Error>> {
        let pty_system = native_pty_system();
        let pair = pty_system.openpty(PtySize { rows: 24, cols: 80, pixel_width: 0, pixel_height: 0 })?;
        let shell = std::env::var("SHELL").unwrap_or_else(|_| "/bin/sh".into());
        let mut cmd = CommandBuilder::new(&shell); cmd.env("TERM", "xterm-256color");
        let _child = pair.slave.spawn_command(cmd)?;
        drop(pair.slave);
        let reader = pair.master.try_clone_reader()?;
        let writer = pair.master.take_writer()?;
        Ok(Self { writer, reader, master: pair.master, shell_name: shell })
    }
}

impl TerminalWidget {
    /// Spawn a PTY + reader thread and return a fully wired `TerminalWidget`.
    /// Returns an error if PTY creation fails.
    pub fn spawn_default() -> anyhow::Result<Self> {
        let backend = TerminalBackend::new().map_err(|e| anyhow::anyhow!(e.to_string()))?;
        let writer = std::sync::Arc::new(std::sync::Mutex::new(backend.writer));
        let master = std::sync::Arc::new(std::sync::Mutex::new(backend.master));
        let (output_tx, output_rx) = crossbeam_channel::unbounded();

        let reader = std::sync::Arc::new(std::sync::Mutex::new(backend.reader));
        std::thread::spawn(move || {
            let mut buf = [0u8; 8192];
            loop {
                let mut r = reader.lock().unwrap();
                match std::io::Read::read(&mut *r, &mut buf) {
                    Ok(0) => break,
                    Ok(n) => { let _ = output_tx.send(buf[..n].to_vec()); }
                    Err(_) => break,
                }
            }
        });

        Ok(TerminalWidget::new(writer, master, output_rx))
    }
}
