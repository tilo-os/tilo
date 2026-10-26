//! The workbook's own life: what the app is built from, the resource it attaches to and adopts, what it submits as the person edits, and the locale and currency a document is shown in.

use super::*;

impl SheetsApp {
    pub fn new() -> Self {
        let command_slot = CommandSlot::default();
        Self {
            command_slot,
            bar_commands: Vec::new(),
            chrome_theme: Theme::dark(),
            grid: Grid::new(),
            text_scale: 1.0,
            rect: Rect::ZERO,
            formula_rect: Rect::ZERO,
            grid_rect: Rect::ZERO,
            tabs_rect: Rect::ZERO,
            settings_sheet: None,
            metrics: ChromeMetrics::fallback(),
            tab_rects: Vec::new(),
            add_tab_rect: Rect::ZERO,
            sheet_colors: Vec::new(),
            tab_menu_target: None,
            tab_menu: ContextMenu::new(),
            tab_menu_rows: Vec::new(),
            tab_colour: Popover::with_content(ColorSwatchPicker::new(Vec::new(), 1))
                .side(PopoverSide::Above),
            tab_colour_shown: None,
            cell_menu: ContextMenu::new(),
            cell_menu_rows: Vec::new(),
            cell_menu_target: None,
            find_matches: Vec::new(),
            find_current: None,
            pending_find_replace: Vec::new(),
            pending_ensure_visible: false,
            active_cell_bold: false,
            staged_overlay: None,
            tab_rename: None,
            name_box_rect: Rect::ZERO,
            fx_rect: Rect::ZERO,
            name_box_edit: None,
            formula_field_rect: Rect::ZERO,
            formula_char_x: Vec::new(),
            formula_scroll_x: 0.0,
            formula_bar_selecting: false,
            sheet_names: Vec::new(),
            sheet_charts: Vec::new(),
            sheet_comments: Vec::new(),
            annotation_clicks: Vec::new(),
            managed_chart: None,
            dock_rect: Rect::ZERO,
            dock_cards: Vec::new(),
            dock_scroll: ui::widget::ScrollRegion::horizontal(),
            sheet_ids: Vec::new(),
            workbook: Rc::new(Workbook::default()),
            revision: 0,
            live: None,
            active_sheet: 0,
            status: "Ready".into(),
            status_notice: ui::widget::toast::Transient::new(),
            status_said: String::new(),
            paint_damage: ui::widget::paint_damage::PaintDamage::default(),
            staged: None,
            pending: Pending { redraw: true },
            last_press: None,
            last_mouse: Point::ZERO,
            current_active_format: CellFormat::General,
            settings_panel: SettingsPanel::None,
            global: Locale::default(),
            locale: None,
            doc_settings: DocSettings::default(),
            cache: None,
            pending_reveal: None,
            clipboard: None,
            palette: {
                // window-bar: picker, type-ahead in the Ctrl+K command palette
                let mut palette = CommandPalette::commands();
                // window-bar: picker, the command palette's own query
                palette.set_query_placeholder("Search commands\u{2026}");
                palette
            },
            dispatch: None,
            content: None,
            people: None,
            opening: Rc::default(),
            selection_decls: std::cell::RefCell::default(),
            file: None,
        }
    }

    /// The window shows the workbook of the file at `path`.
    pub(crate) fn show_file_of(&mut self, path: PathBuf) {
        self.file = Some(path);
    }

    /// Take part in the framework: submit workbook edits through its dispatcher.
    pub(crate) fn attach_framework(
        &mut self,
        dispatcher: ui::framework::Dispatcher<Workbook>,
        live: ui::framework::Live<Workbook>,
    ) {
        self.dispatch = Some(dispatcher);
        self.adopt_resource_state(&live.get());
        self.live = Some(live);
    }

