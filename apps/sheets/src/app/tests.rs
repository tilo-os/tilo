use super::*;
use crate::actions::SheetsEffect;
use crate::grid::GridCmd;
use automerge::AutoCommit;
use automerge::transaction::Transactable as _;
use ui::framework::testing::TestApp;
use ui::lock::MutexRecover as _;
use ui::portal::PortalOutcome;
use ui::widget::DrawChild;

pub(crate) static CLIP_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

pub(crate) fn attached_app(doc: &AutoCommit) -> SheetsApp {
    let mut app = SheetsApp::new();
    app.attach_framework(
        ui::framework::testing::dispatcher("so.tilo.sheets"),
        ui::framework::testing::detached_live::<Workbook>(doc).unwrap(),
    );
    app
}

pub(crate) fn new_doc() -> AutoCommit {
    ui::tilo_resource::Replica::<Workbook>::new().doc().clone()
}

pub(crate) fn sample_doc() -> AutoCommit {
    crate::sample::replica().doc().clone()
}

pub(crate) fn doc_workbook(doc: &AutoCommit) -> Workbook {
    use ui::tilo_resource::Resource as _;
    Workbook::read(doc).unwrap()
}

pub(crate) fn doc_snapshot(
    doc: &AutoCommit,
    idx: usize,
    _currency: Currency,
) -> crate::workbook::SheetSnapshot {
    doc_workbook(doc)
        .sheets
        .values()
        .nth(idx)
        .map(crate::workbook::Sheet::snapshot)
        .unwrap_or_default()
}

pub(crate) fn doc_cell_raw(doc: &AutoCommit, idx: usize, addr: Addr) -> String {
    doc_snapshot(doc, idx, Currency::USD)
        .raw
        .get(&addr)
        .cloned()
        .unwrap_or_default()
}

pub(crate) fn doc_sheet_count(doc: &AutoCommit) -> usize {
    doc_workbook(doc).sheets.len()
}

pub(crate) fn doc_sheet_names(doc: &AutoCommit) -> Vec<String> {
    doc_workbook(doc)
        .sheets
        .values()
        .map(|sheet| sheet.name.0.clone())
        .collect()
}

pub(crate) fn doc_sheet_color(doc: &AutoCommit, idx: usize) -> Option<u32> {
    doc_workbook(doc)
        .sheets
        .values()
        .nth(idx)
        .and_then(|sheet| sheet.color)
}

pub(crate) fn confirm(app: &mut SheetsApp, doc: &mut AutoCommit) {
    let Some(dispatch) = app.dispatch.clone() else {
        return;
    };
    let mut book = ui::tilo_resource::Replica::<Workbook>::from_doc(doc.clone());
    for action in dispatch.take_asked().expect("what was asked builds") {
        book.apply_action(&action)
            .expect("a confirmed change applies");
    }
    *doc = book.doc().clone();
    app.sync(doc, &automerge::ROOT);
}

pub(super) fn seeded_app() -> (SheetsApp, AutoCommit) {
    let mut doc = sample_doc();
    let mut app = attached_app(&doc);
    app.stage_scenario(None);
    app.settle(&mut Edits::lend(&mut doc), &automerge::ROOT);
    app.sync(&doc, &automerge::ROOT);
    (app, doc)
}

pub(super) fn measured_app(
    width: f64,
    height: f64,
    text_scale: f32,
) -> (SheetsApp, TextRenderer, Theme) {
    let (app, _doc, tr, theme) = measured_app_with_doc(width, height, text_scale);
    (app, tr, theme)
}

pub(super) fn measured_app_with_doc(
    width: f64,
    height: f64,
    text_scale: f32,
) -> (SheetsApp, AutoCommit, TextRenderer, Theme) {
    let (mut app, doc) = seeded_app();
    app.rect = Rect::new(0.0, 0.0, width, height);
    app.relayout();
    let theme = Theme::dark().with_text_scale(text_scale);
    let mut tr = TextRenderer::new();
    tr.set_text_scale(theme.text_scale);
    app.text_scale = f64::from(theme.text_scale);
    app.grid.set_text_scale(app.text_scale);
    app.refresh_chrome_metrics(&mut tr, &theme);
    (app, doc, tr, theme)
}

pub(super) fn paint_frame(app: &mut SheetsApp, tr: &mut TextRenderer, theme: &Theme) {
    let mut scene = Scene::new();
    let presence = PresenceCtx::default();
    app.draw_child(&mut scene, Affine::IDENTITY, tr, theme, &presence);
    app.draw_overlay(&mut scene, Affine::IDENTITY, tr, theme, &presence);
}

fn tab_label_w(tr: &mut TextRenderer, theme: &Theme, label: &str) -> f64 {
    f64::from(
        tr.measure_style(label, None, &theme.text(TextRole::Label).strong())
            .0,
    )
}

#[test]
#[expect(
    clippy::too_many_lines,
    reason = "cases merged from separate tests; each case stays self-contained"
)]
fn chrome_density_follows_measured_text() {
    {
        let (app, _tr, _theme) = measured_app(1280.0, 720.0, 1.0);
        assert!(
            (app.formula_rect.y0 - app.rect.y0).abs() < 0.01,
            "the formula bar leads"
        );
        assert!(
            (app.formula_rect.height() - 40.0).abs() < 0.01,
            "formula bar"
        );
        assert!((app.tabs_rect.height() - 44.0).abs() < 0.01, "tab strip");
        let g = app.grid.metrics();
        assert!((g.col_header_h() - 28.0).abs() < 1.0, "column header");
        assert!(
            g.default_row_h() >= 28.0 && g.default_row_h() < 30.0,
            "default row {}",
            g.default_row_h()
        );
        assert!((g.row_header_w() - 56.0).abs() < 0.01, "row header");
    }

    {
        let (app, mut tr, theme) = measured_app(1280.0, 720.0, 2.0);
        let m = app.metrics.clone();
        assert!(
            m.lh_label > 32.0 && m.lh_label < 36.0,
            "label line box {}",
            m.lh_label
        );

        assert!(app.formula_rect.height() >= m.lh_body + 2.0 * m.s1);
        assert!(app.tabs_rect.height() >= m.lh_label + m.s1);
        assert!(app.grid.metrics().col_header_h() >= m.lh_caption);
        assert!(app.grid.metrics().default_row_h() >= m.lh_body);

        assert!(app.fx_rect.width() >= m.fx_w + 2.0 * m.s2 - 0.01);
        assert!((app.fx_rect.center().y - app.name_box_rect.center().y).abs() < 0.01);
        assert!((app.formula_field_rect.center().y - app.name_box_rect.center().y).abs() < 0.01);
        let reference = f64::from(
            tr.measure_style("ZZ999:ZZ999", None, &theme.text(TextRole::Body))
                .0,
        );
        assert!(app.name_box_rect.width() >= reference);

        for (rect, idx) in &app.tab_rects {
            let name = &app.sheet_names[*idx];
            let w = tab_label_w(&mut tr, &theme, name);
            assert!(rect.width() >= w + 2.0 * m.s3 - 0.01, "tab '{name}' fits");
            assert!(rect.height() >= m.lh_label);
            assert!((rect.center().y - app.add_tab_rect.center().y).abs() < 0.01);
        }
        assert!(
            (app.add_tab_rect.width() - app.add_tab_rect.height()).abs() < 0.01,
            "+ is square"
        );
    }

    // Sheet tabs stay between their floor and ceiling, so a long name never pushes the "+" off.
    {
        for scale in [1.0_f32, 2.0] {
            let (mut app, mut tr, theme) = measured_app(1000.0, 700.0, scale);
            let long = "Quarterly consolidated regional operating expenses by cost centre";
            app.sheet_names = vec![long.to_owned(), "Q".to_owned()];
            app.refresh_chrome_metrics(&mut tr, &theme);
            app.relayout();
            let g = app.metrics.text_growth();
            let s16 = app.metrics.s16;
            let (wide, _) = app.tab_rects[0];
            let (narrow, _) = app.tab_rects[1];
            assert!(
                wide.width() <= 3.0 * s16 * g + 0.01,
                "long name is capped at {scale}x"
            );
            assert!(
                narrow.width() >= s16 * g - 0.01,
                "short name keeps the floor at {scale}x"
            );
            assert!(app.add_tab_rect.x1 <= app.rect.x1, "'+' stays on the strip");
            let style = theme.text(TextRole::Label).strong();
            let room = (wide.width() - 2.0 * app.metrics.s3) as f32;
            let elided = tr.elide_to_width_style(long, room, &style);
            let (ew, _) = tr.measure_style(&elided, None, &style);
            assert!(f64::from(ew) <= f64::from(room) + 0.5);
            assert!(elided.ends_with('…') && elided.chars().count() < long.chars().count());
        }
    }

    {
        for scale in [1.0_f32, 2.0] {
            let (mut app, mut tr, theme) = measured_app(1200.0, 800.0, scale);
            app.open_doc_settings();
            paint_frame(&mut app, &mut tr, &theme);
            let sheet = app.settings_sheet.as_ref().expect("the sheet laid out");
            assert!(
                app.rect.contains(sheet.modal.card.center())
                    && sheet.modal.card.x0 >= app.rect.x0
                    && sheet.modal.card.x1 <= app.rect.x1
                    && sheet.modal.card.y1 <= app.rect.y1,
                "the sheet stays in the window at {scale}x"
            );
            for chip in &sheet.options {
                assert!(
                    sheet.modal.body.x0 - 0.01 <= chip.rect.x0
                        && chip.rect.x1 <= sheet.modal.body.x1 + 0.01,
                    "{:?} stays inside the body at {scale}x",
                    chip.rect
                );
                let (label, _) = app.chip_label_and_state(chip.kind);
                let style = theme.text(TextRole::Label);
                let w = f64::from(tr.measure_style(&label, None, &style).0);
                assert!(chip.rect.width() >= w, "option '{label}' fits");
            }
            for (i, a) in sheet.options.iter().enumerate() {
                for b in sheet.options.iter().skip(i + 1) {
                    assert!(
                        a.rect.intersect(b.rect).area() < 0.01,
                        "options {:?} and {:?} overlap at {scale}x",
                        a.rect,
                        b.rect
                    );
                }
            }
            assert!(
                sheet
                    .modal
                    .actions
                    .iter()
                    .all(|done| sheet.modal.card.contains(done.center())),
                "Done is on the sheet"
            );
        }
    }

    {
        let (mut app, mut tr, theme) = measured_app(1200.0, 800.0, 2.0);
        app.open_tab_menu(0);
        paint_frame(&mut app, &mut tr, &theme);
        assert!(app.tab_menu.is_open());
        assert_eq!(
            app.tab_menu.items().len(),
            app.tab_menu_rows.len(),
            "every menu row has an answer"
        );
        let panel = Widget::get_rect(&app.tab_menu);
        assert!(
            panel.x0 >= app.rect.x0 - 0.01
                && panel.x1 <= app.rect.x1 + 0.01
                && panel.y0 >= app.rect.y0 - 0.01
                && panel.y1 <= app.rect.y1 + 0.01,
            "the menu {panel:?} stays inside the window"
        );
        let tab = app.tab_rects[0].0;
        assert!(
            panel.y1 <= tab.y0 + 0.01 || panel.y0 >= tab.y1 - 0.01,
            "the menu does not cover the tab it acts on"
        );
    }

    {
        let (app, _tr, _theme) = measured_app(560.0, 720.0, 1.0);
        let minimum = f64::from(ui::interaction::InteractionEnvironment::POINTER.minimum_target());
        assert!(app.name_box_rect.height() >= minimum);
        assert!(app.formula_field_rect.height() >= minimum);
        assert!(
            app.tab_rects
                .iter()
                .all(|(rect, _)| rect.height() + 2.0 * app.metrics.s1 >= minimum)
        );

        let (app2, _tr, _theme) = measured_app(560.0, 720.0, 2.0);
        assert!(app2.name_box_rect.height() >= minimum * 1.25);
        assert!(
            app2.grid.accessibility_window().rows[0].bounds.height()
                >= app.grid.accessibility_window().rows[0].bounds.height() * 1.5
        );
    }
}

