//! `shots` — headless screenshot harness for the nirios shell.
//!
//! Drives the same `build_nirios_overlay_host_with_chrome` test helper
//! the e2e + concept-parity specs use, mutates chrome state into each
//! scene of interest (desktop / overview / palette / quick-settings /
//! workspace-settings tabs / QS drill-ins), and renders each via the
//! offscreen wgpu+vello pipeline (`quick_ui::offscreen::OffscreenRenderer`).
//! No window, no compositor, no sudo — runs anywhere wgpu can find an
//! adapter (the toolbox container's llvmpipe works fine).
//!
//! Usage:
//!   shots                                           # headless, all scenes, 1600x900, ./target/nirios-shots
//!   shots --width 1920 --height 1080
//!   shots --out /tmp/shots --scenes desktop,palette,overview
//!   shots --live                                    # ALSO spawn the live winit binary via scripts/screenshot-app.py
//!   shots --live-only                               # skip headless; just spawn live (debug observation)
//!   shots --help                                    # full flags
//!
//! Headless mode writes one PNG per scene and prints the absolute path
//! of each so the caller can `Read` them directly.

use std::path::PathBuf;
use std::process::Command;

use quick_desktop::shell::build_nirios_overlay_host_with_chrome;
use quick_desktop::shell::quick_settings::QsLevel;
use quick_desktop::shell::state::WorkspaceTree;
use quick_ui::offscreen::OffscreenRenderer;
use quick_ui::{TextRenderer, Theme, Widget};

use std::collections::HashMap;
use taffy::prelude::*;
use vello::kurbo::{Affine, Point};
use vello::peniko::Color;
use vello::Scene;

const ALL_SCENES: &[&str] = &[
    "desktop",
    "overview",
    "palette",
    "quick-settings",
    "qs-wifi-drillin",
    "qs-bt-drillin",
    "ws-settings-general",
    "ws-settings-roles",
    "ws-settings-files",
];

#[derive(Debug)]
struct Args {
    width: u32,
    height: u32,
    out: PathBuf,
    scenes: Vec<String>,
    live: bool,
    live_only: bool,
}

impl Args {
    fn parse() -> Result<Self, String> {
        let mut width = 1600u32;
        let mut height = 900u32;
        let mut out = PathBuf::from("target/nirios-shots");
        let mut scenes: Vec<String> = ALL_SCENES.iter().map(|s| (*s).to_string()).collect();
        let mut live = false;
        let mut live_only = false;

        let mut argv: Vec<String> = std::env::args().skip(1).collect();
        while let Some(arg) = argv.first().cloned() {
            argv.remove(0);
            match arg.as_str() {
                "--help" | "-h" => {
                    print_help();
                    std::process::exit(0);
                }
                "--width" => {
                    let v = argv
                        .first()
                        .ok_or("--width needs a value")?
                        .parse::<u32>()
                        .map_err(|e| format!("--width: {e}"))?;
                    argv.remove(0);
                    width = v;
                }
                "--height" => {
                    let v = argv
                        .first()
                        .ok_or("--height needs a value")?
                        .parse::<u32>()
                        .map_err(|e| format!("--height: {e}"))?;
                    argv.remove(0);
                    height = v;
                }
                "--out" => {
                    let v = argv.first().ok_or("--out needs a value")?.clone();
                    argv.remove(0);
                    out = PathBuf::from(v);
                }
                "--scenes" => {
                    let v = argv.first().ok_or("--scenes needs a value")?.clone();
                    argv.remove(0);
                    scenes = v.split(',').map(|s| s.trim().to_string()).collect();
                    for s in &scenes {
                        if !ALL_SCENES.contains(&s.as_str()) {
                            return Err(format!(
                                "unknown scene '{s}'. Valid: {}",
                                ALL_SCENES.join(", ")
                            ));
                        }
                    }
                }
                "--live" => live = true,
                "--live-only" => {
                    live = true;
                    live_only = true;
                }
                other => return Err(format!("unknown arg '{other}' (try --help)")),
            }
        }

        if width == 0 || height == 0 {
            return Err("width and height must be > 0".into());
        }
        Ok(Args {
            width,
            height,
            out,
            scenes,
            live,
            live_only,
        })
    }
}

fn print_help() {
    println!(
        "shots — headless screenshot harness for nirios shell\n\
\n\
Flags:\n\
  --width N         viewport width  (default 1600)\n\
  --height N        viewport height (default 900)\n\
  --out DIR         output directory (default ./target/nirios-shots)\n\
  --scenes A,B,C    comma-separated subset of scenes (default: all)\n\
  --live            ALSO spawn the live winit binary visibly via\n\
                    scripts/screenshot-app.py for observation\n\
  --live-only       skip headless renders; just spawn the live binary\n\
\n\
Available scenes:\n  {}\n\
\n\
Examples:\n\
  shots\n\
  shots --width 1920 --height 1080 --out /tmp/shots\n\
  shots --scenes desktop,palette,overview\n\
  shots --live          # headless PNGs + live window for visual check\n",
        ALL_SCENES.join(", ")
    );
}

fn main() {
    let args = match Args::parse() {
        Ok(a) => a,
        Err(e) => {
            eprintln!("shots: {e}");
            std::process::exit(2);
        }
    };

    if !args.live_only {
        run_headless(&args);
    }

    if args.live {
        run_live(&args);
    }
}

