//! An HDR or colour-managed app behind a keeper: the description it sets on a window reaches the
//! desktop, and comes back with the window's first frame after a restart; what the desktop prefers
//! reaches the app; what it creates while no desktop is attached waits for the next one; and a
//! desktop that supports less fails what it cannot serve the way the protocol defines.

use std::collections::HashMap;

use neighbour_wire::colorimetry::{ColorPrimaries, Colorimetry, TransferCharacteristics};
use neighbour_wire::hdr_metadata::Hdr10Metadata;
use wayland_client::backend::ObjectId;
use wayland_client::{Connection, Dispatch, Proxy as _, QueueHandle, WEnum, delegate_noop};
use wayland_protocols::wp::color_management::v1::client::{
    wp_color_management_output_v1::WpColorManagementOutputV1,
    wp_color_management_surface_feedback_v1::{self, WpColorManagementSurfaceFeedbackV1},
    wp_color_management_surface_v1::WpColorManagementSurfaceV1,
    wp_color_manager_v1::{
        self, Feature, Primaries, RenderIntent, TransferFunction, WpColorManagerV1,
    },
    wp_image_description_creator_icc_v1::WpImageDescriptionCreatorIccV1,
    wp_image_description_creator_params_v1::WpImageDescriptionCreatorParamsV1,
    wp_image_description_info_v1::{self, WpImageDescriptionInfoV1},
    wp_image_description_v1::{self, Cause, WpImageDescriptionV1},
};

use crate::support::{App, Heard, Rig};

const SIZE: (i32, i32) = (64, 48);
const GREY: [u8; 4] = [128, 128, 128, 255];

/// What the app heard of colour management.
#[derive(Default)]
pub struct ColorSeen {
    pub manager: Option<WpColorManagerV1>,
    pub features: Vec<u32>,
    pub done: bool,
    /// The identity each ready description was told.
    pub ready: HashMap<ObjectId, u32>,
    /// The cause each failed description was told.
    pub failed: HashMap<ObjectId, u32>,
    pub preferred_changed: Vec<u32>,
    /// What each information object said, and whether it was done.
    pub info: HashMap<ObjectId, (Vec<String>, bool)>,
}

/// What the desktop says the only window's content is in.
fn window_color(rig: &mut Rig) -> Option<(Colorimetry, Option<Hdr10Metadata>)> {
    let id = rig.desktop().windows().first()?.0.id;
    rig.desktop().window_color(id)
}

fn manager(rig: &mut Rig, app: &mut App) -> WpColorManagerV1 {
    rig.pump(&mut [app], |_, apps| apps[0].heard.color.done);
    app.heard
        .color
        .manager
        .clone()
        .expect("the keeper offers colour management")
}

/// A PQ BT.2020 description mastered at 0.005 to 1000 cd/m², as an HDR video player makes.
fn hdr_description(app: &App, manager: &WpColorManagerV1) -> WpImageDescriptionV1 {
    let qh = app.queue.handle();
    let creator = manager.create_parametric_creator(&qh, ());
    creator.set_primaries_named(Primaries::Bt2020);
    creator.set_tf_named(TransferFunction::St2084Pq);
    creator.set_mastering_luminance(50, 1_000);
    creator.set_max_cll(800);
    creator.create(&qh, ())
}

fn ready(rig: &mut Rig, app: &mut App, desc: &WpImageDescriptionV1) -> u32 {
    let id = desc.id();
    rig.pump(&mut [app], |_, apps| {
        apps[0].heard.color.ready.contains_key(&id) || apps[0].heard.color.failed.contains_key(&id)
    });
    assert!(
        !app.heard.color.failed.contains_key(&id),
        "the description failed"
    );
    app.heard.color.ready[&id]
}

fn assert_hdr(rig: &mut Rig) {
    let (color, hdr) = window_color(rig).expect("the window carries the app's description");
    assert_eq!(color.primaries, ColorPrimaries::Bt2020);
    assert_eq!(color.transfer, TransferCharacteristics::SmpteSt2084);
    let hdr = hdr.expect("PQ carries its mastering volume");
    assert_eq!(hdr.mastering.max_display_mastering_luminance, 10_000_000);
    assert_eq!(hdr.mastering.min_display_mastering_luminance, 50);
    assert_eq!(hdr.light.max_content_light_level, 800);
}

