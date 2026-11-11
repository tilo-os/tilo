//! [`TabStrip`] is the window bar's strip as a described component: its pills are
//! the bar's, it reports what the person did with them as the view's own events,
//! and it draws nothing of its own.

use tilo_ui::component::{Described, Event, Person, ResourceId};
use tilo_ui::tokens::PresenceHue;
use tilo_ui::widget::{TabItemProps, TabStrip, TabStripProps, pack_point, unpack_point};

fn tab(key: &str, title: &str) -> TabItemProps {
    TabItemProps {
        key: key.to_owned(),
        item: Some(ResourceId(format!("@notes.Note:{key}"))),
        title: Some(title.to_owned()),
        icon: Some(tilo_ui::icons::Icon::FileText),
        attention: false,
        modified: false,
        busy: false,
        presence: Vec::new(),
    }
}

fn props(tabs: Vec<TabItemProps>, active: usize) -> TabStripProps {
    TabStripProps {
        key: "frame".to_owned(),
        active: tabs.get(active).and_then(|t| t.item.clone()),
        focused: true,
        activate: Event::None,
        close: Event::None,
        reorder: Event::None,
        detach: Event::None,
        tabs,
    }
}

/// The pills are the bar's: one per tab, in the view's order, with the active tab in front
/// and each carrying the glyph its type names.
#[test]
fn the_strip_is_the_window_bars_strip() {
    let tabs = vec![tab("a", "Sorting"), tab("b", "Second"), tab("c", "Third")];
    let strip = TabStrip::from_props("frame", props(tabs, 1));
    let bar = strip.bar();
    let titles: Vec<&str> = bar.tabs().iter().map(|t| t.title.as_str()).collect();
    assert_eq!(titles, ["Sorting", "Second", "Third"]);
    let active = bar.active_id();
    assert_eq!(
        active,
        bar.tabs()[1].id,
        "the view's active tab is the bar's"
    );
    assert!(
        bar.tabs().iter().all(|t| t.app_kind.is_some()),
        "each pill carries its type's glyph"
    );
}

/// A strip of one tab offers no close chip (the frame closes it); of two or more, each tab
/// does.
#[test]
fn a_lone_tab_is_closed_with_its_frame() {
    let one = TabStrip::from_props("frame", props(vec![tab("a", "Only")], 0));
    assert!(!one.bar().closable_for_tests());
    let two = TabStrip::from_props("frame", props(vec![tab("a", "A"), tab("b", "B")], 0));
    assert!(two.bar().closable_for_tests());
}

/// A strip's props are the same strip while only its events change, and a new strip when its
/// tabs, its active tab or its focus do.
#[test]
fn a_strip_keeps_its_pills_while_its_events_change() {
    let tabs = vec![tab("a", "A"), tab("b", "B")];
    let mut strip = TabStrip::from_props("frame", props(tabs.clone(), 0));
    let mut next = props(tabs.clone(), 0);
    next.activate = Event::None;
    assert!(strip.set_props(next), "the same tabs are the same strip");

    let mut retitled = props(tabs.clone(), 0);
    retitled.tabs[0].title = Some("Renamed".to_owned());
    assert!(
        !strip.set_props(retitled),
        "a retitled tab builds the strip again, and shows the new title"
    );
    let strip = TabStrip::from_props("frame", {
        let mut p = props(tabs, 0);
        p.tabs[0].title = Some("Renamed".to_owned());
        p
    });
    assert_eq!(strip.bar().tabs()[0].title, "Renamed");
}

/// Who is at a resource shows on its pill, in their presence colour.
#[test]
fn a_pill_says_who_is_at_its_resource() {
    let mut tabs = vec![tab("a", "A")];
    tabs[0].presence = vec![tilo_ui::widget::PresenceFaceProps {
        key: "mia".to_owned(),
        who: Some(Person {
            identity: "mia".to_owned(),
            name: "Mia".to_owned(),
            hue: PresenceHue::of_identity("mia"),
        }),
        name: None,
    }];
    let strip = TabStrip::from_props("frame", props(tabs, 0));
    let id = strip.bar().tabs()[0].id;
    let holder = strip
        .bar()
        .holder(id)
        .expect("the pill carries who is at its resource");
    assert_eq!(holder.name, "Mia");
}