fn run_headless(args: &Args) {
    std::fs::create_dir_all(&args.out).expect("create output dir");
    let mut renderer = OffscreenRenderer::new(args.width, args.height);
    println!(
        "shots: headless render at {}x{} → {}",
        args.width,
        args.height,
        args.out.display()
    );
    for name in &args.scenes {
        let scene = render_scene(name, args.width as f64, args.height as f64);
        let path = args.out.join(format!("{name}.png"));
        renderer.render_png(&scene, Color::TRANSPARENT, &path);
        println!("  wrote {}", path.display());
    }
}

fn run_live(args: &Args) {
    println!("shots: launching live winit binary via scripts/screenshot-app.py");
    // Locate the script + binary relative to the workspace root.
    let workspace_root = workspace_root();
    let script = workspace_root.join("scripts").join("screenshot-app.py");
    let binary = workspace_root
        .join("target")
        .join("release")
        .join("quick-desktop");
    if !binary.exists() {
        eprintln!(
            "shots: release binary not found at {} — run `cargo build --release -p quick-desktop` first",
            binary.display()
        );
        return;
    }
    let live_out = args.out.join("live-desktop.png");
    let status = Command::new("python3")
        .arg(&script)
        .arg("--launch")
        .arg(format!("distrobox enter my-distrobox -- {}", binary.display()))
        .arg("--title")
        .arg("QuickUI App")
        .arg("--out")
        .arg(&live_out)
        .arg("--settle")
        .arg("2.0")
        .status();
    match status {
        Ok(s) if s.success() => println!("  wrote {}", live_out.display()),
        Ok(s) => eprintln!("  screenshot-app.py exited with {s}"),
        Err(e) => eprintln!("  failed to spawn screenshot-app.py: {e}"),
    }
}

fn workspace_root() -> PathBuf {
    // Walk up from the binary's CARGO_MANIFEST_DIR (set at compile
    // time) until we hit the directory containing Cargo.lock.
    let mut p = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    while !p.join("Cargo.lock").exists() {
        if !p.pop() {
            return PathBuf::from(".");
        }
    }
    p
}

/// Build a Vello `Scene` for `name` at the requested viewport size.
fn render_scene(name: &str, w: f64, h: f64) -> Scene {
    let tree = WorkspaceTree::bootstrap_default();
    let (mut host, chrome) = build_nirios_overlay_host_with_chrome(&tree);

    // Per-scene state mutations BEFORE build_layout — the layout pass
    // observes overlay flags so panels reserve space correctly. The
    // palette flag lives on the host (private) rather than chrome,
    // so we use `set_palette_visible` to drive it.
    {
        let mut st = chrome.borrow_mut();
        match name {
            "desktop" => {}
            "overview" => {
                st.overlays.overview = true;
                drop(st);
                host.force_overview_settled(true);
            }
            "palette" => {
                drop(st);
                host.set_palette_visible(true);
            }
            "quick-settings" => st.overlays.quick_settings = true,
            "qs-wifi-drillin" => {
                st.overlays.quick_settings = true;
                st.quick_settings.push_level(QsLevel::Wifi);
            }
            "qs-bt-drillin" => {
                st.overlays.quick_settings = true;
                st.quick_settings.push_level(QsLevel::Bluetooth);
            }
            "ws-settings-general" => st.overlays.ws_settings = true,
            "ws-settings-roles" => {
                st.overlays.ws_settings = true;
                st.editing_workspace_name = Some("__roles_tab__".into());
            }
            "ws-settings-files" => {
                st.overlays.ws_settings = true;
                // T3's side-channel mirrors __roles_tab__'s but flips the
                // panel body to the Files grid.
                st.editing_workspace_name = Some("__files_tab__".into());
            }
            other => panic!("unknown scene '{other}'"),
        }
    }

    let theme = Theme::dark();
    let mut taffy: TaffyTree<()> = TaffyTree::new();
    let mut node_map: HashMap<NodeId, *mut dyn Widget> = HashMap::new();
    let child = host.build_layout(&mut taffy, &theme, &mut node_map);
    let root = taffy
        .new_with_children(
            Style {
                size: Size {
                    width: Dimension::length(w as f32),
                    height: Dimension::length(h as f32),
                },
                ..Default::default()
            },
            &[child],
        )
        .expect("root node");
    let mut tr = TextRenderer::new();
    let measure_map = node_map.clone();
    taffy
        .compute_layout_with_measure(
            root,
            Size {
                width: AvailableSpace::Definite(w as f32),
                height: AvailableSpace::Definite(h as f32),
            },
            |known, available, node_id, _, _| {
                if let Some(&ptr) = measure_map.get(&node_id) {
                    unsafe { (*ptr).measure(known, available, &mut tr) }
                } else {
                    taffy::Size::ZERO
                }
            },
        )
        .expect("compute_layout");
    host.apply_layout(&taffy, child, Point::ORIGIN);
    let mut scene = Scene::new();
    let presence = quick_ui::PresenceCtx::default();
    host.draw(&mut scene, Affine::IDENTITY, &mut tr, &theme, &presence);
    scene
}
