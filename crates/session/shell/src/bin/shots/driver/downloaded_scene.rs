//! Downloaded programs in the person's apps: one added from its `AppImage`, with
//! its own icon, and one whose file is gone. The artwork is real: Tilo
//! Imager's own icon, which its `AppImage` and its tarball both carry, and
//! the system's generic application icon for a program with none.
use std::time::Duration;

use desktop_entry::downloaded::{Icon, Identity, Program, ProgramKind, Store};

use super::ShellDriver;

impl ShellDriver {
    /// Tilo Imager, added from `~/Downloads/Tilo-Imager-x86_64.AppImage`,
    /// and Ardour, added from a tarball's folder that was deleted since. Written into
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
        let imager = program(
            home.join("Downloads/Tilo-Imager-x86_64.AppImage"),
            ProgramKind::AppImage,
        );
        let _ = store.add(
            &identity("org.tilo.Imager", "Tilo Imager", imager_icon()),
            &imager,
        );
        let ardour = program(
            home.join("Downloads/Ardour-8.4/ardour"),
            ProgramKind::Program,
        );
        let _ = store.add(&identity("ardour", "Ardour", None), &ardour);
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
    }

    /// The launcher (Super+N) with the Apps chip chosen and nothing typed:
    /// every app the person has, the added downloads among them, each
    /// saying which kind of app it is.
    pub(super) fn scene_downloaded_launcher(&mut self) {
        self.seed_downloaded_apps();
        self.fb.host_mut().filter_palette_to_apps_for_shots();
        self.scene_palette_query("");
    }

    /// "Add \<name\> to apps?" the first time a downloaded program runs: an
    /// `AppImage` (with Move to Applications), a program from a tarball
    /// with its icon beside it, and a loose binary with no artwork, which
    /// shows the system's generic application icon. The icon goes through
    /// the worker's own [`shell::shell::downloaded_apps::ask_icon`].
    pub(super) fn scene_downloaded_add(&mut self, kind: AddScene) {
        let ask = |name: &str, file: &str, appimage: bool, icon: Option<Icon>| {
            shell::shell::add_app_sheet::Ask {
                surface_id: 1,
                name: name.into(),
                file: file.into(),
                appimage,
                icon: shell::shell::downloaded_apps::ask_icon(icon.as_ref()),
            }
        };
        let ask = match kind {
            AddScene::AppImage => ask(
                "Tilo Imager",
                "/home/you/Downloads/Tilo-Imager-x86_64.AppImage",
                true,
                imager_icon(),
            ),
            AddScene::Tarball => ask(
                "Tilo Imager",
                "/home/you/Downloads/tilo-imager-linux-x86_64/tilo-imager",
                false,
                imager_icon(),
            ),
            AddScene::NoArtwork => ask("MyTool", "/home/you/Downloads/mytool", false, None),
        };
        let _ = self.paint_settled();
        self.host_mut().ask_add_app(ask);
        // Past the sheet's entrance.
        self.step_frames(20);
    }
}

/// Which first-run question a scene shows.
#[derive(Clone, Copy)]
pub(super) enum AddScene {
    AppImage,
    Tarball,
    NoArtwork,
}

/// Tilo Imager's icon, as its `AppImage` embeds it and its tarball ships it.
fn imager_icon() -> Option<Icon> {
    Icon::sniff(
        include_bytes!("../../../../../../../apps/imager/packaging/linux/tilo-imager.svg").to_vec(),
    )
}

/// A 128-pixel rounded square in `rgb` with a lighter disc, as PNG: an app
/// icon's shape, so a row shows the app's own artwork.
pub(super) fn app_icon_png(rgb: [u8; 3]) -> Option<Vec<u8>> {
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
    Some(bytes.into_inner())
}