#[test]
fn idle_frames_do_no_redundant_work() {
    {
        let (mut app, mut doc) = seeded_app();
        app.sync(&doc, &automerge::ROOT);
        let first_revision = app.revision;
        let first_ids = app.sheet_ids.as_ptr();
        app.sync(&doc, &automerge::ROOT);
        assert_eq!(app.revision, first_revision);
        assert_eq!(app.sheet_ids.as_ptr(), first_ids);

        app.switch_sheet(&mut Edits::lend(&mut doc), 1);
        app.sync(&doc, &automerge::ROOT);
        assert_eq!(app.sheet_ids.as_ptr(), first_ids);
    }
}

#[test]
fn affordances_and_resource_operations_match_action_specs() {
    {
        let (mut app, mut doc) = seeded_app();
        let count_before = doc_sheet_count(&doc);
        assert!(count_before >= 2, "seeded workbook has multiple sheets");
        let sales = app.sheet_ids[1].clone();
        let item = ui::tilo_resource::items::item_resource_id::<Workbook>("sheets", &sales);
        let resources = app.published();
        let offered = tilo_actions::flatten(&resources)
            .into_iter()
            .any(|flat| flat.resource_id.as_str() == item && flat.action.id == "delete-sheet");
        assert!(offered, "the sheet offers its delete");

        app.run_tab_action(&mut Edits::lend(&mut doc), 1, TabAction::Delete);
        confirm(&mut app, &mut doc);
        assert_eq!(
            doc_sheet_count(&doc),
            count_before - 1,
            "the affordance really removed the sheet"
        );
    }

    {
        let (mut app, mut doc) = seeded_app();
        app.grid.select_only((0, 9)); // A10, an empty cell
        let before = doc.get_heads();
        app.apply_cmds(
            &mut Edits::lend(&mut doc),
            vec![
                GridCmd::SetCell {
                    addr: (0, 9),
                    raw: "hello".into(),
                },
                GridCmd::Commit,
            ],
        );
        assert_eq!(doc_cell_raw(&doc, 0, (0, 9)), "hello");
        let after = doc.get_heads();
        let patches = doc.diff(&before, &after);
        assert!(
            patches.iter().all(|patch| {
                patch
                    .path
                    .iter()
                    .all(|(_, prop)| !matches!(prop, automerge::Prop::Map(key) if key == "name"))
            }),
            "a cell edit writes its cell, not the sheet"
        );
    }
}

pub(super) fn sample_app() -> TestApp<crate::resource::SheetsShell> {
    let mut app = TestApp::<crate::resource::SheetsShell>::new().unwrap();
    for invoke in crate::sample::workbook() {
        app.invoke_confirmed(invoke).unwrap();
    }
    app
}

fn shown(app: &mut TestApp<crate::resource::SheetsShell>) -> (usize, String) {
    app.settle();
    app.with_surface::<SheetsApp, _>(|sheets| (sheets.active_sheet, sheets.status.clone()))
        .expect("the grid is in the view")
}

fn raw_at(app: &TestApp<crate::resource::SheetsShell>, sheet: usize, addr: Addr) -> String {
    app.state()
        .sheets
        .values()
        .nth(sheet)
        .map(crate::workbook::Sheet::snapshot)
        .and_then(|snapshot| snapshot.raw.get(&addr).cloned())
        .unwrap_or_default()
}

#[test]
fn the_workbook_publishes_its_effects_and_items_open_themselves() {
    let mut app = sample_app();
    let resources = app.harness().resources();
    let workbook = &resources[0];
    for id in [
        "import-csv",
        "import-xlsx",
        "import-ods",
        "export-csv",
        "export-xlsx",
        "export-ods",
        "commands",
        "go-to-cell",
        "toggle-formula-view",
    ] {
        assert!(
            workbook.actions.iter().any(|action| action.id == id),
            "{id} is published"
        );
    }
    for (id, extension) in [
        ("import-csv", "csv"),
        ("import-xlsx", "xlsx"),
        ("import-ods", "ods"),
    ] {
        let import = workbook
            .actions
            .iter()
            .find(|action| action.id == id)
            .unwrap();
        assert!(import.contract.opens_path(), "{id} opens a path");
        assert_eq!(
            import.contract.opened_extensions().collect::<Vec<_>>(),
            [extension]
        );
    }
    assert!(
        workbook
            .actions
            .iter()
            .filter(|action| action.id.starts_with("export-"))
            .all(|action| !action.contract.opens_path()),
        "an export takes a path but opens nothing"
    );
    let sales = workbook
        .children
        .iter()
        .find(|child| child.label == "Sales")
        .expect("the Sales sheet");
    assert!(
        workbook.actions.iter().any(|action| action.id == "open"),
        "the workbook opens what a search hit or an agent names"
    );

    app.call("open", serde_json::json!({ "anchor": sales.id.as_str() }))
        .expect("the sheet opens");
    app.settle();
    let (active, status) = shown(&mut app);
    assert_eq!(active, 1, "the Sales sheet is shown");
    assert_eq!(status, "Went to Sales!A1");

    let budget = app.state().sheets.ids().next().unwrap().clone();
    let cell = crate::resource::cell_anchor(&budget, (1, 4));
    app.call("open", serde_json::json!({ "anchor": cell }))
        .expect("a cell opens");
    app.settle();
    let selected = app
        .with_surface::<SheetsApp, _>(|sheets| (sheets.active_sheet, sheets.grid.active()))
        .unwrap();
    assert_eq!(selected, (0, (1, 4)), "Budget!B5 is selected");

    app.effect(&SheetsEffect::GoToCell {
        reference: "C3".into(),
    })
    .unwrap();
    app.effect(&SheetsEffect::ToggleFormulaView).unwrap();
    let view = app
        .with_surface::<SheetsApp, _>(|sheets| (sheets.grid.active(), sheets.grid.formula_view()))
        .unwrap();
    assert_eq!(view, ((2, 2), true));
}

#[test]
fn every_format_exports_to_a_path_and_imports_back_once() {
    let dir = tempfile::tempdir().unwrap();
    for ext in ["csv", "xlsx", "ods"] {
        let mut app = sample_app();
        let path = dir.path().join(format!("Budget.{ext}"));
        let at = Some(path.as_path().try_into().unwrap());
        let (export, import) = match ext {
            "csv" => (
                SheetsEffect::ExportCsv { path: at.clone() },
                SheetsEffect::ImportCsv { path: at },
            ),
            "xlsx" => (
                SheetsEffect::ExportXlsx { path: at.clone() },
                SheetsEffect::ImportXlsx { path: at },
            ),
            _ => (
                SheetsEffect::ExportOds { path: at.clone() },
                SheetsEffect::ImportOds { path: at },
            ),
        };
        assert!(
            matches!(
                app.effect(&export),
                Err(ui::tilo_actions::DoorError::ConfirmationRequired)
            ),
            "writing over a named path asks first"
        );
        app.effect_confirmed(&export).unwrap();
        assert!(path.exists(), "the .{ext} was written");
        assert!(shown(&mut app).1.starts_with("Exported"));

        let before = app.state().sheets.len();
        app.effect(&import).unwrap();
        app.settle();
        let added = if ext == "csv" { 1 } else { before };
        assert_eq!(app.state().sheets.len(), before + added, ".{ext}");
        assert_eq!(raw_at(&app, before, (0, 0)), "Category");
        if ext != "csv" {
            assert_eq!(
                raw_at(&app, before + 1, (0, 0)),
                "Region",
                "the second sheet travels too"
            );
            assert_eq!(
                raw_at(&app, before, (4, 1)),
                "=SUM(B2:D2)",
                "a formula comes back live"
            );
        }
        let (active, status) = shown(&mut app);
        assert_eq!(active, before + added - 1, "the last added sheet is shown");
        assert!(status.starts_with("Imported"), "{status}");

        app.with_surface::<SheetsApp, _>(|sheets| sheets.active_sheet = 0);
        app.effect(&import).unwrap();
        app.settle();
        assert_eq!(app.state().sheets.len(), before + added, "no second copy");
        let (active, status) = shown(&mut app);
        assert_eq!(active, before, "the open sheet is shown");
        assert!(status.contains("already open"), "{status}");

        assert!(app.undo(), "the import is one undo step");
        assert_eq!(app.state().sheets.len(), before);
    }
}

