//! The Displays pane's screen settings: high dynamic range, the brightness of ordinary content in
//! it, a variable refresh rate and the colour profile a screen offers, each set through the
//! session's published displays resource.

use std::cell::RefCell;
use std::path::Path;
use std::rc::Rc;

use tilo_action_policy::{MemberAccess, Role, WorkspacePolicy};
use tilolang_runtime::delegate::{Invocation, Invoked, Invoker};
use tilolang_runtime::{Actor, ResourceRef, Value, ViewInstance};
use tilolang_tilo::host::Host;

const APP: &str = "so.tilo.settings";

/// The app as it ships: its source, its ids and the platform's packages it reads through.
fn settings() -> Host {
    let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../../apps/tl/settings");
    let source = tilolang::PackageSource::read_dir(&dir).expect("the package reads");
    let ids = tilolang::IdsLock::parse(
        &std::fs::read_to_string(dir.join("app.ids")).expect("the package's ids"),
    )
    .expect("its ids parse");
    let metas = crate::fixtures::settings_packages().expect("the platform's packages read");
    let compiled =
        tilolang::compile_package_source(&source, &ids, &metas).expect("the package compiles");
    let mut policy = WorkspacePolicy::new("team", "ada");
    policy.members.insert("ada".to_owned(), Role::Owner);
    let owner = MemberAccess::person(policy, "ada");
    let host = Host::new(
        Actor::from(tilolang_tilo::runtime::ids::MemberId::person("ada")),
        move || Some(owner.clone()),
    )
    .expect("a host");
    host.load_compiled(&compiled).expect("the package loads");
    tilolang_tilo::platform_apps::load(&host, &compiled.meta.uses);
    crate::fixtures::settings_sections(&host);
    host
}

/// The person's devices, with the screens each says it drives, as the session publishes them.
fn devices() -> ResourceRef {
    ResourceRef::parse_id("session:devices", "so.tilo.device#Devices").expect("a devices resource")
}

/// This device's displays, as the session publishes them.
fn displays() -> ResourceRef {
    ResourceRef::parse_id("session:displays", "so.tilo.device#Displays")
        .expect("a displays resource")
}

/// The Displays pane as its own view, drawn.
fn displays_pane(host: &Host) -> ViewInstance {
    let id = host
        .package(APP)
        .expect("the app is loaded")
        .meta
        .views
        .iter()
        .find(|view| view.name == "displays_pane")
        .expect("the Displays pane is declared")
        .id
        .clone();
    let mut view = host
        .runtime()
        .open_view(
            &Actor::from(tilolang_tilo::runtime::ids::MemberId::person("ada")),
            &id,
            Vec::new(),
        )
        .expect("the pane opens");
    view.poll_patches().expect("the pane draws");
    view
}

/// The lines a view shows, its text as written.
fn texts(view: &ViewInstance) -> Vec<String> {
    crate::fixtures::nodes(view.tree())
        .into_iter()
        .flat_map(|node| node.props.values())
        .filter_map(|value| match value {
            Value::Text(text) => Some(text.clone()),
            Value::Rich(rich) => Some(rich.written_text()),
            _ => None,
        })
        .collect()
}

/// The kinds of node the view draws, for a failure that says what it drew.
fn kinds(view: &ViewInstance) -> Vec<String> {
    crate::fixtures::nodes(view.tree())
        .into_iter()
        .map(|node| node.kind.clone())
        .collect()
}

/// The key of the control of `kind` whose label `label` reads, as the pane lays it out.
fn labelled(view: &ViewInstance, kind: &str, label: &str) -> String {
    view.tree()
        .of_kind(kind)
        .into_iter()
        .find(|node| node.props.get("label") == Some(&Value::text(label)))
        .unwrap_or_else(|| {
            panic!(
                "a {kind} labelled {label}: {:?} {:?}",
                texts(view),
                kinds(view)
            )
        })
        .key
        .clone()
}

/// One invocation as it was sent, with its arguments by name.
#[derive(Clone, Default)]
struct Sent(Rc<RefCell<Vec<Invocation>>>);

impl Sent {
    /// The invocation of `app`'s `name`, with its arguments by name.
    fn asked_on(&self, app: &str, name: &str) -> Vec<(String, Value)> {
        self.0
            .borrow()
            .iter()
            .rev()
            .find_map(|invocation| match invocation {
                Invocation::Action {
                    app: asked_app,
                    name: asked,
                    args,
                    ..
                } if asked == name && asked_app.as_str() == app => Some(args.clone()),
                _ => None,
            })
            .unwrap_or_else(|| panic!("the pane asks {app}'s `{name}`: {:?}", self.0.borrow()))
    }

    /// The device resource's invocation of `name`, with its arguments by name.
    fn asked(&self, name: &str) -> Vec<(String, Value)> {
        self.asked_on("so.tilo.device", name)
    }
}

