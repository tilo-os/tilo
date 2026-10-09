use super::*;
use crate::shell::search::{ResourceKind, SearchResult};
use ui::widget::Container;

fn mounted_host() -> ShellOverlayHost {
    let mut host = ShellOverlayHost::with_fixture_content(Box::new(Container::new()));
    host.palette_visible = true;
    host.palette.set_focus(true);
    host.palette.set_rect(Rect::new(20.0, 30.0, 620.0, 630.0));
    host.search_results = (0..3)
        .map(|index| {
            SearchResult::nullary(
                ResourceKind::Action,
                format!("provider-{index}"),
                format!("open-pane-{index}"),
                "Same label",
                None,
                100,
                |_, _, _| panic!("inspection must never invoke a result"),
            )
        })
        .collect();
    host.palette.set_entries(vec![
        CommandEntry::new("result:0", "Alpha").group("First"),
        CommandEntry::new("result:1", "Bravo").group("Second"),
        CommandEntry::new("result:2", "Charlie").group("Second"),
    ]);
    host
}

#[test]
fn inspection_reads_real_identity_geometry_hover_without_mutating_selection() {
    let mut host = mounted_host();
    host.palette.set_cursor(0);
    let second = host.palette.row_rect(1).expect("second row is mounted");
    host.note_pointer(second.center());
    let selected_before = host.palette.selected_id().map(str::to_owned);
    let query_before = host.palette.query().to_owned();
    let inspection = host.palette_inspection();
    assert_eq!(inspection["selected_entry_id"], "result:0");
    assert_eq!(inspection["hovered_entry_id"], "result:1");
    let row = &inspection["rows"][1];
    assert_eq!(row["entry_id"], "result:1");
    assert_eq!(row["provider_id"], "provider-1");
    assert_eq!(row["action_id"], "open-pane-1");
    assert_eq!(row["hovered"], true);
    assert_eq!(row["selected"], false);
    assert_eq!(
        row["bounds"],
        serde_json::json!([second.x0, second.y0, second.x1, second.y1])
    );
    assert_eq!(host.palette.selected_id(), selected_before.as_deref());
    assert_eq!(host.palette.query(), query_before);
    assert_eq!(host.search_results.len(), 3);
    assert_eq!(host.palette_inspection(), inspection, "reads are inert");
    assert!(!inspection.to_string().contains("Same label"));
    drop(host);
}

#[test]
fn inspection_maps_filtered_visible_ordinal_to_real_result_ordinal() {
    let mut host = mounted_host();
    host.palette.set_bypass_fuzzy(false);
    host.palette.set_query("Charlie");
    let inspection = host.palette_inspection();
    let rows = inspection["rows"].as_array().expect("mounted rows");
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0]["visible_index"], 0);
    assert_eq!(rows[0]["result_index"], 2);
    assert_eq!(rows[0]["provider_id"], "provider-2");
    assert_eq!(rows[0]["action_id"], "open-pane-2");
    assert_eq!(inspection["query_current"], false);
    assert_eq!(inspection["query_bytes"], 7);
    assert_eq!(
        inspection["query_hash"],
        blake3::hash(b"Charlie").to_hex().to_string()
    );
    assert!(!inspection.to_string().contains("Charlie"));
    drop(host);
}

#[test]
fn inspection_has_no_hover_on_header_and_no_mounted_rows_when_hidden() {
    let mut host = mounted_host();
    let first = host.palette.row_rect(0).expect("first row is mounted");
    // The actual grouped widget leaves the first section header before this row.
    let above = Point::new(first.center().x, first.y0 - 1.0);
    assert_eq!(host.palette.row_at(above), None);
    host.note_pointer(above);
    assert!(host.palette_inspection()["hovered_entry_id"].is_null());
    host.palette_visible = false;
    let hidden = host.palette_inspection();
    assert_eq!(hidden["mounted"], false);
    assert_eq!(hidden["rows"], serde_json::json!([]));
    for field in [
        "bounds",
        "query_bounds",
        "selected_entry_id",
        "hovered_entry_id",
    ] {
        assert!(hidden[field].is_null(), "hidden {field} must be absent");
    }
    drop(host);
}

#[test]
fn inspection_generation_changes_only_when_real_results_are_refreshed() {
    let chrome = crate::shell::chrome_state::ChromeState::new(
        crate::shell::state::WorkspaceTree::bootstrap_demo(),
    );
    let mut host = mounted_host().with_chrome(chrome);
    let doc = AutoCommit::new();
    host.refresh_search_results(&doc, &automerge::ROOT);
    let first = host.palette_inspection();
    assert_eq!(first["query_current"], true);
    assert_eq!(first["results_generation"], 1);
    host.refresh_search_results(&doc, &automerge::ROOT);
    assert_eq!(host.palette_inspection()["results_generation"], 1);
    host.palette.set_query("unmatched inspected query");
    assert_eq!(host.palette_inspection()["query_current"], false);
    host.refresh_search_results(&doc, &automerge::ROOT);
    assert_eq!(host.palette_inspection()["results_generation"], 2);
    assert_eq!(host.palette_inspection()["query_current"], true);
    drop(host);
}

fn draw_palette(host: &mut ShellOverlayHost) {
    host.observe_palette_draws(true);
    host.palette.draw(
        &mut vello::Scene::new(),
        Affine::IDENTITY,
        &mut ui::TextRenderer::new(),
        &ui::Theme::dark(),
        &ui::PresenceCtx::default(),
    );
}

#[test]
fn rendered_inspection_requires_real_draw_and_rejects_replaced_entry_mapping() {
    let mut host = mounted_host();
    assert!(host.palette_inspection()["rendered"].is_null());
    draw_palette(&mut host);
    let before = host.palette_inspection();
    assert_eq!(before["rendered"]["rows"][0]["action_id"], "open-pane-0");
    host.palette.set_query("different query");
    let changed = host.palette_inspection();
    assert_ne!(changed["query_hash"], changed["rendered"]["query_hash"]);
    assert_eq!(
        changed["rendered"], before["rendered"],
        "inspection cannot fabricate a draw"
    );
    draw_palette(&mut host);
    let drawn = host.palette_inspection();
    assert_eq!(drawn["query_hash"], drawn["rendered"]["query_hash"]);
    host.palette
        .set_entries(vec![CommandEntry::new("result:0", "Replacement")]);
    host.search_results[0].id = "replacement-action".into();
    assert!(
        host.palette_inspection()["rendered"].is_null(),
        "old drawing cannot bind to new result:0"
    );
    draw_palette(&mut host);
    assert_eq!(
        host.palette_inspection()["rendered"]["rows"][0]["action_id"],
        "replacement-action"
    );
    host.palette_visible = false;
    assert!(host.palette_inspection()["rendered"].is_null());
    drop(host);
}