#[test]
fn the_file_chooser_saves_exports_and_picks_imports() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("Exported.csv");
    let (services, recorder) = ui::host::HostServices::recording();
    let _host = ui::host::enter(&services);
    let mut app = sample_app();

    recorder.respond_to_pick(PortalOutcome::Opened(path.clone()));
    app.effect(&SheetsEffect::ExportCsv { path: None })
        .expect("exporting through the save dialog asks nothing first");
    let written = std::fs::read_to_string(&path).expect("the chosen file was written");
    assert!(written.starts_with("Category"), "{written}");
    assert_eq!(
        recorder.pick_requests()[0].suggested_name(),
        Some("Budget.csv")
    );

    let before = app.state().sheets.len();
    recorder.respond_to_pick(PortalOutcome::Cancelled);
    app.effect(&SheetsEffect::ImportXlsx { path: None })
        .unwrap();
    app.settle();
    assert_eq!(
        app.state().sheets.len(),
        before,
        "a cancelled import adds nothing"
    );

    recorder.respond_to_pick(PortalOutcome::Opened(path.clone()));
    app.effect(&SheetsEffect::ImportCsv { path: None }).unwrap();
    app.settle();
    let state = app.state();
    assert_eq!(state.sheets.len(), before + 1);
    assert_eq!(
        state
            .sheets
            .values()
            .nth(before)
            .map(|sheet| sheet.name.0.as_str()),
        Some("Exported")
    );
    assert_eq!(raw_at(&app, before, (0, 0)), "Category");
    assert_eq!(recorder.pick_requests().len(), 3);

    app.with_surface::<SheetsApp, _>(|sheets| sheets.active_sheet = 0);
    recorder.respond_to_pick(PortalOutcome::Opened(path.clone()));
    app.effect(&SheetsEffect::ImportCsv { path: None }).unwrap();
    app.settle();
    assert_eq!(app.state().sheets.len(), before + 1, "no second copy");
    let (active, status) = shown(&mut app);
    assert_eq!(active, before, "the imported sheet is shown");
    assert!(status.contains("already open"), "{status}");
    app.effect(&SheetsEffect::ImportCsv {
        path: Some(path.as_path().try_into().unwrap()),
    })
    .unwrap();
    app.settle();
    assert_eq!(app.state().sheets.len(), before + 1, "by path either");
}

#[test]
fn a_file_that_cannot_be_read_fails_its_import() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("broken.xlsx");
    std::fs::write(&path, b"not a workbook").unwrap();
    let mut app = sample_app();
    let before = app.state().sheets.len();
    let error = app
        .effect(&SheetsEffect::ImportXlsx {
            path: Some(path.as_path().try_into().unwrap()),
        })
        .expect_err("a broken package fails");
    assert!(
        matches!(error, tilo_actions::DoorError::Failed(_)),
        "{error:?}"
    );
    assert_eq!(app.state().sheets.len(), before);
    assert!(
        shown(&mut app)
            .1
            .starts_with("Spreadsheet operation failed")
    );

    let sparse = format!("{}x", "\n".repeat(MAX_CSV_MATERIALIZED_SLOTS));
    let error = SheetsApp::read_tables(Interchange::Csv, sparse.as_bytes(), "sparse")
        .expect_err("a sparse far-away cell must not allocate a huge rectangle");
    assert!(error.contains("dimensions require"), "{error}");
}

fn value_at(app: &SheetsApp, addr: Addr) -> Option<CellValue> {
    app.cache
        .as_ref()
        .and_then(|c| c.values.get(&addr).cloned())
}

#[test]
fn presence_and_sheet_switching_follow_the_active_tab() {
    {
        let (mut app, _doc) = seeded_app();
        assert!(!app.sheet_names.is_empty(), "seeded app has sheets");

        let (key, payload) =
            ui::presence::capability::presence_payload(&app).expect("a sheet is active");
        let scope = presence::CellScope::from_key(&key).expect("cell scope");
        assert_eq!(scope.sheet, app.sheet_ids[app.active_sheet].as_str());
        assert_eq!(payload, PresencePayload::Arrived);

        if app.sheet_names.len() > 1 {
            let other = (app.active_sheet + 1) % app.sheet_names.len();
            app.active_sheet = other;
            let (key2, _) = ui::presence::capability::presence_payload(&app).expect("still active");
            let scope = presence::CellScope::from_key(&key2).expect("cell scope");
            assert_eq!(scope.sheet, app.sheet_ids[other].as_str());
        }
    }

    {
        let (mut app, mut doc) = seeded_app();
        app.grid.select_only((0, 30)); // A31 on sheet 0
        app.grid.begin_edit_in_place();
        app.grid.insert_text_public("draft");
        app.switch_sheet(&mut Edits::lend(&mut doc), 1); // click the "Sales" tab
        assert_eq!(app.active_sheet, 1);
        assert_eq!(
            doc.pending_ops(),
            0,
            "switching tabs is local presentation and must not write the shared workbook"
        );
        assert_eq!(doc_cell_raw(&doc, 0, (0, 30)), "draft");
        assert_eq!(doc_cell_raw(&doc, 1, (0, 30)), "");
    }
}

#[test]
fn formula_bar_and_fx_start_formulas() {
    {
        let (app, _doc) = seeded_app();
        assert_eq!(app.grid.active(), (4, 1));
        assert!(!app.grid.is_editing());
        assert_eq!(app.grid.active_display_raw(), "=SUM(B2:D2)");
        // The selected-formula highlight must be available without editing.
        assert_eq!(
            app.grid.highlight_formula_body().as_deref(),
            Some("SUM(B2:D2)")
        );
    }

    {
        let (mut app, _doc) = seeded_app();
        app.rect = Rect::new(0.0, 0.0, 1000.0, 700.0);
        app.relayout();
        assert!(app.name_box_rect.width() > 0.0 && app.name_box_rect.height() > 0.0);
        assert!(app.fx_rect.width() > 0.0 && app.fx_rect.height() > 0.0);
        assert!(
            app.name_box_rect.x1 <= app.fx_rect.x0 + 0.01,
            "name box overlaps fx"
        );
        assert!(
            app.fx_rect.x1 <= app.formula_field_rect.x0 + 0.01,
            "fx overlaps field"
        );
        assert!(app.formula_rect.contains(app.name_box_rect.center()));
        assert!(app.formula_rect.contains(app.fx_rect.center()));
    }

    {
        let (mut app, _doc) = seeded_app();
        app.grid.select_only((0, 20)); // A21, empty
        assert!(!app.grid.is_editing());
        app.start_formula_entry();
        assert!(app.grid.is_editing());
        assert_eq!(app.grid.active_display_raw(), "=");
        assert_eq!(app.grid.edit_caret(), Some(1));
    }

    {
        let (mut app, mut doc) = seeded_app();
        app.apply_cmds(
            &mut Edits::lend(&mut doc),
            vec![
                GridCmd::SetCell {
                    addr: (0, 21),
                    raw: "5".into(),
                },
                GridCmd::Commit,
            ],
        );
        app.grid.select_only((0, 21)); // A22 = 5
        app.start_formula_entry();
        assert_eq!(app.grid.active_display_raw(), "=5");
        let mut out = Vec::new();
        app.grid.flush_edit(&mut out);
        app.apply_cmds(&mut Edits::lend(&mut doc), out);
        assert_eq!(doc_cell_raw(&doc, 0, (0, 21)), "=5");
        assert_eq!(value_at(&app, (0, 21)), Some(CellValue::Number(5.0)));
    }
}

#[test]
fn recalculation_and_auto_sum_write_working_formulas() {
    {
        let (app, _doc) = seeded_app();
        assert_eq!(value_at(&app, (4, 1)), Some(CellValue::Number(3600.0)));
        assert!(matches!(app.format_of_active(), CellFormat::Currency(_)));
    }

    {
        let (mut app, mut doc) = seeded_app();
        let b2 = (1, 1);
        let e2 = (4, 1);
        assert_eq!(value_at(&app, e2), Some(CellValue::Number(3600.0)));

        app.apply_cmds(
            &mut Edits::lend(&mut doc),
            vec![
                GridCmd::SetCell {
                    addr: b2,
                    raw: "1000".into(),
                },
                GridCmd::Commit,
            ],
        );

        assert_eq!(doc_cell_raw(&doc, 0, b2), "1000");
        assert_eq!(value_at(&app, e2), Some(CellValue::Number(3400.0)));
    }

    {
        let (mut app, mut doc) = seeded_app();
        app.grid.select_only((1, 6)); // B7, under B2:B6
        app.auto_sum(&mut Edits::lend(&mut doc));
        let raw = doc_cell_raw(&doc, 0, (1, 6));
        assert_eq!(raw, "=SUM(B2:B6)", "auto-sum wrote {raw:?}");
        match value_at(&app, (1, 6)) {
            Some(CellValue::Number(n)) => assert!(n > 0.0),
            other => panic!("expected a numeric AutoSum result, got {other:?}"),
        }
    }

    {
        let (mut app, mut doc) = seeded_app();
        app.grid.select_only((0, 0)); // A1, row 0
        app.auto_sum(&mut Edits::lend(&mut doc));
        assert_eq!(app.status, "Nothing above to sum");
        assert_eq!(doc_cell_raw(&doc, 0, (0, 0)), "Category"); // unchanged
    }

    {
        let (mut app, mut doc) = seeded_app();
        app.grid.select_only((0, 1)); // A2, directly under A1 = "Category" (text)
        let before = doc_cell_raw(&doc, 0, (0, 1));
        app.auto_sum(&mut Edits::lend(&mut doc));
        assert_eq!(app.status, "No numeric run above");
        assert_eq!(doc_cell_raw(&doc, 0, (0, 1)), before);
        assert!(!doc_cell_raw(&doc, 0, (0, 1)).starts_with("=SUM"));
    }
}

