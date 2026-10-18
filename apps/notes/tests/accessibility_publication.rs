//! What the host's accessibility walk costs and publishes when Notes runs
//! as the desktop runs an app in a tile (`ui::app::EmbeddedApp`): the real
//! window over its real record, driven through `App::surface_input`, framed
//! by `App::surface_frame_with_damage`, then the semantic tree refreshed as
//! the host reads it after every frame.
//!
//! A note at rest walks no widgets however many frames the host draws; a
//! keystroke publishes the block it changed on the frame that shows it.

use notes::NotesShell;
use ui::app::{App, AppSurface, SurfaceDamage};
use ui::framework::testing::drive_live_window;
use ui::synthetic_input::{cursor_moved, key_input, mouse_press, mouse_release};
use ui::winit::event::ElementState;
use ui::winit::keyboard::Key;
use vello::Scene;

const WIDTH: u32 = 1280;
const HEIGHT: u32 = 900;

/// The app's region of the host's scene, at scale 1.
struct Region;

impl AppSurface for Region {
    fn scale_factor(&self) -> f64 {
        1.0
    }
    fn inner_size(&self) -> ui::winit::dpi::PhysicalSize<u32> {
        ui::winit::dpi::PhysicalSize::new(WIDTH, HEIGHT)
    }
    fn request_redraw(&self) {}
    fn set_cursor(&self, _: ui::winit::window::CursorIcon) {}
}

/// One host frame, as `EmbeddedApp::draw` runs it: app frames until the app
/// is at rest, then the accessibility walk. Returns what the walk published
/// and whether the frame repainted anything.
fn frame(app: &mut App, damage: &mut SurfaceDamage) -> (ui::a11y_tree::TreeDelta, bool) {
    let mut scene = Scene::new();
    for _ in 0..3 {
        scene.reset();
        if !app.surface_frame_with_damage(&Region, &mut scene, damage) || !app.unsettled() {
            break;
        }
    }
    let delta = app.refresh_semantic_tree();
    let repainted = damage.take().is_none_or(|rects| !rects.is_empty());
    (delta, repainted)
}

fn input(app: &mut App, damage: &mut SurfaceDamage, event: ui::winit::event::WindowEvent) {
    app.surface_input(&Region, event, false);
    frame(app, damage);
}

/// The values of the published nodes named `name`.
fn values_of(app: &App, name: &str) -> Vec<String> {
    app.semantic_tree()
        .iter()
        .filter(|node| node.name.as_deref() == Some(name))
        .filter_map(|node| node.value.clone())
        .collect()
}

/// Draw host frames until the window is at rest: a second of frames in a row
/// that repainted nothing and walked no widgets, so the save of the last key
/// has landed and the timers it set (a spelling squiggle settling, the word
/// under the caret) have run.
fn rest(app: &mut App, damage: &mut SurfaceDamage) {
    let mut quiet = 0_u32;
    for _ in 0..400 {
        std::thread::sleep(std::time::Duration::from_millis(25));
        let builds = app.semantic_tree_builds_for_test();
        let (_, repainted) = frame(app, damage);
        quiet = if repainted || app.semantic_tree_builds_for_test() != builds {
            0
        } else {
            quiet.saturating_add(1)
        };
        if quiet >= 40 {
            return;
        }
    }
    panic!("the window never came to rest");
}

/// A new note in a fresh window holding `text`, the caret in its body, at
/// rest.
fn a_note_holding(app: &mut App, damage: &mut SurfaceDamage, text: &str) {
    a_note_holding_among(app, damage, text, 0);
}

/// [`a_note_holding`] in a notebook already holding `notes` other notes,
/// written through the notebook's own actions as an agent writes them.
fn a_note_holding_among(app: &mut App, damage: &mut SurfaceDamage, text: &str, notes: usize) {
    app.init_layout();
    for _ in 0..5 {
        frame(app, damage);
    }
    for note in 0..notes {
        let action = notes::actions::create_page_action(
            notes::PageId::new_v4(),
            None,
            &format!("Listed note {note}"),
        );
        let invoke = ui::tilo_resource::Invoke::<notes::Notebook>::apply(&action)
            .expect("creating a note is a notebook action");
        app.invoke_local_resource_action(&invoke.address(), &invoke.arguments)
            .expect("the notebook takes the note");
        frame(app, damage);
    }
    let new_note = app
        .semantic_tree()
        .iter()
        .find(|node| node.name.as_deref() == Some("New note"))
        .and_then(|node| node.bounds)
        .expect("a New note button on screen")
        .center();
    for event in [
        cursor_moved(new_note.x, new_note.y),
        mouse_press(),
        mouse_release(),
    ] {
        input(app, damage, event);
    }
    // The pointer rests in the empty page below the note.
    input(app, damage, cursor_moved(640.0, 700.0));
    for letter in text.chars() {
        let key = Key::Character(letter.to_string().into());
        input(app, damage, key_input(&key, ElementState::Pressed));
        input(app, damage, key_input(&key, ElementState::Released));
    }
    assert!(
        values_of(app, "Block text").contains(&text.to_owned()),
        "the keys reached the note: {:?}",
        values_of(app, "Block text")
    );
    rest(app, damage);
}