/// Restart the desktop and wait for the window's first frame on the new one.
fn restart(rig: &mut Rig, app: &mut App, configure: impl FnOnce(&mut tilo_compositor::Compositor)) {
    rig.drop_desktop();
    rig.settle(&mut [app]);
    rig.attach_new_with(configure);
    rig.pump(&mut [app], |rig, _| !rig.windows().is_empty());
}

#[test]
fn an_hdr_window_shows_its_description_and_its_first_frame_after_a_restart_carries_it() {
    let mut rig = Rig::new(None);
    let mut app = App::connect(&mut rig);
    app.open_window(&mut rig, "Film", Some("player"));
    let manager = manager(&mut rig, &mut app);
    let desc = hdr_description(&app, &manager);
    ready(&mut rig, &mut app, &desc);
    let surface = app.surface.clone().unwrap();
    let color = manager.get_surface(&surface, &app.queue.handle(), ());
    color.set_image_description(&desc, RenderIntent::Perceptual);
    app.show(&mut rig, SIZE, GREY);
    assert_hdr(&mut rig);

    // The pump stops at the first moment the new desktop shows the window: the description is
    // already there.
    restart(&mut rig, &mut app, |_| {});
    assert_hdr(&mut rig);

    // Unset is applied with the next commit.
    color.unset_image_description();
    app.draw(&surface, SIZE, GREY);
    rig.pump(&mut [&mut app], |rig, _| window_color(rig).is_none());
}

#[test]
fn a_surface_waits_for_a_description_that_is_not_ready_and_shows_without_one_that_failed() {
    let mut rig = Rig::new(None);
    let mut app = App::connect(&mut rig);
    app.open_window(&mut rig, "Film", Some("player"));
    let manager = manager(&mut rig, &mut app);
    let desc = hdr_description(&app, &manager);
    ready(&mut rig, &mut app, &desc);
    let surface = app.surface.clone().unwrap();
    manager
        .get_surface(&surface, &app.queue.handle(), ())
        .set_image_description(&desc, RenderIntent::Perceptual);
    app.show(&mut rig, SIZE, GREY);
    assert_hdr(&mut rig);

    // A new desktop holds every description it is asked for: until one is ready, no content of
    // the surface that names it reaches the desktop.
    rig.drop_desktop();
    rig.settle(&mut [&mut app]);
    rig.attach_new_with(|desktop| desktop.hold_color_descriptions());
    for _ in 0..10 {
        rig.settle(&mut [&mut app]);
    }
    assert!(
        rig.windows().is_empty(),
        "nothing reached the desktop before the description was ready"
    );
    rig.desktop().release_color_descriptions();
    rig.pump(&mut [&mut app], |rig, _| !rig.windows().is_empty());
    assert_hdr(&mut rig);

    // A desktop that refuses the description: the window shows without it.
    rig.drop_desktop();
    rig.settle(&mut [&mut app]);
    rig.attach_new_with(|desktop| desktop.hold_color_descriptions());
    for _ in 0..10 {
        rig.settle(&mut [&mut app]);
    }
    assert!(rig.windows().is_empty());
    rig.desktop()
        .fail_color_descriptions("the desktop does not serve it");
    rig.pump(&mut [&mut app], |rig, _| !rig.windows().is_empty());
    assert!(
        window_color(&mut rig).is_none(),
        "the window showed without the description it could not have"
    );
    assert!(!app.broken);
    assert_eq!(rig.keeper.describe().fault, None);
}

