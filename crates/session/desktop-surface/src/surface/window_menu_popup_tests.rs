use super::*;
use crate::surface::tests::one_window_desk;

/// The window `tab` shows fills its strip, as the desk's tree holds it.
fn fills_the_strip(handle: &DesktopHandle, tab: TabId) -> bool {
    handle.read(|tree| {
        tree.workspaces
            .iter()
            .flat_map(|workspace| &workspace.strips)
            .flat_map(|strip| &strip.window_groups)
            .find(|group| group.tabs.iter().any(|shown| shown.id == tab))
            .is_some_and(|group| group.maximized)
    })
}

/// A secondary click on a window's bar opens its menu where the click landed, and a click on
/// "Fill the strip" fills the strip with that window: the menu's own row, chosen as a person
/// chooses it, does what the desk says it does.
#[test]
fn a_row_clicked_in_the_menu_does_it_to_the_window_whose_bar_asked() {
    let (surface, handle) = one_window_desk(1);
    let mut desk =
        ui::headless::FrameHarness::new(ui::state::State::new(), Box::new(surface), (1280, 800));
    desk.relayout();
    let tab = handle.read(|tree| tree.workspaces[0].strips[0].window_groups[0].tabs[0].id);
    assert!(!fills_the_strip(&handle, tab));
    // What a secondary click on the window's bar leaves for its host.
    handle
        .shared
        .borrow_mut()
        .menus
        .push((tab, Point::new(200.0, 120.0)));

    let mut page = ui::headless::FrameHarness::new(
        ui::state::State::new(),
        Box::new(WindowMenuPopup::new(handle.clone())),
        (1280, 800),
    );
    page.relayout();
    page.poll_frame().expect("the menu draws");
    // Where the row is drawn, as the menu tells assistive technology: clicked there, as a
    // person clicks it.
    let row = page
        .a11y_nodes()
        .into_iter()
        .find(|(_, node)| node.label() == Some("Fill the strip"))
        .and_then(|(_, node)| node.bounds())
        .expect("the window's menu offers to fill the strip, where it is drawn");
    page.click_at(f64::midpoint(row.x0, row.x1), f64::midpoint(row.y0, row.y1));
    for _ in 0..4 {
        page.relayout();
        desk.relayout();
    }
    assert!(
        fills_the_strip(&handle, tab),
        "the row clicked filled the strip with the window whose bar asked"
    );
}

/// The menu stands where the bar was pressed, inside the area the popup covers, whatever
/// window the frame that draws it names: a page's frame can name none of its own, or a box a
/// widget drawn before left behind.
#[test]
fn the_menu_stands_where_the_bar_was_pressed_whatever_the_frame_names() {
    let (_surface, handle) = one_window_desk(1);
    let tab = handle.read(|tree| tree.workspaces[0].strips[0].window_groups[0].tabs[0].id);
    let mut popup = WindowMenuPopup::new(handle.clone());
    // Laid out over the whole page, as a host mounts it.
    popup.rect = Rect::new(0.0, 0.0, 1280.0, 800.0);
    handle
        .shared
        .borrow_mut()
        .menus
        .push((tab, Point::new(578.0, 58.0)));
    assert!(popup.follow().changed, "the bar's ask opens the menu");
    let presence = ui::presence::PresenceCtx::default();
    presence.viewport.set(Some(Rect::new(0.0, 0.0, 1.0, 1.0)));
    let theme = ui::Theme::dark();
    let mut text = ui::text_render::TextRenderer::new();
    popup.draw_overlay(
        &mut ui::vello::Scene::new(),
        ui::kurbo::Affine::IDENTITY,
        &mut text,
        &theme,
        &presence,
    );
    let menu = popup.menu.get_rect();
    assert!(
        menu.x0 >= 578.0 && menu.y0 >= 58.0 && menu.width() > 100.0,
        "the menu stands under the press, at its own size: {menu:?}"
    );
}
