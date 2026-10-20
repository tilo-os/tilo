//! The search sources the shots stage once for every scene: Flathub's
//! cached apps, Tilo apps the registries offer, and a Passwords vault.
use std::time::Duration;

use super::ShellDriver;

impl ShellDriver {
    /// What universal search reaches beyond the apps here: Flathub's cached
    /// apps, Tilo apps the registries offer that this device lacks, and a
    /// vault Passwords indexed and left unlocked before it closed.
    #[allow(
        clippy::too_many_lines,
        reason = "one screenshot fixture, top to bottom"
    )]
    pub(super) fn seed_search_sources(&mut self) {
        // One device across the scenes: staged once, so each scene's query
        // meets indexes already built, as every keystroke after the first
        // does.
        use app_catalogue::{CatalogueEntry, Installation, Source};
        use shell::shell::catalogue::CatalogueApp;
        use ui::search::index_publication::{IndexDelta, IndexEntry, IndexWeight};
        static SEEDED: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);
        if SEEDED.swap(true, std::sync::atomic::Ordering::SeqCst) {
            return;
        }
        let flathub = |id: &str, name: &str, tagline: &str, category: &str| CatalogueEntry {
            id: id.to_owned(),
            source: Source::Flathub {
                remote: "flathub".to_owned(),
                installation: Installation::System,
            },
            name: name.to_owned(),
            other_names: Vec::new(),
            tagline: tagline.to_owned(),
            description: String::new(),
            keywords: Vec::new(),
            category: Some(category.to_owned()),
            icon_file: None,
            icon_url: None,
            resource_types: Vec::new(),
            actions: Vec::new(),
        };
        let offered = |id: &str, name: &str, category: &str| CatalogueApp {
            package_id: id.to_owned(),
            display_name: name.to_owned(),
            category: Some(category.to_owned()),
            version: "1.4.0".to_owned(),
            registry: "Tilo".to_owned(),
            trust: "publisher-signed".to_owned(),
            ..CatalogueApp::default()
        };
        shell::shell::catalogue::stage_for_shots(
            vec![
                offered("so.tilo.sheets", "Sheets", "Office"),
                offered("so.tilo.calendar", "Calendar", "Office"),
                offered("so.tilo.file-explorer", "Files", "System"),
            ],
            vec![
                flathub(
                    "com.spotify.Client",
                    "Spotify",
                    "Online music streaming service",
                    "Audio",
                ),
                flathub(
                    "dev.alextren.Spot",
                    "Spot",
                    "Listen to music on Spotify",
                    "Audio",
                ),
                flathub(
                    "org.gimp.GIMP",
                    "GNU Image Manipulation Program",
                    "Create images and edit photographs",
                    "Graphics",
                ),
                flathub(
                    "io.github.shiftey.Desktop",
                    "GitHub Desktop",
                    "Simple collaboration from your desktop",
                    "Development",
                ),
            ],
        );
        let room = {
            let handle = self.chrome_handle();
            let st = handle.borrow();
            st.tree()
                .desktop
                .active_workspace()
                .map(|workspace| workspace.room.to_string())
        };
        // A live session's broker answers apps' content (its search) and
        // opens a hit; the capture's stands in for it.
        #[cfg(feature = "test-support")]
        self.fb.host_mut().attach_app_action_broker(
            shell::shell::collab_broker_capnp::CapnpBrokerState::test_broker_with_idle_directory_queue(),
        );
        // A device has its catalogue indexed long before anyone types: ask
        // once so the background index builds, and let it land, so the
        // scene's query is answered from it as a keystroke is.
        // A frame with the palette open refreshes the indexes; the builds
        // land on a later one.
        self.fb.host_mut().set_palette_visible(true);
        self.fb.host_mut().set_palette_query("a");
        let _ = self.paint_settled();
        std::thread::sleep(Duration::from_millis(400));
        let _ = self.paint_settled();
        let Some(room) = room else { return };
        let login = |key: &str, site: &str, user: &str| IndexEntry {
            key: key.to_owned(),
            resource: key.to_owned(),
            action: None,
            title: site.to_owned(),
            subtitle: Some(user.to_owned()),
            group: Some("Passwords".to_owned()),
            icon: Some("globe".to_owned()),
            boost: 0,
            fields: vec![
                (site.to_ascii_lowercase(), IndexWeight::Identity),
                (user.to_owned(), IndexWeight::Identity),
            ],
            secret: true,
        };
        shell::shell::search::app_content_index::session().apply(
            &shell::shell::search::app_content_index::SlotKey::new(
                "so.tilo.passwords",
                &room,
                "passwords:vault",
            ),
            IndexDelta {
                reset: true,
                upsert: vec![
                    login("passwords.entry:github", "github.com", "ana.kowalski"),
                    login("passwords.entry:gitlab", "gitlab.com", "ana@kowalski.dev"),
                    login("passwords.entry:mail", "mail.example.org", "ana"),
                ],
                remove: Vec::new(),
                secrets_unlocked: Some(true),
            },
        );
    }
}