#[test]
fn formats_apply_and_clear_across_selections() {
    {
        let (mut app, mut doc) = seeded_app();
        app.grid.select_only((1, 8)); // B9 = 2600 (currency in the seed)
        app.apply_format(&mut Edits::lend(&mut doc), CellFormat::Percent);
        assert_eq!(app.format_of_active(), CellFormat::Percent);
    }

    {
        let (mut app, mut doc) = seeded_app();
        app.grid.select_range_public((1, 1), (1, 2)); // B2:B3
        app.grid.add_range_public((3, 1), (3, 2)); // D2:D3 (discontiguous)
        app.apply_format(&mut Edits::lend(&mut doc), CellFormat::Percent);
        let snap = doc_snapshot(&doc, 0, Currency::USD);
        for a in [(1, 1), (1, 2), (3, 1), (3, 2)] {
            assert_eq!(
                snap.fmt.get(&a).copied(),
                Some(CellFormat::Percent),
                "cell {a:?}"
            );
        }
        assert!(matches!(
            snap.fmt.get(&(2, 1)),
            Some(CellFormat::Currency(_))
        ));
    }

    {
        let (mut app, mut doc) = seeded_app();
        app.grid.select_only((0, 25)); // A26, empty
        app.apply_format(&mut Edits::lend(&mut doc), CellFormat::Percent);
        assert_eq!(app.format_of_active(), CellFormat::Percent);
        app.clear_format(&mut Edits::lend(&mut doc));
        assert_eq!(app.format_of_active(), CellFormat::General);
        let snap = doc_snapshot(&doc, 0, Currency::USD);
        assert_eq!(snap.fmt.get(&(0, 25)).copied(), None);
    }

    {
        let (mut app, mut doc) = seeded_app();
        app.grid.select_only((1, 1)); // B2
        app.dispatch_command(&mut Edits::lend(&mut doc), "format_percent", None);
        let cur = app.default_currency();
        let snap = doc_snapshot(&doc, 0, cur);
        assert_eq!(snap.fmt.get(&(1, 1)), Some(&CellFormat::Percent));
    }
}

#[test]
fn selection_summary_aggregates_every_range() {
    {
        let (mut app, _doc) = seeded_app();
        app.grid.select_range_public((1, 1), (1, 5)); // B2:B6
        app.grid.add_range_public((3, 1), (3, 5)); // D2:D6 (discontiguous)
        assert!(app.grid.is_multi_range(), "two ranges are selected");

        let s = app
            .selection_summary()
            .expect("a numeric multi-range selection has a status-bar summary");
        assert_eq!(s.count, 10, "5 cells from B2:B6 + 5 from D2:D6");
        assert!(
            (s.sum - 4220.0).abs() < 1e-6,
            "sum over both ranges, got {}",
            s.sum
        );
        assert!(
            (s.avg - 422.0).abs() < 1e-6,
            "avg = 4220 / 10, got {}",
            s.avg
        );
    }

    {
        let (mut app, _doc) = seeded_app();
        app.grid.select_range_public((1, 1), (1, 2)); // B2:B3 = 1200, 420
        app.grid.add_range_public((2, 1), (2, 2)); // C2:C3 = 1200, 460
        let s = app
            .selection_summary()
            .expect("numeric multi-range selection");
        assert_eq!(s.count, 4);
        assert!(
            (s.sum - (1200.0 + 420.0 + 1200.0 + 460.0)).abs() < 1e-6,
            "sum={}",
            s.sum
        );
    }

    {
        let (mut app, _doc) = seeded_app();
        app.grid.set_selection((1, 1), (1, 5));
        let summary = app.selection_summary().expect("numeric selection");
        assert_eq!(summary.count, 5);
        assert!((summary.sum - 2040.0).abs() < 1e-6, "sum={}", summary.sum);
        assert!((summary.avg - 408.0).abs() < 1e-6, "avg={}", summary.avg);
    }
}

#[test]
fn imported_tables_restore_formulas_and_replace_a_blank_workbook() {
    {
        let (app, doc) = seeded_app();
        let table =
            SheetsApp::table_from_snapshot(&doc_snapshot(&doc, app.active_sheet, Currency::USD));
        let path = std::env::temp_dir().join("sheets-app-formula-rt.xlsx");
        crate::xlsx::write_xlsx(&path, std::slice::from_ref(&table)).unwrap();
        let tables = crate::xlsx::read_xlsx(&path).unwrap();
        let _ = std::fs::remove_file(&path);

        let mut doc2 = new_doc();
        let mut fresh = attached_app(&doc2);
        fresh
            .import_tables(&mut Edits::lend(&mut doc2), tables)
            .unwrap();

        assert_eq!(doc_cell_raw(&doc2, 0, (4, 1)), "=SUM(B2:D2)");
        let snap = doc_snapshot(&doc2, 0, Currency::USD);
        let values = crate::calc::recalculate(&snap.raw);
        assert_eq!(values.get(&(4, 1)), Some(&CellValue::Number(3600.0)));
        assert_eq!(doc_cell_raw(&doc2, 0, (1, 1)), "1200");
    }

    {
        let mut doc = new_doc();
        let mut app = attached_app(&doc);
        let table = crate::xlsx::SheetTable {
            name: "sales".into(),
            hidden: false,
            rows: vec![vec![Some(crate::xlsx::ImportedCell {
                raw: "Region".into(),
                format: CellFormat::General,
                formula: None,
                bold: false,
            })]],
        };
        app.import_tables(&mut Edits::lend(&mut doc), vec![table.clone()])
            .unwrap();
        assert_eq!(doc_sheet_names(&doc), ["sales"]);
        assert_eq!(app.active_sheet, 0);
        app.import_tables(&mut Edits::lend(&mut doc), vec![table])
            .unwrap();
        assert_eq!(
            doc_sheet_names(&doc).len(),
            2,
            "a used workbook keeps its sheets"
        );
    }
}

#[test]
fn fill_commands_and_handle_extend_series_and_formulas() {
    {
        let (mut app, mut doc) = seeded_app();
        app.grid.select_range_public((4, 1), (4, 3)); // E2:E4, E2 = =SUM(B2:D2)
        app.fill_down(&mut Edits::lend(&mut doc));
        assert_eq!(doc_cell_raw(&doc, 0, (4, 2)), "=SUM(B3:D3)");
        assert_eq!(doc_cell_raw(&doc, 0, (4, 3)), "=SUM(B4:D4)");
        match value_at(&app, (4, 2)) {
            Some(CellValue::Number(n)) => assert!(n > 0.0, "filled formula recomputed to {n}"),
            other => panic!("expected numeric fill result, got {other:?}"),
        }
    }

    {
        let (mut app, mut doc) = seeded_app();
        app.apply_cmds(
            &mut Edits::lend(&mut doc),
            vec![
                GridCmd::SetCell {
                    addr: (7, 1),
                    raw: "1".into(),
                }, // H2
                GridCmd::SetCell {
                    addr: (7, 2),
                    raw: "2".into(),
                }, // H3
                GridCmd::Commit,
            ],
        );
        app.grid.select_range_public((7, 1), (7, 2)); // the pattern
        app.grid.start_fill_to((7, 5)); // drag the handle down to H6
        app.run_pending_fill(&mut Edits::lend(&mut doc));
        assert_eq!(doc_cell_raw(&doc, 0, (7, 3)), "3");
        assert_eq!(doc_cell_raw(&doc, 0, (7, 4)), "4");
        assert_eq!(doc_cell_raw(&doc, 0, (7, 5)), "5");
        assert_eq!(value_at(&app, (7, 5)), Some(CellValue::Number(5.0)));
    }

    {
        let (mut app, mut doc) = seeded_app();
        app.grid.select_only((4, 1)); // E2 = =SUM(B2:D2)
        app.grid.start_fill_to((4, 4)); // drag down to E5
        app.run_pending_fill(&mut Edits::lend(&mut doc));
        assert_eq!(doc_cell_raw(&doc, 0, (4, 2)), "=SUM(B3:D3)");
        assert_eq!(doc_cell_raw(&doc, 0, (4, 3)), "=SUM(B4:D4)");
        assert_eq!(doc_cell_raw(&doc, 0, (4, 4)), "=SUM(B5:D5)");
        match value_at(&app, (4, 4)) {
            Some(CellValue::Number(n)) => assert!(n > 0.0, "filled formula recomputed to {n}"),
            other => panic!("expected numeric fill result, got {other:?}"),
        }
    }

    {
        let (mut app, mut doc) = seeded_app();
        app.apply_cmds(
            &mut Edits::lend(&mut doc),
            vec![
                GridCmd::SetCell {
                    addr: (7, 10),
                    raw: "10".into(),
                }, // H11
                GridCmd::SetCell {
                    addr: (8, 10),
                    raw: "20".into(),
                }, // I11
                GridCmd::Commit,
            ],
        );
        app.grid.select_range_public((7, 10), (8, 10));
        app.grid.start_fill_to((10, 10)); // drag right to K11
        app.run_pending_fill(&mut Edits::lend(&mut doc));
        assert_eq!(doc_cell_raw(&doc, 0, (9, 10)), "30");
        assert_eq!(doc_cell_raw(&doc, 0, (10, 10)), "40");
    }

    {
        assert_eq!(crate::refs::offset_refs("=A1+$B$2", 1, 1), "=B2+$B$2");
        assert_eq!(
            crate::refs::offset_refs("=SUM(A1:A3)*2", 2, 0),
            "=SUM(C1:C3)*2"
        );
        assert_eq!(crate::refs::offset_refs("=$A1+A$1", 1, 1), "=$A2+B$1");
        assert_eq!(crate::refs::offset_refs("=1e3+A1", 0, 1), "=1e3+A2");
        assert_eq!(
            crate::refs::offset_refs("=IF(A1>0,\"A1\",B2)", 0, 1),
            "=IF(A2>0,\"A1\",B3)"
        );
        assert_eq!(crate::refs::offset_refs("1200", 3, 3), "1200");
    }

    {
        let (mut app, mut doc) = seeded_app();
        app.grid.select_range_public((4, 1), (6, 1)); // E2:G2, E2 = =SUM(B2:D2)
        app.fill_right(&mut Edits::lend(&mut doc));
        assert_eq!(doc_cell_raw(&doc, 0, (5, 1)), "=SUM(C2:E2)");
        assert_eq!(doc_cell_raw(&doc, 0, (6, 1)), "=SUM(D2:F2)");
        match value_at(&app, (5, 1)) {
            Some(CellValue::Number(n)) => assert!(n > 0.0, "filled formula recomputed to {n}"),
            other => panic!("expected numeric fill result, got {other:?}"),
        }
    }
}