impl Invoker for Sent {
    fn invoke(&self, invocation: &Invocation) -> Invoked {
        self.0.borrow_mut().push(invocation.clone());
        Invoked::Done(Value::Unit)
    }
}

/// The value of `named` among `args` .
fn arg(args: &[(String, Value)], named: &str) -> Value {
    args.iter()
        .find(|(name, _)| name == named)
        .map(|(_, value)| value.clone())
        .unwrap_or_else(|| panic!("an argument `{named}`: {args:?}"))
}

/// A screen that can show HDR and vary its refresh rate, offering two colour profiles.
const READING: &str = r#"{
  "displays": [
    {
      "connector": "DP-1",
      "name": "Studio display",
      "brightness": null,
      "backlight": false,
      "colour": {
        "max_luminance_nits": 1000.0,
        "supports_pq_output": true,
        "supports_hlg_output": false,
        "bt2020_gamut": true
      },
      "x": 0, "y": 0, "width": 2560, "height": 1440, "scale": 1.0,
      "transform": "normal", "primary": true,
      "mode": {"width": 2560, "height": 1440, "refresh_mhz": 60000},
      "modes": [{"width": 2560, "height": 1440, "refresh_mhz": 60000}],
      "hdr": true, "hdr_on": false, "sink_hdr": true, "hdr_unavailable": null,
      "sdr_brightness_nits": 203,
      "vrr": true, "vrr_on": true, "sink_vrr": true, "vrr_unavailable": null,
      "colour_profile": null,
      "colour_profiles": [
        {"key": "/usr/share/color/icc/studio.icc", "label": "Studio"},
        {"key": "/usr/share/color/icc/srgb.icc", "label": "sRGB"}
      ]
    }
  ],
  "trusted_tools": [{"executable": "/usr/bin/kanshi", "name": "kanshi"}]
}"#;

#[test]
fn the_pane_sets_a_screen_s_hdr_its_sdr_brightness_vrr_and_its_colour_profile() {
    let host = settings();
    host.runtime()
        .set_reading(&displays(), READING)
        .expect("the displays are set");
    let mut view = displays_pane(&host);
    let sent = Sent::default();
    host.runtime().set_invoker(sent.clone());

    // Turning HDR on: the pane asks the session for this screen.
    let hdr = labelled(&view, "ui.Toggle", "HDR");
    view.fire_with(&hdr, "check", Some(&Value::Bool(true)))
        .expect("HDR turns on");
    let asked = sent.asked("set-display-hdr");
    assert_eq!(arg(&asked, "display"), Value::text("DP-1"));
    // A switch's value reaches the device as the optional its event carries, which the host
    // hands over as the plain answer.
    assert_eq!(
        arg(&asked, "on"),
        Value::Opt(Some(Box::new(Value::Bool(true))))
    );

    // How bright ordinary content looks in HDR, in cd/m².
    let brightness = labelled(&view, "ui.Slider", "SDR content brightness");
    view.fire_with(&brightness, "change", Some(&Value::Float(300.0)))
        .expect("the brightness moves");
    let asked = sent.asked("set-display-sdr-brightness");
    assert_eq!(arg(&asked, "display"), Value::text("DP-1"));
    assert_eq!(arg(&asked, "nits"), Value::Int(300));

    // The refresh rate varies only while the person lets it.
    let vrr = labelled(&view, "ui.Toggle", "Variable refresh rate");
    view.fire_with(&vrr, "check", Some(&Value::Bool(false)))
        .expect("VRR is turned off");
    let asked = sent.asked("set-display-vrr");
    assert_eq!(arg(&asked, "display"), Value::text("DP-1"));
    assert_eq!(
        arg(&asked, "on"),
        Value::Opt(Some(Box::new(Value::Bool(false))))
    );

    // The colour profile it shows, by the id the reading names.
    let srgb = labelled(&view, "ui.Segment", "sRGB");
    view.press(&srgb).expect("the profile is chosen");
    let asked = sent.asked("set-display-colour-profile");
    assert_eq!(arg(&asked, "display"), Value::text("DP-1"));
    assert_eq!(
        arg(&asked, "profile"),
        Value::Opt(Some(Box::new(Value::text("/usr/share/color/icc/srgb.icc")))),
        "the chosen profile's id, not its name"
    );

    // The system default is no profile at all.
    let system = labelled(&view, "ui.Segment", "System default");
    view.press(&system).expect("the system default is chosen");
    let asked = sent.asked("set-display-colour-profile");
    assert_eq!(arg(&asked, "profile"), Value::Opt(None));
}