#[test]
fn a_note_at_rest_walks_no_widgets_and_a_key_publishes_on_its_frame() {
    ui::i18n::I18nManager::init("en-US");
    drive_live_window::<NotesShell, _>(|app| {
        let mut damage = SurfaceDamage::default();
        a_note_holding(app, &mut damage, "Rest");
        let builds = app.semantic_tree_builds_for_test();
        for _ in 0..30 {
            let (delta, _) = frame(app, &mut damage);
            assert!(delta.is_empty(), "a frame at rest published {delta:?}");
        }
        assert_eq!(
            app.semantic_tree_builds_for_test(),
            builds,
            "thirty frames at rest rebuilt the semantic tree"
        );

        let key = Key::Character("s".into());
        app.surface_input(&Region, key_input(&key, ElementState::Pressed), false);
        let (delta, _) = frame(app, &mut damage);
        assert!(
            app.semantic_tree_builds_for_test() > builds,
            "the keystroke's frame walked no widgets"
        );
        assert!(
            values_of(app, "Block text").contains(&"Rests".to_owned()),
            "the frame that shows the key publishes it: {:?}",
            values_of(app, "Block text")
        );
        assert!(!delta.is_empty(), "the keystroke's frame published nothing");
    })
    .unwrap();
}

/// A hand resting on the mouse over a note's text moves the pointer a pixel
/// at a time: a move that changes no hover repaints nothing and walks no
/// widgets, so the frame costs the host no accessibility walk.
#[test]
fn a_pointer_moving_within_a_block_walks_no_widgets() {
    ui::i18n::I18nManager::init("en-US");
    drive_live_window::<NotesShell, _>(|app| {
        let mut damage = SurfaceDamage::default();
        a_note_holding(app, &mut damage, "Rest");
        let text = app
            .semantic_tree()
            .iter()
            .find(|node| node.name.as_deref() == Some("Block text"))
            .and_then(|node| node.bounds)
            .expect("the note's text on screen")
            .center();
        // Over the text: the block is hovered from here on.
        input(app, &mut damage, cursor_moved(text.x, text.y));
        rest(app, &mut damage);

        let builds = app.semantic_tree_builds_for_test();
        for step in 1..=6_u8 {
            app.surface_input(
                &Region,
                cursor_moved(text.x + f64::from(step), text.y),
                false,
            );
            let (delta, _) = frame(app, &mut damage);
            assert!(delta.is_empty(), "a pointer move published {delta:?}");
        }
        assert_eq!(
            app.semantic_tree_builds_for_test(),
            builds,
            "pointer moves within a block walked the widgets"
        );
    })
    .unwrap();
}

/// What the accessibility walk of a keystroke's frame costs in a new note
/// beside a page list of `notes` other notes: the semantic nodes it built,
/// and the subtrees widgets handed it other than as the very node it kept,
/// each of which it compared or built. Also the rows the page list shows.
fn keystroke_walk_beside(notes: usize) -> (usize, usize, usize) {
    drive_live_window::<NotesShell, _>(|app| {
        let mut damage = SurfaceDamage::default();
        a_note_holding_among(app, &mut damage, "Rest", notes);
        let key = Key::Character("s".into());
        app.surface_input(&Region, key_input(&key, ElementState::Pressed), false);
        frame(app, &mut damage);
        assert!(
            values_of(app, "Block text").contains(&"Rests".to_owned()),
            "the frame that shows the key publishes it: {:?}",
            values_of(app, "Block text")
        );
        let tree = app.semantic_tree();
        let rows = tree
            .iter()
            .filter(|node| node.role == ui::a11y::Role::TreeItem)
            .count();
        (
            tree.nodes_built_for_test(),
            tree.subtrees_compared_for_test(),
            rows,
        )
    })
    .unwrap()
}

/// A keystroke in a note costs the walk the block it changed: the page
/// list beside it hands every row over as the very node it handed over
/// before, and the notebook's published resources keep their nodes, so the
/// walk builds and compares the same in a notebook of three notes as in one
/// of twenty-one.
#[test]
fn a_keystroke_walks_the_same_nodes_however_many_notes_are_listed() {
    ui::i18n::I18nManager::init("en-US");
    let (few_built, few_compared, few_rows) = keystroke_walk_beside(2);
    let (many_built, many_compared, many_rows) = keystroke_walk_beside(20);
    assert!(
        few_rows >= 3 && many_rows >= 21,
        "the page list shows every note: {few_rows} and {many_rows} rows"
    );
    assert_eq!(
        (few_built, few_compared),
        (many_built, many_compared),
        "a keystroke beside {few_rows} listed notes built and compared \
         ({few_built}, {few_compared}); beside {many_rows}, ({many_built}, {many_compared})"
    );
}