/// A point a pill was let go at crosses the event unchanged, at the window's scale.
#[test]
fn a_drop_point_crosses_an_event_whole() {
    for (x, y) in [(0.0, 0.0), (12.0, 640.0), (1279.0, 799.0), (3440.0, 1439.0)] {
        assert_eq!(unpack_point(pack_point(x, y)), (x, y), "at {x},{y}");
    }
}

/// The strip lays out headlessly, as the window bar does in a frame: it takes the width its
/// column gives it and stands a bar tall.
#[test]
fn the_strip_lays_out_as_the_window_bars_does() {
    use tilo_ui::widget::{Column, Element};

    let tabs = vec![tab("a", "Sorting"), tab("b", "Second"), tab("c", "Third")];
    let strip = TabStrip::from_props("frame", props(tabs, 0));
    let column = Column::new().push(Box::new(strip) as Element);
    let harness = tilo_ui::headless::FrameHarness::new_with_theme(
        tilo_ui::State::new(),
        Box::new(column),
        (900, 200),
        tilo_ui::Theme::light(),
    );
    let rect = harness.inspection().root.rect;
    assert!(rect[3] > 0.0, "the strip stands a bar tall: {rect:?}");
}

/// A press on a pill is the view's own event in the pass that took it: the strip reports what
/// its bar marked, so a view hears the pill it was pressed for without asking for reports.
#[test]
fn a_press_on_a_pill_is_the_views_event_in_the_same_pass() {
    use tilo_ui::component::{Fired, Intent, Value};
    use tilo_ui::widget::{Column, Element};

    let queue: tilo_ui::Intents<Fired> = tilo_ui::Intents::default();
    let mut given = props(vec![tab("a", "A"), tab("b", "B")], 0);
    given.activate = Event::View(Intent::new(&queue, "frame", "activate"));
    let strip = TabStrip::from_props("frame", given);
    let column = Column::new().push(Box::new(strip) as Element);
    let mut harness = tilo_ui::headless::FrameHarness::new_with_theme(
        tilo_ui::State::new(),
        Box::new(column),
        (400, 200),
        tilo_ui::Theme::light(),
    );
    harness.relayout();
    let pill = {
        let strip = harness.root_widget().children()[0];
        let mut names = Vec::new();
        let found = strip
            .test_rects()
            .into_iter()
            .find(|(name, rect)| {
                names.push(name.clone());
                name == "tab:B" && rect.area() > 0.0
            })
            .map(|(_, rect)| rect);
        found.unwrap_or_else(|| panic!("the second pill is a target; the bar answers to {names:?}"))
    };
    let device_id = tilo_ui::winit::event::DeviceId::dummy();
    harness.send_host_event(&tilo_ui::winit::event::WindowEvent::CursorMoved {
        device_id,
        position: tilo_ui::winit::dpi::PhysicalPosition::new(pill.center().x, pill.center().y),
    });
    for state in [
        tilo_ui::winit::event::ElementState::Pressed,
        tilo_ui::winit::event::ElementState::Released,
    ] {
        harness.send_host_event(&tilo_ui::winit::event::WindowEvent::MouseInput {
            device_id,
            state,
            button: tilo_ui::winit::event::MouseButton::Left,
        });
    }
    let fired = queue.take();
    assert_eq!(fired.len(), 1, "one press is one event: {fired:?}");
    assert_eq!(fired[0].event, "activate");
    assert_eq!(
        fired[0].value,
        Some(Value::Resource("@notes.Note:b".to_owned())),
        "the event carries the resource of the pill pressed"
    );
}
