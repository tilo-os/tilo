//! Durable cell scopes and the real peer-selection paint path.
use super::tests::measured_app;
use super::*;

#[test]
fn cell_presence_follows_insertions_and_rename_without_moving_the_local_selection() {
    let (mut app, mut tr, theme) = measured_app(1440.0, 810.0, 1.5);
    let local = app.grid.active();
    let key = app.cell_presence_key().expect("authored cell");
    let scope = presence::CellScope::from_key(&key).expect("roundtrip");
    let sheet_id = Id::from_raw(&scope.sheet);
    let mut workbook = app.workbook.as_ref().clone();
    let sheet = workbook.sheets.get_mut(&sheet_id).expect("sheet");
    sheet.name = ui::tilo_resource::Atom("Household".into());
    sheet.cells.insert_row(0, Id::from_raw("new-row"));
    assert_eq!(
        scope.corners(&sheet.layout()),
        Some(((local.0, local.1 + 1), (local.0, local.1 + 1)))
    );
    app.adopt_resource_state(&Rc::new(workbook));

    let peer = ui::permissions::UserId("ada".into());
    let mut presence = PresenceCtx::default();
    presence.register_presence(&key, peer.clone());
    presence.set_display_name(peer, "Ada");
    let mut overlay = Scene::new();
    app.draw_cell_presence(&mut overlay, Affine::IDENTITY, &mut tr, &theme, &presence);
    assert!(
        !overlay.encoding().resources.glyph_runs.is_empty(),
        "the peer's name paints after the sheet is renamed and a row inserted"
    );
    assert_eq!(
        app.grid.active(),
        local,
        "remote presence cannot move our selection"
    );

    // A different sheet and malformed/oversized scopes cannot paint a label.
    app.active_sheet = 1;
    let mut elsewhere = Scene::new();
    app.draw_cell_presence(&mut elsewhere, Affine::IDENTITY, &mut tr, &theme, &presence);
    assert!(elsewhere.encoding().resources.glyph_runs.is_empty());
    for key in [
        PresenceKey::resource("other-app", "{}", "cell-selection"),
        PresenceKey::resource("sheets", "{", "cell-selection"),
        PresenceKey::resource("sheets", "x".repeat(4097), "cell-selection"),
    ] {
        assert!(presence::CellScope::from_key(&key).is_none());
    }
}