#[test]
fn an_scrgb_window_keeps_its_description_and_a_desktop_without_scrgb_fails_it() {
    let mut rig = Rig::new(None);
    let mut app = App::connect(&mut rig);
    app.open_window(&mut rig, "Game", Some("game"));
    let manager = manager(&mut rig, &mut app);
    assert!(
        app.heard
            .color
            .features
            .contains(&(Feature::WindowsScrgb as u32))
    );
    let qh = app.queue.handle();
    let scrgb = manager.create_windows_scrgb(&qh, ());
    ready(&mut rig, &mut app, &scrgb);
    let surface = app.surface.clone().unwrap();
    let feedback = manager.get_surface_feedback(&surface, &qh, ());
    let preferred = feedback.get_preferred(&qh, ());
    let preferred = ready(&mut rig, &mut app, &preferred);
    manager
        .get_surface(&surface, &qh, ())
        .set_image_description(&scrgb, RenderIntent::Perceptual);
    app.show(&mut rig, SIZE, GREY);
    let (color, _) = window_color(&mut rig).expect("the scRGB description");
    assert_eq!(color.transfer, TransferCharacteristics::Linear);
    assert_eq!(color.primaries, ColorPrimaries::Bt709);

    restart(&mut rig, &mut app, |_| {});
    let (color, _) = window_color(&mut rig).expect("the scRGB description after a restart");
    assert_eq!(color.transfer, TransferCharacteristics::Linear);

    // A desktop on a display without scRGB: the window shows without its description, and the
    // app is pointed at what the desktop prefers instead.
    let without_scrgb: Vec<u32> = [1, 2, 3, 4, 5].into();
    restart(&mut rig, &mut app, |desktop| {
        desktop.limit_color_features(&without_scrgb);
    });
    assert!(window_color(&mut rig).is_none());
    rig.pump(&mut [&mut app], |_, apps| {
        apps[0].heard.color.preferred_changed.contains(&preferred)
    });
    // The app was told scRGB is supported: one it makes now fails, gracefully.
    let again = manager.create_windows_scrgb(&qh, ());
    let id = again.id();
    rig.pump(&mut [&mut app], |_, apps| {
        apps[0].heard.color.failed.contains_key(&id)
    });
    assert_eq!(app.heard.color.failed[&id], Cause::Unsupported as u32);
    assert!(!app.broken, "nothing ended the app");
    assert_eq!(rig.keeper.describe().fault, None);
}

#[test]
fn the_desktops_preferred_description_reaches_the_app_and_survives_a_restart() {
    let mut rig = Rig::new(None);
    let mut app = App::connect(&mut rig);
    app.open_window(&mut rig, "Viewer", Some("viewer"));
    let manager = manager(&mut rig, &mut app);
    let qh = app.queue.handle();
    let surface = app.surface.clone().unwrap();
    let feedback = manager.get_surface_feedback(&surface, &qh, ());
    let preferred = feedback.get_preferred_parametric(&qh, ());
    let identity = ready(&mut rig, &mut app, &preferred);
    let info = preferred.get_information(&qh, ());
    let info_id = info.id();
    rig.pump(&mut [&mut app], |_, apps| {
        apps[0]
            .heard
            .color
            .info
            .get(&info_id)
            .is_some_and(|(_, done)| *done)
    });
    let facts = app.heard.color.info[&info_id].0.clone();
    assert!(facts.contains(&"primaries_named 1".to_owned()), "{facts:?}");
    assert!(facts.contains(&"tf_named 1".to_owned()), "{facts:?}");

    // The app shows in the description the desktop prefers.
    manager
        .get_surface(&surface, &qh, ())
        .set_image_description(&preferred, RenderIntent::Perceptual);
    app.show(&mut rig, SIZE, GREY);
    let (color, _) = window_color(&mut rig).expect("the preferred description");
    assert_eq!(color.primaries, ColorPrimaries::Bt709);

    // While no desktop is attached the app is answered from what the last one said.
    rig.drop_desktop();
    rig.settle(&mut [&mut app]);
    let away = feedback.get_preferred(&qh, ());
    assert_eq!(ready(&mut rig, &mut app, &away), identity);

    // A desktop that prefers the same is no news, and the window carries the description again
    // from its first frame there.
    rig.attach_new(640, 480);
    rig.pump(&mut [&mut app], |rig, _| !rig.windows().is_empty());
    let (color, _) = window_color(&mut rig).expect("the preferred description after a restart");
    assert_eq!(color.primaries, ColorPrimaries::Bt709);
    rig.settle(&mut [&mut app]);
    assert!(app.heard.color.preferred_changed.is_empty());
    let again = feedback.get_preferred(&qh, ());
    assert_eq!(ready(&mut rig, &mut app, &again), identity);

    // What an output displays comes from the desktop too.
    let output = app.heard.outputs.first().cloned().expect("an output");
    let described = manager.get_output(&output, &qh, ());
    let displayed = described.get_image_description(&qh, ());
    ready(&mut rig, &mut app, &displayed);
}

