#![allow(
    clippy::arithmetic_side_effects,
    clippy::cast_possible_truncation,
    clippy::drop_non_drop,
    clippy::expect_used,
    clippy::indexing_slicing,
    clippy::unreadable_literal,
    clippy::unwrap_used,
    reason = "migration stack lint debt is scoped to this target"
)]

//! Real native shared History qualification fixture. --plan emits measured
//! `FrameHarness` coordinates for the same retained tile at an800x600 viewport.
//! Native mode uses the ordinary scoped factory and shared scheduled input.
use automerge::{AutoCommit, ROOT, ReadDoc, transaction::Transactable};
use std::{cell::RefCell, collections::HashMap};
use taffy::{NodeId, TaffyTree};
use tilo_ui::TextRole;
use tilo_ui::{
    State,
    history_mode::{
        HistoryDefinition, HistoryPresentation, HistoryPreview, HistoryScopeDefinition,
        PreviewAccessibility, PreviewInput, PreviewNavigation, PreviewView, restore,
    },
    widget::{AppKind, AppWidget, Column, IconDescriptor, Text, TileContent, TileHost, Widget},
};
use vello::{
    Scene,
    kurbo::{Affine, Point, Rect},
};
use winit::event::WindowEvent;
const APP: &str = "dev.tilo.managed-history-probe";
static SCHEMA: restore::HistoryRestoreSchema = restore::HistoryRestoreSchema {
    id: "native-history-note",
    version: 1,
    roots: &[restore::HistoryRestoreRoot {
        key: "body",
        required: true,
        expected_type: restore::RestoreValueType::Scalar(restore::ScalarType::String),
    }],
    selectors: &[],
    preserved_roots: &[],
    preserved: &[],
    validate: |_| Ok(()),
    validate_result: |_| Ok(()),
};
static HISTORY: HistoryDefinition = HistoryDefinition {
    app_id: APP,
    scopes: &[HistoryScopeDefinition {
        key: "note",
        title: "Native notebook",
        presentation: HistoryPresentation::Domain { create: preview },
        content_schema: Some(&SCHEMA),
    }],
};
thread_local! { static PREVIEW: RefCell<serde_json::Value> = const { RefCell::new(serde_json::Value::Null) }; }
struct Preview {
    body: String,
    view: PreviewView,
    rect: Rect,
    observation: serde_json::Value,
}
fn committed_change_count(doc: &AutoCommit) -> usize {
    let mut clone = doc.clone();
    clone.rollback();
    clone.get_changes(&[]).len()
}
fn body(doc: &AutoCommit) -> String {
    doc.get(ROOT, "body")
        .unwrap()
        .and_then(|(value, _)| value.as_str().map(str::to_owned))
        .unwrap_or_default()
}
fn preview(input: PreviewInput) -> Result<Box<dyn HistoryPreview>, String> {
    let doc = AutoCommit::load(input.snapshot.bytes()).map_err(|error| error.to_string())?;
    let body = body(&doc);
    let observation = serde_json::json!({"body": body, "side": format!("{:?}", input.snapshot.side()), "heads": input.snapshot.heads().iter().map(ToString::to_string).collect::<Vec<_>>() });
    Ok(Box::new(Preview {
        body,
        view: input.initial_view,
        rect: Rect::ZERO,
        observation,
    }))
}
impl HistoryPreview for Preview {
    fn layout(
        &mut self,
        rect: Rect,
        _: &mut tilo_ui::TextRenderer,
        _: &tilo_ui::Theme,
    ) -> Result<(), String> {
        self.rect = rect;
        Ok(())
    }
    fn draw(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        text: &mut tilo_ui::TextRenderer,
        theme: &tilo_ui::Theme,
    ) {
        PREVIEW.with_borrow_mut(|value| *value = self.observation.clone());
        text.draw_text(
            scene,
            &format!("Historical notebook\n{}", self.body),
            24.0,
            theme.text_primary,
            transform * Affine::translate((self.rect.x0 + 24.0, self.rect.y0 + 36.0)),
            Some((self.rect.width() - 48.0) as f32),
        );
    }
    fn navigate(&mut self, _: PreviewNavigation) -> bool {
        false
    }
    fn view(&self) -> PreviewView {
        self.view.clone()
    }
    fn accessibility(&self) -> PreviewAccessibility {
        PreviewAccessibility {
            description: format!("Historical notebook: {}", self.body),
            pages: vec![],
        }
    }
}
struct Note(Column);
impl AppWidget for Note {
    fn app_kind(&self) -> AppKind {
        AppKind::Editor
    }
    fn app_title(&self) -> &'static str {
        "History probe"
    }
    fn app_icon(&self) -> IconDescriptor {
        IconDescriptor::Glyph("N")
    }
}
impl Widget for Note {
    fn name(&self) -> &'static str {
        "Native notebook"
    }
    fn children(&self) -> Vec<&dyn Widget> {
        vec![&self.0]
    }
    fn children_mut(&mut self) -> Vec<&mut dyn Widget> {
        vec![&mut self.0]
    }
    fn build_layout(
        &mut self,
        tree: &mut TaffyTree<()>,
        theme: &tilo_ui::Theme,
        nodes: &mut HashMap<NodeId, *mut dyn Widget>,
    ) -> NodeId {
        let child = self.0.build_layout(tree, theme, nodes);
        tree.new_with_children(taffy::Style::default(), &[child])
            .unwrap()
    }
    fn apply_layout(&mut self, tree: &TaffyTree<()>, node: NodeId, origin: Point) {
        let pos = tree.layout(node).unwrap().location;
        self.0.apply_layout(
            tree,
            tree.children(node).unwrap()[0],
            Point::new(origin.x + f64::from(pos.x), origin.y + f64::from(pos.y)),
        );
    }
    fn get_rect(&self) -> Rect {
        self.0.get_rect()
    }
    fn draw(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        text: &mut tilo_ui::TextRenderer,
        theme: &tilo_ui::Theme,
        presence: &tilo_ui::presence::PresenceCtx,
    ) {
        self.0.draw(scene, transform, text, theme, presence);
    }
    fn handle_event(&mut self, event: &WindowEvent, ctx: &mut tilo_ui::widget::EventCtx<'_>) {
        self.0.handle_event(
            event,
            &mut ctx.reborrow(ctx.path, ctx.taffy.children(ctx.node_id).unwrap()[0]),
        );
    }
}
fn fixture() -> (
    tempfile::TempDir,
    tilo_ui::RecordStore,
    tilo_ui::record::RecordCheckout,
    Box<dyn Widget>,
) {
    let directory = tempfile::tempdir().unwrap();
    let mut owners = tilo_ui::record::RecordOwners::new(directory.path().to_path_buf());
    let store = owners.open(APP, "native-probe").unwrap();
    let checkout = tilo_ui::record::RecordCheckout::new(
        tilo_ui::RecordId::for_tab(70001),
        tilo_ui::record::RecordKind::new("note"),
    );
    store
        .reserve_record(&checkout)
        .unwrap()
        .initialize(|doc| {
            doc.put(ROOT, "body", "Original notebook text").unwrap();
            Ok(())
        })
        .unwrap();
    store
        .with_history(&checkout.id, |doc, history| {
            doc.put(ROOT, "body", "Current notebook text").unwrap();
            history.commit_action_authored_with_message(
                doc,
                None,
                None,
                Some(tilo_ui::collaboration::local_user_id()),
                Some("Edit notebook".into()),
            );
        })
        .unwrap();
    let note = Note(
        Column::new()
            .push(Box::new(
                Text::new("Shared native History probe").with_role(TextRole::LargeTitle),
            ))
            .push(Box::new(
                Text::from_document(|doc, _| format!("Live document: {}", body(doc)))
                    .with_role(TextRole::LargeTitle),
            )),
    );
    let mut tile = TileHost::new(TileContent::App(Box::new(note)));
    tile.bind_record(store.clone(), checkout.clone());
    tile.register_document_history(&HISTORY).unwrap();
    (directory, store, checkout, Box::new(tile))
}
fn main() {
    tilo_ui::i18n::I18nManager::init("en-US");
    let (_directory, store, checkout, root) = fixture();
    if std::env::args().any(|arg| arg == "--plan") {
        let mut driver = tilo_ui::headless::FrameHarness::new(State::new(), root, (800, 600));
        let mut steps = vec![serde_json::json!({"kind":"hover","x":1.0,"y":1.0})];
        let scale = std::env::var("HISTORY_PROBE_SCALE")
            .ok()
            .and_then(|value| value.parse::<f64>().ok())
            .unwrap_or(2.0);
        let mut click = |driver: &mut tilo_ui::headless::FrameHarness, rect: Rect| {
            let p = rect.center();
            steps.push(serde_json::json!({"kind":"click", "x":p.x*scale,"y":p.y*scale}));
            driver.click_at(p.x, p.y);
        };
        let open = driver.targets()["history-open"];
        click(&mut driver, open);
        let rows = driver
            .a11y_nodes()
            .into_iter()
            .filter(|(_, node)| node.role() == accesskit::Role::ListItem)
            .filter_map(|(_, node)| node.bounds())
            .collect::<Vec<_>>();
        let row = rows.last().expect("actual immutable history row");
        click(&mut driver, Rect::new(row.x0, row.y0, row.x1, row.y1));
        let restore = driver.targets()["history-restore"];
        click(&mut driver, restore);
        drop(click);
        steps.push(serde_json::json!({"kind":"key", "key":"escape"}));
        driver.key("Escape");
        let restore = driver.targets()["history-restore"].center();
        steps.push(serde_json::json!({"kind":"click","x":restore.x*scale,"y":restore.y*scale}));
        driver.click_at(restore.x, restore.y);
        steps.push(serde_json::json!({"kind":"key", "key":"enter"}));
        driver.key("Enter");
        assert_eq!(
            store.read(&checkout.id, body).unwrap(),
            "Original notebook text",
            "same shared frame path restores selected content"
        );
        let back = driver.targets()["history-back-live"].center();
        steps.push(serde_json::json!({"kind":"click","x":back.x*scale,"y":back.y*scale}));
        println!("{}", serde_json::to_string_pretty(&serde_json::json!({"name":"managed native History review and restore", "interval_us":4000000, "warmup_steps":1, "steps":steps})).unwrap());
        return;
    }
    let mut app = tilo_ui::App::from_factory(State::new(), move || root)
        .with_window_title("Managed History qualification");
    app.set_native_input_probe(move || store.read_history(&checkout.id, |doc, history| serde_json::json!({"body":body(doc),"heads":doc.base_heads().iter().map(ToString::to_string).collect::<Vec<_>>(),"pending_ops":doc.pending_ops(),"committed_change_count":committed_change_count(doc),"undo_revision":history.revision(),"last_drawn_preview":PREVIEW.with_borrow(Clone::clone)})).unwrap());
    app.run();
}