    /// What shows a search hit or an agent's `open`: a sheet, or a cell of
    /// one, revealed at the next settle.
    pub(crate) fn opener(
        &self,
    ) -> impl FnMut(&str) -> Result<(), ui::tilo_resource::Refusal> + 'static {
        let opening = Rc::clone(&self.opening);
        move |anchor: &str| {
            let (sheet, addr) = match crate::resource::parse_cell_anchor(anchor) {
                Some(cell) => cell,
                None => ui::tilo_resource::items::item_of_resource::<Workbook>(anchor)
                    .map(|(_, sheet)| (sheet, (0, 0)))
                    .ok_or_else(|| {
                        ui::tilo_resource::Refusal::new(format!(
                            "`{anchor}` is not in the workbook"
                        ))
                    })?,
            };
            *opening.borrow_mut() = Some(PendingReveal {
                sheet_id: Some(sheet),
                addr,
                status: None,
                arriving: false,
            });
            Ok(())
        }
    }

    /// How a comment's `author` is named to the person reading it: by the
    /// name the room knows them by, their own included; never by a raw
    /// participant id. Before anyone has named them, the reader is "You"
    /// and anyone else "Someone"; a record from before ids, whose author
    /// field holds a name, reads as that name.
    pub(crate) fn author_name(&self, author: &str) -> String {
        if author.is_empty() {
            return "Someone".to_owned();
        }
        if let Some(name) = self
            .people
            .as_ref()
            .map(|people| people.name(author))
            .filter(|name| name != author)
        {
            return name;
        }
        let me = self.dispatch.as_ref().and_then(|dispatch| dispatch.me()) == Some(author)
            || self
                .people
                .as_ref()
                .is_some_and(|people| people.is_me(author));
        if me {
            return ui::local_identity::bound()
                .map(|identity| identity.display_name.trim().to_owned())
                .filter(|name| !name.is_empty())
                .unwrap_or_else(|| "You".to_owned());
        }
        if opaque_participant_id(author) {
            "Someone".to_owned()
        } else {
            author.to_owned()
        }
    }

    /// Name comment authors from the room's roster.
    pub(crate) fn attach_people(&mut self, people: ui::framework::People) {
        self.people = Some(people);
        self.pending.redraw = true;
    }

    /// Follow the person's shared locale defaults.
    pub(crate) fn follow_locale(&mut self, locale: ui::framework::Setting<Locale>) {
        self.locale = Some(locale);
    }

    pub(crate) fn adopt_resource_state(&mut self, state: &Rc<Workbook>) {
        if Rc::ptr_eq(state, &self.workbook) {
            return;
        }
        self.workbook = Rc::clone(state);
        self.revision = self.revision.wrapping_add(1);
        // A hidden sheet has no tab; its cells still feed formulas.
        let shown = || state.sheets.iter().filter(|(_, sheet)| !sheet.hidden);
        self.sheet_names = shown().map(|(_, sheet)| sheet.name.0.clone()).collect();
        self.sheet_colors = shown().map(|(_, sheet)| sheet.color).collect();
        self.sheet_ids = shown().map(|(id, _)| id.clone()).collect();
        self.doc_settings = state.settings;
        self.pending.redraw = true;
    }

    /// Read the workbook again after an own edit applied to `doc`.
    pub(super) fn follow(&mut self, doc: &Edits<'_>) {
        let Some(live) = &self.live else {
            return;
        };
        if live.sync(doc.doc()).is_ok() {
            let state = live.get();
            self.adopt_resource_state(&state);
        }
    }

    pub(super) fn active_sheet_id(&self) -> Option<Id> {
        self.sheet_ids.get(self.active_sheet).cloned()
    }

    /// The cells of the active sheet as stored: blank where nothing is.
    pub(super) fn stored_cells(&self) -> impl Fn(Addr) -> Cell + '_ {
        let sheet = self
            .active_sheet_id()
            .and_then(|id| self.workbook.sheet(&id));
        let layout = sheet.map(crate::workbook::Sheet::layout);
        move |addr| {
            sheet
                .zip(layout.as_ref())
                .and_then(|(sheet, layout)| sheet.cell_at(layout, addr).cloned())
                .unwrap_or_default()
        }
    }

    /// Submit `invoke` now through the workbook's dispatcher — the one door
    /// every edit takes, the palette's and an agent's too — and read the
    /// workbook again. A refusal is the status.
    pub(super) fn submit(&mut self, doc: &mut Edits<'_>, invoke: Invoke<Workbook>) -> bool {
        let Some(dispatch) = &self.dispatch else {
            self.status = "The workbook is not open for editing".into();
            return false;
        };
        let applied = match dispatch.submit_now(doc, invoke) {
            Ok(()) => true,
            Err(error) => {
                self.status = error.to_string();
                false
            }
        };
        self.follow(doc);
        applied
    }

    /// Write `writes` (input, and format when given) into the active sheet
    /// as field edits of the cells, one undo step; a cell keeps what a
    /// write leaves out.
    pub(super) fn write_raw(
        &mut self,
        doc: &mut Edits<'_>,
        writes: Vec<(Addr, String, Option<CellFormat>)>,
    ) {
        let stored = self.stored_cells();
        let cells: Vec<(Addr, Cell)> = writes
            .into_iter()
            .map(|(addr, raw, format)| {
                let mut cell = stored(addr);
                cell.raw = Atom(raw);
                if let Some(format) = format {
                    cell.format = format;
                }
                (addr, cell)
            })
            .collect();
        drop(stored);
        self.write_cells(doc, &cells);
    }

    /// Set the format of `cells` of the active sheet, one undo step.
    pub(super) fn write_formats(&mut self, doc: &mut Edits<'_>, cells: Vec<(Addr, CellFormat)>) {
        let stored = self.stored_cells();
        let cells: Vec<(Addr, Cell)> = cells
            .into_iter()
            .map(|(addr, format)| {
                let mut cell = stored(addr);
                cell.format = format;
                (addr, cell)
            })
            .collect();
        drop(stored);
        self.write_cells(doc, &cells);
    }

    /// Store `cells` in the active sheet as they are, one undo step.
    pub(super) fn write_cells(&mut self, doc: &mut Edits<'_>, cells: &[(Addr, Cell)]) {
        let Some(sheet) = self.active_sheet_id() else {
            return;
        };
        let edits = crate::workbook::cell_edits(&self.workbook, &sheet, cells);
        if edits.is_empty() {
            return;
        }
        let members: Vec<Invoke<Workbook>> = edits
            .iter()
            .filter_map(|edit| Invoke::edit(edit).ok())
            .collect();
        // One write is one action: one change, one history entry.
        self.submit(doc, Invoke::group(members));
    }

    /// The document's effective locale (per-doc overrides over global).
    pub(super) fn locale(&self) -> EffectiveLocale {
        EffectiveLocale::resolve(&self.global, &self.doc_settings)
    }

    pub(super) fn default_currency(&self) -> Currency {
        self.locale().currency
    }
}