#[test]
fn a_paste_is_one_change() {
    let _clipboard = CLIP_LOCK.lock_recover();
    let (mut app, mut doc) = seeded_app();
    app.grid.select_range_public((0, 1), (3, 3)); // A2:D4
    app.copy_selection(false);
    app.grid.select_only((0, 20));
    let before = doc.get_heads();
    app.paste(&mut Edits::lend(&mut doc));
    assert_eq!(doc_cell_raw(&doc, 0, (0, 20)), "Rent");
    assert_eq!(
        doc_cell_raw(&doc, 0, (3, 22)),
        doc_cell_raw(&doc, 0, (3, 3))
    );
    assert_eq!(
        doc.get_changes(&before).len(),
        1,
        "one change for twelve cells"
    );
}

#[test]
fn copy_cut_and_paste_relativise_formulas_correctly() {
    {
        let _clip = CLIP_LOCK.lock_recover();
        let (mut app, mut doc) = seeded_app();
        app.grid.select_only((1, 1)); // B2 = 1200, currency
        app.copy_selection(false);
        app.grid.select_only((1, 7)); // B8, empty
        app.paste(&mut Edits::lend(&mut doc));
        assert_eq!(doc_cell_raw(&doc, 0, (1, 7)), "1200");
        assert!(matches!(
            app.grid.cell_fmt_at((1, 7)),
            CellFormat::Currency(_)
        ));
        assert_eq!(value_at(&app, (1, 7)), Some(CellValue::Number(1200.0)));
    }

    {
        let _clip = CLIP_LOCK.lock_recover();
        let (mut app, mut doc) = seeded_app();
        app.grid.select_only((4, 1)); // E2
        app.copy_selection(false);
        app.grid.select_only((5, 2)); // F3
        app.paste(&mut Edits::lend(&mut doc));
        assert_eq!(doc_cell_raw(&doc, 0, (5, 2)), "=SUM(C3:E3)");
        assert_eq!(value_at(&app, (5, 2)), Some(CellValue::Number(2360.0)));
    }

    {
        let _clip = CLIP_LOCK.lock_recover();
        let (mut app, mut doc) = seeded_app();
        app.grid.select_only((0, 0)); // A1 = "Category"
        app.copy_selection(true); // cut
        app.grid.select_only((0, 20)); // A21, empty
        app.paste(&mut Edits::lend(&mut doc));
        assert_eq!(doc_cell_raw(&doc, 0, (0, 0)), "");
        assert_eq!(doc_cell_raw(&doc, 0, (0, 20)), "Category");
    }

    {
        let _clip = CLIP_LOCK.lock_recover();
        let (mut app, mut doc) = seeded_app();
        app.grid.select_only((4, 1)); // E2 = =SUM(B2:D2)
        app.copy_selection(true); // cut
        app.grid.select_only((6, 10)); // G11, empty, far away
        app.paste(&mut Edits::lend(&mut doc));
        assert_eq!(doc_cell_raw(&doc, 0, (6, 10)), "=SUM(B2:D2)");
        assert_eq!(doc_cell_raw(&doc, 0, (4, 1)), "", "source cleared");
    }

    {
        let _clip = CLIP_LOCK.lock_recover();
        let (mut app, mut doc) = seeded_app();
        app.grid.select_only((4, 1)); // E2 = =SUM(B2:D2)
        app.copy_selection(false);
        app.grid.select_range_public((5, 1), (5, 3));
        app.paste(&mut Edits::lend(&mut doc));
        assert_eq!(doc_cell_raw(&doc, 0, (5, 1)), "=SUM(C2:E2)");
        assert_eq!(doc_cell_raw(&doc, 0, (5, 2)), "=SUM(C3:E3)");
        assert_eq!(doc_cell_raw(&doc, 0, (5, 3)), "=SUM(C4:E4)");
    }

    {
        let (mut app, mut doc) = seeded_app();
        let target = (0, 40); // A41, empty region
        assert!(app.paste_tsv(&mut Edits::lend(&mut doc), target, "10\t20\n30\t40\n"));
        assert_eq!(doc_cell_raw(&doc, 0, (0, 40)), "10");
        assert_eq!(doc_cell_raw(&doc, 0, (1, 40)), "20");
        assert_eq!(doc_cell_raw(&doc, 0, (0, 41)), "30");
        assert_eq!(doc_cell_raw(&doc, 0, (1, 41)), "40");
        app.sync(&doc, &automerge::ROOT);
        assert_eq!(value_at(&app, (1, 41)), Some(CellValue::Number(40.0)));
        assert!(!app.paste_tsv(&mut Edits::lend(&mut doc), target, ""));
    }
}

#[test]
fn layout_reflows_and_keeps_controls_usable_at_large_text() {
    {
        let (mut app, _doc) = seeded_app();

        app.rect = Rect::new(0.0, 0.0, 1280.0, 720.0);
        app.relayout();
        let wide_grid_w = app.grid_rect.width();
        let wide_field_x1 = app.formula_field_rect.x1;
        assert!(
            (app.grid_rect.x1 - 1280.0).abs() < 0.5,
            "wide grid tracks rect"
        );

        app.rect = Rect::new(0.0, 0.0, 560.0, 720.0);
        app.relayout();
        assert!(
            (app.grid_rect.x1 - 560.0).abs() < 0.5,
            "narrow grid tracks the new rect"
        );
        assert!(
            app.grid_rect.width() < wide_grid_w,
            "grid reflowed narrower"
        );
        assert!(
            app.formula_field_rect.x1 < wide_field_x1,
            "formula field reflowed"
        );

        app.rect = Rect::new(0.0, 0.0, 1280.0, 720.0);
        app.relayout();
        assert!(
            (app.grid_rect.width() - wide_grid_w).abs() < 0.5,
            "reflows back to wide"
        );
    }
}

#[test]
fn tab_menu_renames_duplicates_deletes_moves_and_colours() {
    {
        let (mut app, mut doc) = seeded_app();
        app.begin_tab_rename(0);
        for ch in ["L", "e", "d", "g", "e", "r"] {
            assert!(feed(
                &mut app,
                &mut doc,
                &char_event(ch),
                Modifiers::default()
            ));
        }
        assert!(feed(
            &mut app,
            &mut doc,
            &named_event(winit::keyboard::NamedKey::Enter),
            Modifiers::default()
        ));
        assert_eq!(doc_sheet_names(&doc)[0], "Ledger");
    }

    {
        let (mut app, mut doc) = seeded_app();
        let before = doc_sheet_names(&doc);
        app.begin_tab_rename(0);
        assert!(feed(
            &mut app,
            &mut doc,
            &char_event("Discarded"),
            Modifiers::default()
        ));
        assert!(feed(
            &mut app,
            &mut doc,
            &named_event(winit::keyboard::NamedKey::Escape),
            Modifiers::default()
        ));
        assert!(app.tab_rename.is_none());
        assert_eq!(doc_sheet_names(&doc), before);
        assert_eq!(doc.pending_ops(), 0, "nothing was written");
    }

    {
        let (mut app, mut doc) = seeded_app();
        let before = doc_sheet_count(&doc);
        app.run_tab_action(&mut Edits::lend(&mut doc), 0, TabAction::Duplicate);
        assert_eq!(doc_sheet_count(&doc), before + 1);
        assert_eq!(doc_sheet_names(&doc)[1], "Budget copy");
        assert_eq!(doc_cell_raw(&doc, 1, (4, 1)), "=SUM(B2:D2)");
    }

    {
        let (mut app, mut doc) = seeded_app();
        app.run_tab_action(&mut Edits::lend(&mut doc), 1, TabAction::Delete);
        confirm(&mut app, &mut doc);
        assert_eq!(doc_sheet_names(&doc), vec!["Budget"]);
        app.run_tab_action(&mut Edits::lend(&mut doc), 0, TabAction::Delete);
        assert_eq!(doc_sheet_count(&doc), 1);
    }

    {
        let (mut app, mut doc) = seeded_app();
        app.run_tab_action(&mut Edits::lend(&mut doc), 0, TabAction::MoveRight);
        assert_eq!(doc_sheet_names(&doc), vec!["Sales", "Budget"]);
        assert_eq!(doc_cell_raw(&doc, 1, (4, 1)), "=SUM(B2:D2)");
    }

    {
        let (mut app, mut doc) = seeded_app();
        app.run_tab_action(&mut Edits::lend(&mut doc), 0, TabAction::SetColor(Some(2)));
        assert_eq!(doc_sheet_color(&doc, 0), Some(2));
        app.run_tab_action(&mut Edits::lend(&mut doc), 0, TabAction::SetColor(None));
        assert_eq!(doc_sheet_color(&doc, 0), None);
    }
}

