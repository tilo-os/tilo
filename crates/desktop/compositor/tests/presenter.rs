//! Presenter's view on your screen, slides on the big one.
//!
//! A laptop plugged into a television: the app opens an `.audience` toplevel beside its window.
//! The slides must land on the television even though the laptop's panel comes first and has more
//! pixels, and the app's own window must move to the laptop as the presenter's view: covering
//! that screen, and back where it stood at the size it had when the presentation ends. Driven
//! over a real connection with real `wl_output` globals, since which screen is which is what goes
//! over the wire.

use tilo_compositor::{Compositor, OutputConfig, StackPolicy};
use wayland_protocols::xdg::shell::client::xdg_toplevel::XdgToplevel;

use crate::harness::{TestClient, map, pump};

const APP: &str = "so.tilo.test.slides";
const AUDIENCE: &str = "so.tilo.test.slides.audience";

/// A 14" panel of 2880x1800 on eDP, at the 200% its density argues for, and a 65" television of
/// 1920x1080 on HDMI beside it: the laptop comes first and has more pixels, so neither may decide.
fn laptop_and_television() -> Compositor {
    let mut compositor = Compositor::headless(Box::new(StackPolicy)).expect("compositor");
    compositor
        .add_output(&OutputConfig::new("eDP-1", 2880, 1800).panel((302, 189)))
        .expect("laptop panel");
    compositor
        .add_output(&OutputConfig::new("HDMI-A-1", 1920, 1080).panel((1430, 800)))
        .expect("television");
    compositor
}

/// The logical rectangle of the output `name`.
fn rect_of(compositor: &Compositor, name: &str) -> (i32, i32, i32, i32) {
    compositor
        .outputs()
        .into_iter()
        .find(|output| output.name == name)
        .and_then(|output| output.rect)
        .expect("the output is on")
}

/// Where the window of `app` stands, and its id.
fn window(compositor: &Compositor, app: &str) -> Option<(u64, (i32, i32, i32, i32))> {
    compositor
        .windows()
        .into_iter()
        .find(|(info, _)| info.app_id.as_deref() == Some(app))
        .map(|(info, rect)| (info.id, rect))
}

/// The size `toplevel` was last configured to, and whether it was asked to be fullscreen.
fn configured(client: &TestClient, toplevel: &XdgToplevel) -> Option<((i32, i32), bool)> {
    client
        .seen
        .toplevel_configures
        .iter()
        .rev()
        .find(|(of, _, _)| of == toplevel)
        .map(|(_, size, fullscreen)| (*size, *fullscreen))
}

/// The app's two windows on one connection: its own, mapped at 800x600, and the audience's,
/// opened after it.
struct Slides {
    client: TestClient,
    presenter: (wayland_client::protocol::wl_surface::WlSurface, XdgToplevel),
    audience: (wayland_client::protocol::wl_surface::WlSurface, XdgToplevel),
}

impl Slides {
    fn open(compositor: &mut Compositor) -> Self {
        let mut client = map(
            TestClient::connect(compositor),
            compositor,
            APP,
            (800, 600),
            [30, 30, 30, 255],
        );
        let presenter = (
            client.surface.take().unwrap(),
            client.toplevel.take().unwrap(),
        );
        client.create_window(AUDIENCE);
        client.commit();
        let audience = (
            client.surface.clone().unwrap(),
            client.toplevel.clone().unwrap(),
        );
        let toplevel = audience.1.clone();
        pump(compositor, &mut [&mut client], |_, clients| {
            configured(clients[0], &toplevel).is_some()
        });
        let ((w, h), _) = configured(&client, &audience.1).unwrap();
        client.show(w, h, [240, 240, 240, 255]);
        pump(compositor, &mut [&mut client], |compositor, _| {
            window(compositor, AUDIENCE).is_some()
        });
        Self {
            client,
            presenter,
            audience,
        }
    }

    /// Draw `which` at the size it was last asked for, as a client does, and let the compositor
    /// take it.
    fn redraw(&mut self, compositor: &mut Compositor, presenter: bool) {
        let (surface, toplevel) = if presenter {
            &self.presenter
        } else {
            &self.audience
        };
        let ((w, h), _) = configured(&self.client, toplevel).unwrap();
        self.client.surface = Some(surface.clone());
        self.client.show(w, h, [30, 30, 30, 255]);
        self.client.surface = None;
        let app = if presenter { APP } else { AUDIENCE };
        pump(compositor, &mut [&mut self.client], |compositor, _| {
            window(compositor, app).is_some_and(|(_, rect)| (rect.2, rect.3) == (w, h))
        });
    }
}