#[test]
fn a_description_made_while_no_desktop_is_attached_waits_for_the_next_one() {
    let mut rig = Rig::new(None);
    let mut app = App::connect(&mut rig);
    app.open_window(&mut rig, "Film", Some("player"));
    let manager = manager(&mut rig, &mut app);
    app.show(&mut rig, SIZE, GREY);
    rig.drop_desktop();
    rig.settle(&mut [&mut app]);

    let desc = hdr_description(&app, &manager);
    rig.settle(&mut [&mut app]);
    let id = desc.id();
    assert!(!app.heard.color.ready.contains_key(&id));
    assert!(!app.heard.color.failed.contains_key(&id));

    rig.attach_new(640, 480);
    ready(&mut rig, &mut app, &desc);
    let surface = app.surface.clone().unwrap();
    manager
        .get_surface(&surface, &app.queue.handle(), ())
        .set_image_description(&desc, RenderIntent::Perceptual);
    app.draw(&surface, SIZE, GREY);
    rig.pump(&mut [&mut app], |rig, _| window_color(rig).is_some());
    assert_hdr(&mut rig);
}

#[test]
fn an_icc_creator_ends_only_that_app_as_the_desktop_would() {
    let mut rig = Rig::new(None);
    let mut app = App::connect(&mut rig);
    let manager = manager(&mut rig, &mut app);
    assert!(
        !app.heard
            .color
            .features
            .contains(&(Feature::IccV2V4 as u32))
    );
    manager.create_icc_creator(&app.queue.handle(), ());
    rig.pump(&mut [&mut app], |_, apps| apps[0].broken);
    let error = app.connection.protocol_error().expect("a protocol error");
    assert_eq!(error.object_interface, "wp_color_manager_v1");
    assert_eq!(
        error.code,
        wp_color_manager_v1::Error::UnsupportedFeature as u32
    );

    let mut other = App::connect(&mut rig);
    manager_of(&mut rig, &mut other);
    assert_eq!(rig.keeper.describe().fault, None);
}

fn manager_of(rig: &mut Rig, app: &mut App) {
    rig.pump(&mut [app], |_, apps| apps[0].heard.color.done);
}

impl Dispatch<WpColorManagerV1, ()> for Heard {
    fn event(
        heard: &mut Self,
        _: &WpColorManagerV1,
        event: wp_color_manager_v1::Event,
        (): &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        match event {
            wp_color_manager_v1::Event::SupportedFeature {
                feature: WEnum::Value(feature),
            } => heard.color.features.push(feature as u32),
            wp_color_manager_v1::Event::Done => heard.color.done = true,
            _ => {}
        }
    }
}

impl Dispatch<WpImageDescriptionV1, ()> for Heard {
    fn event(
        heard: &mut Self,
        desc: &WpImageDescriptionV1,
        event: wp_image_description_v1::Event,
        (): &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        match event {
            wp_image_description_v1::Event::Ready { identity } => {
                heard.color.ready.insert(desc.id(), identity);
            }
            wp_image_description_v1::Event::Failed { cause, .. } => {
                heard.color.failed.insert(desc.id(), u32::from(cause));
            }
            _ => {}
        }
    }
}

impl Dispatch<WpImageDescriptionInfoV1, ()> for Heard {
    fn event(
        heard: &mut Self,
        info: &WpImageDescriptionInfoV1,
        event: wp_image_description_info_v1::Event,
        (): &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        use wp_image_description_info_v1::Event;
        let entry = heard.color.info.entry(info.id()).or_default();
        let fact = match event {
            Event::Done => {
                entry.1 = true;
                return;
            }
            Event::PrimariesNamed { primaries } => {
                format!("primaries_named {}", u32::from(primaries))
            }
            Event::TfNamed { tf } => format!("tf_named {}", u32::from(tf)),
            Event::Luminances {
                min_lum,
                max_lum,
                reference_lum,
            } => format!("luminances {min_lum} {max_lum} {reference_lum}"),
            _ => "other".to_owned(),
        };
        entry.0.push(fact);
    }
}

impl Dispatch<WpColorManagementSurfaceFeedbackV1, ()> for Heard {
    fn event(
        heard: &mut Self,
        _: &WpColorManagementSurfaceFeedbackV1,
        event: wp_color_management_surface_feedback_v1::Event,
        (): &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        if let wp_color_management_surface_feedback_v1::Event::PreferredChanged { identity } = event
        {
            heard.color.preferred_changed.push(identity);
        }
    }
}

delegate_noop!(Heard: ignore WpColorManagementOutputV1);
delegate_noop!(Heard: ignore WpImageDescriptionCreatorParamsV1);
delegate_noop!(Heard: ignore WpImageDescriptionCreatorIccV1);
delegate_noop!(Heard: ignore WpColorManagementSurfaceV1);