#[test]
fn reload_reuses_unchanged_cache_and_invalidates_it_after_an_edit() {
    {
        let (mut app, mut doc) = seeded_app();
        doc.commit();
        app.sync(&doc, &automerge::ROOT);
        let cached = app
            .cache
            .as_ref()
            .expect("seed reload populates cache")
            .revision;

        app.pending.redraw = false;
        app.sync(&doc, &automerge::ROOT);
        assert!(
            !app.pending.redraw,
            "unchanged reload should return before rebuilding"
        );
        assert_eq!(app.cache.as_ref().unwrap().revision, cached);

        app.write_raw(
            &mut Edits::lend(&mut doc),
            vec![((0, 0), "1234".to_owned(), None)],
        );
        app.pending.redraw = false;
        app.sync(&doc, &automerge::ROOT);

        let refreshed = app
            .cache
            .as_ref()
            .expect("edited reload refreshes cache")
            .revision;
        assert_ne!(refreshed, cached, "an edit invalidates the cached state");
        assert_eq!(value_at(&app, (0, 0)), Some(CellValue::Number(1234.0)));
        assert!(app.pending.redraw, "rebuilt grid must request a redraw");
    }
}

#[test]
fn name_box_parses_and_navigates_references() {
    {
        assert_eq!(parse_ref_target("b7"), Some(((1, 6), (1, 6))));
        assert_eq!(parse_ref_target("$A$1"), Some(((0, 0), (0, 0))));
        assert_eq!(parse_ref_target("A1:C3"), Some(((0, 0), (2, 2))));
        assert_eq!(parse_ref_target("not-a-ref"), None);
        assert_eq!(parse_ref_target("A1:"), None);
        assert_eq!(parse_ref_target(""), None);
    }

    {
        let (mut app, mut doc) = seeded_app();
        app.grid.select_only((0, 0));
        app.begin_name_box_edit(); // seeds "A1", fully selected
        assert!(app.name_box_edit.is_some());
        assert!(feed(
            &mut app,
            &mut doc,
            &char_event(" C 5 "),
            Modifiers::default()
        ));
        assert_eq!(
            app.name_box_edit.as_ref().map(TextInput::value),
            Some("C5"),
            "the reference field drops whitespace before domain parsing"
        );
        assert!(feed(
            &mut app,
            &mut doc,
            &named_event(winit::keyboard::NamedKey::Enter),
            Modifiers::default()
        ));
        assert!(app.name_box_edit.is_none());
        assert_eq!(app.grid.active(), (2, 4));
        assert!(app.status.contains("Went to"), "status={}", app.status);
    }

    {
        let (mut app, mut doc) = seeded_app();
        app.begin_name_box_edit();
        assert!(feed(
            &mut app,
            &mut doc,
            &char_event("B2:D4"),
            Modifiers::default()
        ));
        assert!(feed(
            &mut app,
            &mut doc,
            &named_event(winit::keyboard::NamedKey::Enter),
            Modifiers::default()
        ));
        let (tl, br) = app.grid.selection_bounds();
        assert_eq!(tl, (1, 1));
        assert_eq!(br, (3, 3));
    }

    {
        let (mut app, mut doc) = seeded_app();
        app.grid.select_only((4, 1)); // E2
        app.begin_name_box_edit();
        assert!(feed(
            &mut app,
            &mut doc,
            &char_event("nope!"),
            Modifiers::default()
        ));
        assert!(feed(
            &mut app,
            &mut doc,
            &named_event(winit::keyboard::NamedKey::Enter),
            Modifiers::default()
        ));
        assert_eq!(app.grid.active(), (4, 1), "selection must not move");
        assert!(app.status.contains("Invalid"), "status={}", app.status);
    }

    {
        let (mut app, _doc) = seeded_app();
        app.grid.select_only((4, 1));
        app.begin_name_box_edit();
        if let Some(input) = &mut app.name_box_edit {
            input.set_text("");
        }
        app.commit_name_box();
        assert!(app.name_box_edit.is_none());
        assert_eq!(app.grid.active(), (4, 1));
    }
}

pub(super) fn ctrl_mods() -> Modifiers {
    let mut s = winit::keyboard::ModifiersState::empty();
    s.set(winit::keyboard::ModifiersState::CONTROL, true);
    Modifiers::from(s)
}

fn key_event(logical: winit::keyboard::Key) -> WindowEvent {
    ui::synthetic_input::keyboard_input(ui::synthetic_input::key_event(
        logical,
        None,
        ElementState::Pressed,
    ))
}

pub(super) fn char_event(s: &str) -> WindowEvent {
    let mut event = key_event(winit::keyboard::Key::Character(
        winit::keyboard::SmolStr::new(s),
    ));
    if let WindowEvent::KeyboardInput { event: key, .. } = &mut event {
        key.text = Some(winit::keyboard::SmolStr::new(s));
    }
    event
}

pub(super) fn named_event(k: winit::keyboard::NamedKey) -> WindowEvent {
    key_event(winit::keyboard::Key::Named(k))
}

pub(super) fn feed(
    app: &mut SheetsApp,
    doc: &mut AutoCommit,
    ev: &WindowEvent,
    mods: Modifiers,
) -> bool {
    ui::testing::Dispatch::new()
        .modifiers(mods)
        .doc(doc)
        .send(app, ev)
        .handled
}

#[test]
fn cell_edit_composes_with_the_input_method_and_deletes_whole_clusters() {
    use winit::event::Ime;
    use winit::keyboard::NamedKey;
    let (mut app, mut doc) = seeded_app();
    app.grid.select_only((0, 20)); // A21, empty
    assert!(feed(
        &mut app,
        &mut doc,
        &char_event("a"),
        Modifiers::default()
    ));
    assert!(app.grid.is_editing());
    assert_eq!(
        app.ime_content(),
        Some(ui::ime::InputContent::TEXT),
        "the editing cell asks for the input method"
    );
    assert!(app.ime_cursor_rect().is_some());

    let ime = |app: &mut SheetsApp, doc: &mut AutoCommit, ime: Ime| {
        feed(app, doc, &WindowEvent::Ime(ime), Modifiers::default())
    };
    assert!(ime(
        &mut app,
        &mut doc,
        Ime::Preedit("にほ".into(), Some((3, 3)))
    ));
    assert_eq!(
        app.grid.edit_shown(),
        Some(("aにほ".to_owned(), 2)),
        "the composition shows at the caret, the caret inside it"
    );
    assert_eq!(app.grid.active_display_raw(), "a", "but is not typed yet");
    assert!(ime(&mut app, &mut doc, Ime::Commit("日本".into())));
    assert_eq!(app.grid.active_display_raw(), "a日本");

    for typed in ["e\u{301}", "👍🏽", "🇩🇪"] {
        assert!(ime(&mut app, &mut doc, Ime::Commit(typed.into())));
    }
    assert_eq!(app.grid.active_display_raw(), "a日本e\u{301}👍🏽🇩🇪");
    let backspace = named_event(NamedKey::Backspace);
    assert!(feed(&mut app, &mut doc, &backspace, Modifiers::default()));
    assert_eq!(app.grid.active_display_raw(), "a日本e\u{301}👍🏽");
    assert!(feed(&mut app, &mut doc, &backspace, Modifiers::default()));
    assert!(feed(&mut app, &mut doc, &backspace, Modifiers::default()));
    assert_eq!(app.grid.active_display_raw(), "a日本");

    app.grid.set_edit_caret(1, false);
    app.grid.set_edit_caret(3, true);
    assert!(ime(&mut app, &mut doc, Ime::Commit("x".into())));
    assert_eq!(app.grid.active_display_raw(), "ax");
}

#[test]
fn formula_bar_shows_the_cells_composition() {
    use winit::event::Ime;
    let (mut app, mut doc) = seeded_app();
    app.rect = Rect::new(0.0, 0.0, 1280.0, 720.0);
    app.relayout();
    let theme = Theme::dark();
    let mut tr = TextRenderer::new();
    app.refresh_chrome_metrics(&mut tr, &theme);
    app.grid.select_only((0, 20)); // A21, empty
    for typed in ["a", "b"] {
        assert!(feed(
            &mut app,
            &mut doc,
            &char_event(typed),
            Modifiers::default()
        ));
    }
    app.grid.set_edit_caret(1, false);
    let ime = WindowEvent::Ime(Ime::Preedit("にほ".into(), Some((3, 3))));
    assert!(feed(&mut app, &mut doc, &ime, Modifiers::default()));

    let bar = app.grid.formula_bar();
    assert_eq!(bar.text, "aにほb", "the bar shows the composition in place");
    assert_eq!(bar.preedit, Some((1, 3)), "underlined over the composition");
    assert_eq!(bar.caret, Some(2), "the caret sits inside it");
    assert_eq!(app.grid.active_display_raw(), "ab", "nothing is typed yet");

    let mut scene = Scene::new();
    app.draw_formula_field(&mut scene, Affine::IDENTITY, &mut tr, &theme);
    assert_eq!(
        app.formula_char_x.len(),
        "aにほb".chars().count() + 1,
        "the bar laid out the composed text"
    );
    assert!(
        app.formula_char_x[3] > app.formula_char_x[1] + 1.0,
        "the committed 'b' draws after the composition"
    );

    app.grid.set_edit_caret_shown(4, false);
    assert_eq!(app.grid.edit_caret(), Some(2), "after the 'b' of \"ab\"");
    app.grid.set_edit_caret(1, false);

    let commit = WindowEvent::Ime(Ime::Commit("日本".into()));
    assert!(feed(&mut app, &mut doc, &commit, Modifiers::default()));
    assert_eq!(app.grid.active_display_raw(), "a日本b");
    let bar = app.grid.formula_bar();
    assert_eq!(bar.text, "a日本b");
    assert_eq!((bar.preedit, bar.caret), (None, Some(3)));
}