#[test]
fn wl_output_names_the_connector_and_its_millimetres() {
    let mut compositor = laptop_and_television();
    let mut client = TestClient::connect(&mut compositor);
    pump(&mut compositor, &mut [&mut client], |_, clients| {
        clients[0].seen.outputs.len() == 2
            && clients[0]
                .seen
                .outputs
                .iter()
                .all(|(_, name, _)| !name.is_empty())
    });
    let mut advertised: Vec<(String, (i32, i32))> = client
        .seen
        .outputs
        .iter()
        .map(|(_, name, mm)| (name.clone(), *mm))
        .collect();
    advertised.sort();
    assert_eq!(
        advertised,
        [
            ("HDMI-A-1".to_owned(), (1430, 800)),
            ("eDP-1".to_owned(), (302, 189)),
        ],
        "what a client ranks screens by reaches it: the connector and real millimetres"
    );
}

#[test]
fn slides_go_to_the_television_and_the_presenters_view_to_the_laptop() {
    let mut compositor = laptop_and_television();
    let laptop = rect_of(&compositor, "eDP-1");
    let television = rect_of(&compositor, "HDMI-A-1");
    let mut slides = Slides::open(&mut compositor);
    let (presenter_id, _) = window(&compositor, APP).unwrap();
    let (audience_id, _) = window(&compositor, AUDIENCE).unwrap();

    // The compositor's own choice: the app named no screen before its first buffer.
    let presenting = compositor.presenting().expect("a presentation");
    assert_eq!(presenting.audience, audience_id);
    assert_eq!(
        presenting.audience_on, "HDMI-A-1",
        "the slides go to the television, not the laptop's panel that comes first"
    );
    assert_eq!(
        presenting.presenter,
        Some((presenter_id, "eDP-1".to_owned())),
        "the presenter's view moves to the laptop"
    );
    assert_eq!(
        configured(&slides.client, &slides.audience.1),
        Some(((television.2, television.3), true)),
        "the slides fill the television"
    );
    assert_eq!(
        configured(&slides.client, &slides.presenter.1),
        Some(((laptop.2, laptop.3), true)),
        "the presenter's view is sized to the laptop's screen"
    );
    slides.redraw(&mut compositor, true);
    assert_eq!(window(&compositor, APP), Some((presenter_id, laptop)));
    assert_eq!(
        window(&compositor, AUDIENCE),
        Some((audience_id, television))
    );
    let mut covered = compositor.fullscreen_outputs();
    covered.sort();
    assert_eq!(
        covered,
        ["HDMI-A-1", "eDP-1"],
        "each covers its display, so the shell steps aside on both"
    );

    // An app that drops its own single-screen fullscreen next does not pull the presenter's view
    // back.
    slides.presenter.1.unset_fullscreen();
    pump(&mut compositor, &mut [&mut slides.client], |_, _| true);
    assert_eq!(window(&compositor, APP), Some((presenter_id, laptop)));
    assert_eq!(
        configured(&slides.client, &slides.presenter.1),
        Some(((laptop.2, laptop.3), true))
    );

    // The app names its screen: choosing the laptop for the slides sends the presenter's view to
    // the television.
    let laptop_output = slides
        .client
        .seen
        .outputs
        .iter()
        .find(|(_, name, _)| name == "eDP-1")
        .map(|(output, _, _)| output.clone())
        .expect("the laptop's wl_output");
    slides.audience.1.set_fullscreen(Some(&laptop_output));
    pump(
        &mut compositor,
        &mut [&mut slides.client],
        |compositor, _| {
            compositor
                .presenting()
                .is_some_and(|presenting| presenting.audience_on == "eDP-1")
        },
    );
    assert_eq!(
        compositor.presenting().unwrap().presenter,
        Some((presenter_id, "HDMI-A-1".to_owned()))
    );
    slides.redraw(&mut compositor, true);
    slides.redraw(&mut compositor, false);
    assert_eq!(window(&compositor, APP), Some((presenter_id, television)));
    assert_eq!(window(&compositor, AUDIENCE), Some((audience_id, laptop)));

    // Ending the presentation puts the window back where it stood, at the size it had.
    slides.audience.1.destroy();
    slides.audience.0.destroy();
    pump(
        &mut compositor,
        &mut [&mut slides.client],
        |compositor, _| compositor.presenting().is_none(),
    );
    assert_eq!(
        configured(&slides.client, &slides.presenter.1),
        Some(((800, 600), false)),
        "back at its own size, no longer fullscreen"
    );
    slides.redraw(&mut compositor, true);
    let (_, back) = window(&compositor, APP).unwrap();
    assert_eq!((back.2, back.3), (800, 600));
    assert!(
        back.0 + back.2 <= laptop.2,
        "back where it stood on the laptop: {back:?}"
    );
    assert!(compositor.fullscreen_outputs().is_empty());
}

