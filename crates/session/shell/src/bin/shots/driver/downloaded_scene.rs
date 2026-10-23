//! Downloaded programs in the person's apps: one added from its `AppImage`, with
//! its own icon, and one whose file is gone.
use std::time::Duration;

use desktop_entry::downloaded::{Icon, Identity, Program, ProgramKind, Store};

use super::ShellDriver;

impl ShellDriver {
    /// Aseprite, added from `~/Downloads/Aseprite-1.3.AppImage`, and Ardour,
    /// added from a tarball's folder that was deleted since. Written into
    /// this run's private home, as adding them would.
    pub(super) fn seed_downloaded_apps(&mut self) {
        let (Some(home), Some(store)) = (tilo_paths::home(), Store::for_session()) else {
            return;
        };
        let identity = |key: &str, name: &str, icon: Option<Icon>| Identity {
            key: key.to_owned(),
            name: name.to_owned(),
            icon,
            comment: None,
            categories: vec!["Graphics".to_owned()],
            keywords: Vec::new(),
            mime_types: Vec::new(),
            startup_wm_class: None,
            field_code: Some("%F".to_owned()),
        };
        let program = |path: std::path::PathBuf, kind: ProgramKind| {
            use std::os::unix::fs::PermissionsExt as _;
            if let Some(parent) = path.parent() {
                let _ = std::fs::create_dir_all(parent);
            }
            let _ = std::fs::write(&path, b"\x7fELF");
            let _ = std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755));
            Program { path, kind }
        };
        let aseprite = program(
            home.join("Downloads/Aseprite-1.3.AppImage"),
            ProgramKind::AppImage,
        );
        let _ = store.add(
            &identity(
                "com.aseprite.Aseprite",
                "Aseprite",
                icon([0x3d, 0x6a, 0xe0]),
            ),
            &aseprite,
        );
        let ardour = program(
            home.join("Downloads/Ardour-8.4/ardour"),
            ProgramKind::Program,
        );
        let _ = store.add(
            &identity("ardour", "Ardour", icon([0xe0, 0x8a, 0x2b])),
            &ardour,
        );
        let _ = std::fs::remove_dir_all(home.join("Downloads/Ardour-8.4"));
        desktop_entry::cache::invalidate();
        // Let the applications index take them in, as it would long before
        // anyone types.
        self.fb.host_mut().set_palette_visible(true);
        self.fb.host_mut().set_palette_query("a");
        let _ = self.paint_settled();
        std::thread::sleep(Duration::from_millis(400));
        let _ = self.paint_settled();
    }

    pub(super) fn scene_downloaded_query(&mut self, query: &str) {
        self.seed_downloaded_apps();
        self.scene_palette_query(query);
        self.let_icons_arrive(query);
    }

    /// The launcher (Super+N) with the "Host apps" chip chosen and nothing
    /// typed: every app Tilo did not package, the added downloads among
    /// them.
    pub(super) fn scene_downloaded_launcher(&mut self) {
        self.seed_downloaded_apps();
        self.fb.host_mut().filter_palette_to_host_apps_for_shots();
        self.scene_palette_query("");
        self.let_icons_arrive("");
    }

    /// App icons are read and rasterised on their resolver's thread; the
    /// capture waits for them as a person's eye would.
    /// The rows take their artwork when they are built, so they are built
    /// again once it has been found.
    fn let_icons_arrive(&mut self, query: &str) {
        let _ = self.paint_settled();
        std::thread::sleep(Duration::from_millis(700));
        let _ = self.paint_settled();
        self.scene_palette_query(&format!("{query} "));
        self.scene_palette_query(query);
        // Painting asks for the pixels at the row's size, decoded on the
        // same thread.
        let _ = self.paint_settled();
        std::thread::sleep(Duration::from_millis(700));
        let _ = self.paint_settled();
    }
}

/// A 128-pixel rounded square in `rgb` with a lighter disc: an app icon's
/// shape, so the row shows the entry's own artwork.
fn icon(rgb: [u8; 3]) -> Option<Icon> {
    const EDGE: u32 = 128;
    let mut image = image::RgbaImage::new(EDGE, EDGE);
    for (x, y, pixel) in image.enumerate_pixels_mut() {
        let (fx, fy) = (f64::from(x) + 0.5, f64::from(y) + 0.5);
        let corner = 28.0_f64;
        let edge = f64::from(EDGE);
        let dx = (corner - fx).max(fx - (edge - corner)).max(0.0);
        let dy = (corner - fy).max(fy - (edge - corner)).max(0.0);
        if dx.hypot(dy) > corner {
            continue;
        }
        let disc = (fx - 64.0).hypot(fy - 64.0) < 34.0;
        let lift = |c: u8| if disc { c.saturating_add(70) } else { c };
        *pixel = image::Rgba([lift(rgb[0]), lift(rgb[1]), lift(rgb[2]), 0xff]);
    }
    let mut bytes = std::io::Cursor::new(Vec::new());
    image.write_to(&mut bytes, image::ImageFormat::Png).ok()?;
    Icon::sniff(bytes.into_inner())
}