#[test]
fn commands_open_the_hosts_search_and_go_to_cells() {
    let (mut app, _doc) = seeded_app();
    app.open_command_palette();
    assert_eq!(
        asked_search(&mut app),
        ui::search::SearchRequest::this_app()
    );

    assert_ne!(app.grid.active(), (1, 11));
    let resources = app.published();
    let resource = tilo_actions::flatten(&resources)
        .into_iter()
        .find(|flat| flat.action.id == "go-to-cell")
        .expect("the workbook publishes Go to cell")
        .resource_id
        .0;
    let spec =
        tilo_actions::door::spec_of(&resources, &resource, "go-to-cell").expect("it resolves");
    assert_eq!(spec.kind, tilo_actions::OperationKind::View);
    let encoded = spec
        .build_json(&prompt_arguments(spec, "B12"))
        .expect("valid arguments");
    let effect =
        <actions::SheetsEffect as ui::tilo_resource::Effect>::from_payload(&encoded.payload)
            .expect("a workbook effect");
    app.show(effect).expect("the view is shown");
    assert_eq!(app.grid.active(), (1, 11), "selection jumped to B12");

    app.go_to_cell("not-a-ref");
    assert!(app.status.contains("not a valid"), "status: {}", app.status);
}

fn asked_search(app: &mut SheetsApp) -> ui::search::SearchRequest {
    let invocation = ui::widget::controls::take_control_invocation(app)
        .expect("the app asked the host to open search");
    ui::search::SearchRequest::from_arguments(&invocation.arguments)
}

fn prompt_arguments(
    spec: &tilo_actions::ActionSpec,
    text: &str,
) -> serde_json::Map<String, serde_json::Value> {
    let mut arguments = serde_json::Map::new();
    arguments.insert(
        spec.prompt_param().expect("a prompted action").to_owned(),
        text.into(),
    );
    arguments
}

fn answer_asked(app: &mut SheetsApp, doc: &mut AutoCommit, prefill: Option<&str>, text: &str) {
    let request = asked_search(app);
    assert_eq!(request.query.as_deref(), prefill);
    let address = request.asked().expect("the palette asks for an action");
    let resources = app.published();
    let spec = tilo_actions::door::resolve(&resources, &address).expect("the address resolves");
    let encoded = spec
        .build_json(&prompt_arguments(spec, text))
        .expect("the arguments build");
    let action: <Workbook as ui::tilo_resource::Resource>::Action =
        serde_json::from_slice(&encoded.payload).expect("a workbook action");
    let mut book = ui::tilo_resource::Replica::<Workbook>::from_doc(doc.clone());
    book.apply_action(&action).expect("the action applies");
    *doc = book.doc().clone();
    app.sync(doc, &automerge::ROOT);
}

#[test]
fn annotation_edits_ask_for_the_published_actions() {
    let (mut app, mut doc) = seeded_app();
    let sheet = app.sheet_ids[0].clone();
    let mut book = ui::tilo_resource::Replica::<Workbook>::from_doc(doc.clone());
    book.apply(Workbook::ADD_CHART.with(verbs::AddChart {
        sheet: sheet.clone(),
        range: "B2:C4".into(),
        kind: ChartKind::Column,
        title: "Spend".into(),
    }))
    .unwrap();
    book.apply(Workbook::COMMENT.with(verbs::Comment {
        sheet: sheet.clone(),
        range: "B2".into(),
        text: "Check this".into(),
    }))
    .unwrap();
    doc = book.doc().clone();
    app.sync(&doc, &automerge::ROOT);
    let state = doc_workbook(&doc);
    let chart = state
        .sheets
        .get(&sheet)
        .unwrap()
        .charts
        .ids()
        .next()
        .unwrap()
        .clone();
    let comment = state.comments.ids().next().unwrap().clone();

    app.apply_annotation_click(
        AnnotationClick::EditChart(chart.as_str().to_owned()),
        &mut Edits::lend(&mut doc),
    );
    answer_asked(&mut app, &mut doc, Some("Spend"), "Costs");
    let charted = doc_workbook(&doc);
    let edited = charted
        .sheets
        .get(&sheet)
        .unwrap()
        .charts
        .get(&chart)
        .unwrap();
    assert_eq!(edited.title.0, "Costs");
    assert_eq!(edited.kind, ChartKind::Column, "the type is kept");

    app.apply_annotation_click(
        AnnotationClick::Reply(comment.as_str().to_owned()),
        &mut Edits::lend(&mut doc),
    );
    answer_asked(&mut app, &mut doc, None, "Done");

    app.apply_annotation_click(
        AnnotationClick::EditComment(comment.as_str().to_owned()),
        &mut Edits::lend(&mut doc),
    );
    answer_asked(&mut app, &mut doc, Some("Check this"), "Checked");

    let state = doc_workbook(&doc);
    let thread = state.comments.get(&comment).unwrap();
    assert_eq!(thread.text, "Checked");
    let replies: Vec<&str> = thread
        .replies
        .values()
        .map(|reply| reply.text.as_str())
        .collect();
    assert_eq!(replies, ["Done"]);
}

#[test]
fn a_chart_card_changes_its_type_and_cells() {
    let (mut app, mut doc, mut tr, theme) = measured_app_with_doc(1800.0, 1200.0, 1.0);
    let sheet = app.sheet_ids[0].clone();
    let mut book = ui::tilo_resource::Replica::<Workbook>::from_doc(doc.clone());
    book.apply(Workbook::ADD_CHART.with(verbs::AddChart {
        sheet: sheet.clone(),
        range: "B2:C4".into(),
        kind: ChartKind::Column,
        title: "Spend".into(),
    }))
    .unwrap();
    doc = book.doc().clone();
    app.sync(&doc, &automerge::ROOT);
    paint_frame(&mut app, &mut tr, &theme);
    let chart = doc_workbook(&doc)
        .sheets
        .get(&sheet)
        .unwrap()
        .charts
        .ids()
        .next()
        .unwrap()
        .clone();

    let manage = app
        .annotation_clicks
        .iter()
        .find(|(_, action)| matches!(action, AnnotationClick::ManageChart(_)))
        .map(|(rect, _)| rect.center())
        .expect("the header offers chart management");
    assert!(app.try_annotation_click(manage, &mut Edits::lend(&mut doc), &mut false));
    paint_frame(&mut app, &mut tr, &theme);
    let line = app
        .annotation_clicks
        .iter()
        .find(|(_, action)| matches!(action, AnnotationClick::SetChartKind(_, ChartKind::Line)))
        .map(|(rect, _)| rect.center())
        .expect("the card offers a line chart");
    let mut handled = false;
    assert!(app.try_annotation_click(line, &mut Edits::lend(&mut doc), &mut handled));
    let kind_of = |doc: &AutoCommit| {
        doc_workbook(doc)
            .sheets
            .get(&sheet)
            .unwrap()
            .charts
            .get(&chart)
            .unwrap()
            .kind
    };
    assert_eq!(kind_of(&doc), ChartKind::Line);

    let column = format!("sheets.annotation:chart:{}:column", chart.as_str());
    assert!(matches!(
        app.annotation_click_from_target(&column),
        Some(AnnotationClick::SetChartKind(_, ChartKind::Column))
    ));

    paint_frame(&mut app, &mut tr, &theme);
    let cells = app
        .annotation_clicks
        .iter()
        .find(|(_, action)| matches!(action, AnnotationClick::ChartCells(_)))
        .map(|(rect, _)| rect.center())
        .expect("the card offers its cells");
    assert!(app.try_annotation_click(cells, &mut Edits::lend(&mut doc), &mut handled));
    answer_asked(&mut app, &mut doc, Some("B2:C4"), "A1:D3");
    let state = doc_workbook(&doc);
    let sheet_state = state.sheets.get(&sheet).unwrap();
    let shown = sheet_state
        .chart_snapshots(&sheet_state.layout())
        .into_iter()
        .find(|snapshot| snapshot.id == chart.as_str())
        .unwrap();
    assert_eq!((shown.start, shown.end), ((0, 0), (3, 2)));
    assert_eq!(shown.kind, ChartKind::Line, "the type is kept");

    paint_frame(&mut app, &mut tr, &theme);
    assert_eq!(
        app.annotation_clicks.len(),
        7,
        "six commands and the management toggle"
    );
    assert!(app.try_annotation_click(manage, &mut Edits::lend(&mut doc), &mut handled));
    paint_frame(&mut app, &mut tr, &theme);
    assert_eq!(
        app.annotation_clicks.len(),
        1,
        "closing management restores the plot"
    );
    assert!(app.managed_chart.is_none());
}

#[test]
fn an_agent_edits_charts_and_comments_in_one_call_each() {
    let mut app = TestApp::<crate::resource::SheetsShell>::new().unwrap();
    let sheet = app.state().sheets.ids().next().unwrap().clone();
    app.invoke(Workbook::ADD_CHART.with(verbs::AddChart {
        sheet: sheet.clone(),
        range: "A1:B3".into(),
        kind: ChartKind::Column,
        title: "Spend".into(),
    }))
    .unwrap();
    app.invoke(Workbook::COMMENT.with(verbs::Comment {
        sheet: sheet.clone(),
        range: "A1".into(),
        text: "Check this".into(),
    }))
    .unwrap();
    let state = app.state();
    let chart = state
        .sheets
        .get(&sheet)
        .unwrap()
        .charts
        .ids()
        .next()
        .unwrap()
        .clone();
    let comment = state.comments.ids().next().unwrap().clone();
    let on_sheet = ui::tilo_resource::items::item_resource_id::<Workbook>("sheets", &sheet);
    let on_comment = ui::tilo_resource::items::item_resource_id::<Workbook>("comments", &comment);

    app.call_on(
        &on_sheet,
        "edit-chart",
        serde_json::json!({"chart": chart, "title": "Costs", "kind": "line", "range": "B2:C5"}),
    )
    .unwrap();
    assert!(
        app.call_on(
            &on_sheet,
            "edit-chart",
            serde_json::json!({"chart": chart, "title": "Costs", "range": "B2:"}),
        )
        .is_err(),
        "a range is cells"
    );
    app.call_on(
        &on_comment,
        "edit-comment",
        serde_json::json!({"body": "Checked"}),
    )
    .unwrap();
    app.call_on(&on_comment, "reply", serde_json::json!({"text": "Done"}))
        .unwrap();

    let state = app.state();
    let sheet_state = state.sheets.get(&sheet).unwrap();
    let edited = sheet_state.charts.get(&chart).unwrap();
    assert_eq!(
        (edited.title.0.as_str(), edited.kind),
        ("Costs", ChartKind::Line)
    );
    let layout = sheet_state.layout();
    let snapshot = sheet_state
        .chart_snapshots(&layout)
        .into_iter()
        .find(|snapshot| snapshot.id == chart.as_str())
        .unwrap();
    assert_eq!((snapshot.start, snapshot.end), ((1, 1), (2, 4)));
    let thread = state.comments.get(&comment).unwrap();
    assert_eq!(thread.text, "Checked");
    assert_eq!(thread.replies.len(), 1);
}