#[test]
fn one_screen_keeps_the_presenter_where_it_is() {
    let mut compositor = Compositor::headless(Box::new(StackPolicy)).expect("compositor");
    compositor
        .add_output(&OutputConfig::new("eDP-1", 2880, 1800).panel((302, 189)))
        .expect("laptop panel");
    let laptop = rect_of(&compositor, "eDP-1");
    let slides = Slides::open(&mut compositor);
    let (presenter_id, before) = window(&compositor, APP).unwrap();
    let presenting = compositor.presenting().expect("a presentation");
    assert_eq!(
        presenting.audience_on, "eDP-1",
        "the slides fill the one screen"
    );
    assert_eq!(
        presenting.presenter, None,
        "with nowhere else to go the presenter's window stays where it is"
    );
    assert_eq!(
        configured(&slides.client, &slides.audience.1),
        Some(((laptop.2, laptop.3), true))
    );
    assert_eq!(window(&compositor, APP), Some((presenter_id, before)));
    assert_eq!(
        configured(&slides.client, &slides.presenter.1).map(|(_, fullscreen)| fullscreen),
        Some(false)
    );
}

#[test]
fn unplugging_the_television_gives_the_slides_the_laptop_and_plugging_it_back_returns_them() {
    let mut compositor = laptop_and_television();
    let laptop = rect_of(&compositor, "eDP-1");
    let mut slides = Slides::open(&mut compositor);
    let (presenter_id, _) = window(&compositor, APP).unwrap();
    slides.redraw(&mut compositor, true);

    // The cable comes out: the slides stay on the screen that is left, and the presenter's view
    // goes back to the window it was, at its own size.
    compositor.remove_output("HDMI-A-1").expect("unplugged");
    pump(
        &mut compositor,
        &mut [&mut slides.client],
        |compositor, _| {
            compositor
                .presenting()
                .is_some_and(|presenting| presenting.presenter.is_none())
        },
    );
    assert_eq!(compositor.presenting().unwrap().audience_on, "eDP-1");
    pump(&mut compositor, &mut [&mut slides.client], |_, clients| {
        configured(clients[0], &slides.audience.1) == Some(((laptop.2, laptop.3), true))
    });
    assert_eq!(
        configured(&slides.client, &slides.presenter.1),
        Some(((800, 600), false)),
        "the presenter's window is its own again"
    );

    // Plugged back in: the slides return to the television and the presenter's view to the laptop.
    compositor
        .add_output(&OutputConfig::new("HDMI-A-1", 1920, 1080).panel((1430, 800)))
        .expect("television");
    pump(
        &mut compositor,
        &mut [&mut slides.client],
        |compositor, _| {
            compositor.presenting().is_some_and(|presenting| {
                presenting.audience_on == "HDMI-A-1"
                    && presenting.presenter == Some((presenter_id, "eDP-1".to_owned()))
            })
        },
    );
    pump(&mut compositor, &mut [&mut slides.client], |_, clients| {
        configured(clients[0], &slides.presenter.1) == Some(((laptop.2, laptop.3), true))
    });
}

#[test]
fn a_window_named_only_audience_belongs_to_no_app() {
    let mut compositor = laptop_and_television();
    let mut client = map(
        TestClient::connect(&mut compositor),
        &mut compositor,
        APP,
        (800, 600),
        [30, 30, 30, 255],
    );
    client.create_window(".audience");
    client.commit();
    let toplevel = client.toplevel.clone().unwrap();
    pump(&mut compositor, &mut [&mut client], |_, clients| {
        configured(clients[0], &toplevel).is_some()
    });
    client.show(300, 200, [200, 0, 0, 255]);
    pump(&mut compositor, &mut [&mut client], |compositor, _| {
        window(compositor, ".audience").is_some()
    });
    assert_eq!(
        compositor.presenting(),
        None,
        "an app id is needed before the suffix"
    );
}

#[test]
fn another_programs_window_is_no_apps_audience() {
    let mut compositor = laptop_and_television();
    let _slides = map(
        TestClient::connect(&mut compositor),
        &mut compositor,
        APP,
        (800, 600),
        [30, 30, 30, 255],
    );
    // Another program names its window the slides' audience.
    let other = map(
        TestClient::connect(&mut compositor),
        &mut compositor,
        AUDIENCE,
        (300, 200),
        [200, 0, 0, 255],
    );
    assert_eq!(
        compositor.presenting(),
        None,
        "only a window of the app's own connection is its audience"
    );
    let (_, rect) = window(&compositor, AUDIENCE).unwrap();
    assert_eq!((rect.2, rect.3), (300, 200), "an ordinary window");
    drop(other);
}