#[test]
fn a_screen_that_cannot_show_hdr_says_why_instead_of_offering_it() {
    let host = settings();
    let reading = READING.replace(
        r#""hdr": true, "hdr_on": false, "sink_hdr": true, "hdr_unavailable": null,"#,
        r#""hdr": false, "hdr_on": false, "sink_hdr": true, "hdr_unavailable": "driver-unsupported","#,
    );
    host.runtime()
        .set_reading(&displays(), &reading)
        .expect("the displays are set");
    let view = displays_pane(&host);
    assert!(
        view.tree()
            .of_kind("ui.Toggle")
            .into_iter()
            .all(|node| node.props.get("label") != Some(&Value::text("HDR"))),
        "no switch for what cannot be turned on"
    );
    assert!(
        texts(&view)
            .iter()
            .any(|text| text == "The graphics driver cannot send an HDR signal to this display"),
        "{:?} {:?}",
        texts(&view),
        kinds(&view)
    );
}

#[test]
fn the_pane_asks_a_trusted_display_tool_again() {
    let host = settings();
    host.runtime()
        .set_reading(&displays(), READING)
        .expect("the displays are set");
    let mut view = displays_pane(&host);
    let sent = Sent::default();
    host.runtime().set_invoker(sent.clone());

    assert!(
        texts(&view).iter().any(|text| text == "kanshi"),
        "the tool's name is shown: {:?}",
        texts(&view)
    );
    let again = labelled(&view, "ui.Button", "Ask again");
    view.press(&again).expect("the tool is asked again");
    let asked = sent.asked("forget-display-tool");
    assert_eq!(arg(&asked, "tool"), Value::text("/usr/bin/kanshi"));
}

/// This laptop's panel and the tower computer's monitor, which says it drives a screen of its
/// own: the devices' reading carries each.
const DEVICES_READING: &str = r#"{
  "devices": [
    {
      "id": "laptop", "label": "Laptop", "online": true, "nearby": true, "local": true,
      "display": true, "wakeable": false, "audio_output": false,
      "seen_ms": 1760003600000, "storage": [],
      "displays": [
        {
          "output": "edid:panel", "name": "Built-in display", "builtin": true,
          "x": 0, "y": 0, "width": 1920, "height": 1200, "connected": true, "enabled": true
        }
      ]
    },
    {
      "id": "tower", "label": "Tower computer", "online": true, "nearby": false, "local": false,
      "display": true, "wakeable": false, "audio_output": false,
      "seen_ms": 1760003000000, "storage": [],
      "displays": [
        {
          "output": "edid:tower", "name": "Tower monitor", "builtin": false,
          "x": 0, "y": 0, "width": 1920, "height": 1080, "connected": true, "enabled": true
        }
      ]
    }
  ],
  "access": [],
  "playing": null, "chosen": null, "sounds": [], "routes": [], "asks": [],
  "signed_in": true, "workspaces": [], "refused": ""
}"#;

/// The pane draws every device's screens as one layout, and dragging one moves that device's
/// screens together through the person's placement.
#[test]
fn the_pane_shows_another_device_s_screens_where_they_stand() {
    let sent = Sent::default();
    let host = settings();
    host.runtime().set_invoker(sent.clone());
    host.runtime()
        .set_reading(&displays(), READING)
        .expect("the displays are set");
    host.runtime()
        .set_reading(&devices(), DEVICES_READING)
        .expect("the person's devices are set");
    let mut view = displays_pane(&host);
    let shown = texts(&view);
    assert!(
        shown.iter().any(|text| text
            == "Your screens, left to right and top to bottom: Built-in display; Tower monitor on Tower computer"),
        "{shown:?}"
    );
    let drawing = view
        .tree()
        .of_kind("ui.Drawing")
        .into_iter()
        .next()
        .unwrap_or_else(|| {
            panic!(
                "the person's screens are drawn as one layout: {:?}",
                kinds(&view)
            )
        })
        .key
        .clone();

    // A drag from the tower's screen, a tenth of the canvas to the right.
    let point = |x: f64| Value::List(vec![Value::Float(x), Value::Float(0.5)]);
    view.fire_with(&drawing, "press", Some(&Value::Int(2)))
        .expect("the tower's screen is picked");
    view.poll_patches().expect("the picked screen is drawn");
    let picked = texts(&view);
    assert!(picked.iter().any(|text| text == "Online"), "{picked:?}");
    view.fire_with(&drawing, "drag_start", Some(&point(0.5)))
        .expect("the drag starts");
    view.fire_with(&drawing, "release_at", Some(&point(0.6)))
        .expect("the drag is let go");

    let placed = sent.asked_on("so.tilo.settings.sections", "place-display");
    assert_eq!(arg(&placed, "device"), Value::text("tower"));
    assert_eq!(arg(&placed, "output"), Value::text("edid:tower"));
    let moved = arg(&placed, "x");
    assert!(
        matches!(moved, Value::Int(x) if x > 1920),
        "the tower's screen moved right: {moved:?}"
    );
    assert_eq!(arg(&placed, "y"), Value::Int(0));
    assert!(
        sent.0.borrow().iter().all(|invocation| match invocation {
            Invocation::Action { name, args, .. } if name == "place-display" => {
                arg(args, "device") == Value::text("tower")
            }
            _ => true,
        }),
        "only the dragged device's screens move: {:?}",
        sent.0.borrow()
    );
}