#[test]
fn commands_edit_rows_columns_views_and_sheets() {
    {
        let (mut app, mut doc) = seeded_app();
        app.grid.select_range_public((1, 1), (1, 5));
        app.dispatch_command(&mut Edits::lend(&mut doc), "sum_selection", None);
        let raw = doc_cell_raw(&doc, 0, (1, 6));
        assert_eq!(raw, "=SUM(B2:B6)", "sum total written below the selection");
        assert!(matches!(value_at(&app, (1, 6)), Some(CellValue::Number(_))));
    }

    {
        let (mut app, mut doc) = seeded_app();
        let b2 = doc_cell_raw(&doc, 0, (1, 1));
        assert!(!b2.is_empty());
        app.grid.select_only((0, 0)); // A1 → insert above row 0
        app.dispatch_command(&mut Edits::lend(&mut doc), "insert_row", None);
        assert_eq!(doc_cell_raw(&doc, 0, (0, 0)), "", "row 0 is now blank");
        assert_eq!(
            doc_cell_raw(&doc, 0, (0, 1)),
            "Category",
            "header shifted to A2"
        );

        app.grid.select_only((0, 0));
        app.dispatch_command(&mut Edits::lend(&mut doc), "delete_row", None);
        confirm(&mut app, &mut doc);
        assert_eq!(
            doc_cell_raw(&doc, 0, (0, 0)),
            "Category",
            "header back at A1"
        );
    }

    {
        let (mut app, mut doc) = seeded_app();
        let a1 = doc_cell_raw(&doc, 0, (0, 0));
        assert_eq!(a1, "Category");
        app.grid.select_only((0, 0)); // insert left of column A
        app.dispatch_command(&mut Edits::lend(&mut doc), "insert_col", None);
        assert_eq!(doc_cell_raw(&doc, 0, (0, 0)), "", "column A is now blank");
        assert_eq!(
            doc_cell_raw(&doc, 0, (1, 0)),
            "Category",
            "header shifted to B1"
        );
    }

    {
        let (mut app, mut doc) = seeded_app();
        assert!(!app.grid.formula_view());
        app.dispatch_command(&mut Edits::lend(&mut doc), "formula_view", None);
        assert!(app.grid.formula_view(), "formula view turned on");
        app.dispatch_command(&mut Edits::lend(&mut doc), "formula_view", None);
        assert!(!app.grid.formula_view(), "formula view toggled back off");
    }

    {
        let (mut app, mut doc) = seeded_app();
        let before = doc_sheet_count(&doc);
        app.dispatch_command(&mut Edits::lend(&mut doc), "new_sheet", None);
        assert_eq!(doc_sheet_count(&doc), before + 1);
    }
}

#[test]
fn accessibility_exposes_cells_busy_state_and_live_controls() {
    {
        use ui::widget::Widget as _;

        let (mut app, _doc) = seeded_app();
        app.grid.select_only((1, 1));
        let tree = app.accessibility().expect("spreadsheet semantics");
        assert_eq!(tree.role, ui::a11y::Role::Document);
        let toolbar = &tree.children[0];
        assert_eq!(toolbar.role, ui::a11y::Role::Toolbar);
        assert!(toolbar.children.iter().any(|control| {
            control.name.as_deref() == Some("Name box") && control.action_target.is_some()
        }));
        let tabs = &tree.children[1];
        assert_eq!(tabs.role, ui::a11y::Role::TabBar);
        assert!(tabs.children.iter().any(|tab| tab.state.selected));
        assert!(tabs.children.iter().all(|tab| tab.action_target.is_some()));
        let cell = &tree.children[2].children[0].children[0];
        assert_eq!(cell.name.as_deref(), Some("Column A"));
        let visible_row = tree.children[2]
            .children
            .iter()
            .find(|row| row.name.as_deref() == Some("Row 2"))
            .expect("visible second worksheet row");
        let cell = visible_row
            .children
            .iter()
            .find(|cell| cell.name.as_deref() == Some("B2"))
            .expect("active cell in complete visible table semantics");
        assert!(cell.state.selected && cell.state.focused);
        assert!(cell.bounds.is_some());
    }

    {
        use ui::widget::Widget as _;

        let (mut app, mut doc) = seeded_app();
        app.rect = Rect::new(0.0, 0.0, 1280.0, 720.0);
        app.relayout();
        let root = automerge::ROOT;
        app.settle(&mut Edits::lend(&mut doc), &root);
        let formulas = app
            .window_bar()
            .actions
            .into_iter()
            .find(|action| action.name == "Show formulas")
            .expect("the formula view's toggle");
        ui::window_toolbar::deliver(ui::window_toolbar::ToolbarEvent::Action { id: formulas.id });
        app.settle(&mut Edits::lend(&mut doc), &root);
        assert!(app.grid.formula_view());
        assert!(app.perform_semantic_action(
            "sheets.name-box",
            &ui::a11y::Action::Focus,
            &mut Edits::lend(&mut doc),
            &root,
        ));
        assert!(app.name_box_edit.is_some());
        let target = format!("sheets.tab:{}", app.sheet_ids[1].as_str());
        assert!(app.perform_semantic_action(
            &target,
            &ui::a11y::Action::Activate,
            &mut Edits::lend(&mut doc),
            &root,
        ));
        assert_eq!(app.active_sheet, 1);
    }

    {
        use ui::widget::controls::ActionControls as _;

        let (app, _tr, _theme) = measured_app(1280.0, 720.0, 1.0);
        let bound: std::collections::HashSet<_> = app
            .control_action_bindings()
            .into_iter()
            .map(|binding| binding.control_id)
            .collect();
        for target in [
            "sheets.fx",
            "sheets.name-box",
            "sheets.formula-bar",
            "sheets.new-sheet",
        ] {
            assert!(
                bound.contains(target),
                "{target} has no action binding: {bound:?}"
            );
        }
    }
}

#[test]
fn a_notion_export_lands_as_every_table_in_one_undoable_step() {
    use std::io::Write as _;
    let mut buffer = std::io::Cursor::new(Vec::new());
    {
        let mut writer = zip::ZipWriter::new(&mut buffer);
        let options = zip::write::SimpleFileOptions::default()
            .compression_method(zip::CompressionMethod::Stored);
        for (name, text) in [
            ("Tasks 1a2b3c4d5e_all.csv", "Name,Done\nShip,yes\n"),
            ("People ef60718293.csv", "Name,Role\nSam,Design\n"),
        ] {
            writer.start_file(name, options).unwrap();
            writer.write_all(text.as_bytes()).unwrap();
        }
        writer.finish().unwrap();
    }
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("Notion Export.zip");
    std::fs::write(&path, buffer.into_inner()).unwrap();

    let mut app = sample_app();
    let before = app.state().sheets.len();
    app.effect(&SheetsEffect::ImportNotion {
        path: Some(path.as_path().try_into().unwrap()),
    })
    .unwrap();
    app.settle();
    assert_eq!(app.state().sheets.len(), before + 2, "both databases");
    assert_eq!(
        app.state()
            .sheets
            .values()
            .nth(before)
            .map(|sheet| sheet.name.0.as_str()),
        Some("People"),
        "the export's names are kept, in name order"
    );
    assert_eq!(raw_at(&app, before, (0, 0)), "Name");
    assert_eq!(raw_at(&app, before + 1, (1, 1)), "yes");

    assert!(app.undo(), "the import is one undo step");
    assert_eq!(
        app.state().sheets.len(),
        before,
        "one undo takes back both sheets"
    );
}

#[test]
fn a_notion_export_with_no_table_fails_its_import() {
    use std::io::Write as _;
    let mut buffer = std::io::Cursor::new(Vec::new());
    {
        let mut writer = zip::ZipWriter::new(&mut buffer);
        let options = zip::write::SimpleFileOptions::default()
            .compression_method(zip::CompressionMethod::Stored);
        writer.start_file("Notes 1a2b3c4d5e.md", options).unwrap();
        writer.write_all(b"# Notes\n").unwrap();
        writer.finish().unwrap();
    }
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("No tables.zip");
    std::fs::write(&path, buffer.into_inner()).unwrap();

    let mut app = sample_app();
    let before = app.state().sheets.len();
    let error = app
        .effect(&SheetsEffect::ImportNotion {
            path: Some(path.as_path().try_into().unwrap()),
        })
        .expect_err("an export with no table fails");
    assert!(
        matches!(error, tilo_actions::DoorError::Failed(_)),
        "{error:?}"
    );
    assert_eq!(app.state().sheets.len(), before, "nothing was added");
}

#[test]
fn a_relative_path_is_refused_for_every_path_action() {
    use ui::tilo_resource::Effect as _;
    for id in [
        "import-csv",
        "import-xlsx",
        "import-notion",
        "import-ods",
        "export-csv",
        "export-xlsx",
        "export-ods",
    ] {
        let mut arguments = serde_json::Map::new();
        arguments.insert("path".into(), "Budget.csv".into());
        let error = SheetsEffect::decode(id, &arguments).expect_err(id);
        assert!(
            matches!(
                &error,
                ui::tilo_resource::EffectDecodeError::Argument { name, source }
                    if name == "path" && source.to_string().contains("absolute")
            ),
            "{id}: {error}"
        );
    }
}
