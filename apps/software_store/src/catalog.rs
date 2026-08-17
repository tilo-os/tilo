//! The bundled sample catalog.
//!
//! `--shots` and the first-run live app both start from this catalog so
//! the screenshot is fully populated offline. The live app additionally
//! merges real `flatpak list` / podman results over the install snapshot
//! (see `probe.rs`) — but the *catalog* (names, summaries, screenshots,
//! permissions) is always this curated set, because we have no offline
//! AppStream mirror.

use crate::images;
use crate::model::{AppEntry, Category, Compat, Permission, Review, Shot, Source};

fn shot(tone_a: usize, tone_b: usize, caption: &str) -> Shot {
    // `url` is filled in per-app by `app()` (below) so the offline sample
    // drives the real fetch→decode→paint image path with synthesised
    // screenshots, keeping the duotone tones only as a fallback.
    Shot { tone_a, tone_b, caption: caption.into(), url: None }
}

/// Build one catalog entry with sensible defaults; callers override the
/// interesting fields. `reviews` and `compat` default empty; the one
/// fully-fleshed detail app (GIMP) passes them explicitly.
#[allow(clippy::too_many_arguments)]
fn app(
    id: &str,
    name: &str,
    developer: &str,
    summary: &str,
    description: &str,
    category: Category,
    source: Source,
    latest: &str,
    size_mib: f64,
    rating: f32,
    rating_count: u32,
    featured: bool,
    icon_tone: usize,
    monogram: &str,
    permissions: Vec<Permission>,
    shots: Vec<Shot>,
) -> AppEntry {
    // Give the sample entry a real (synthesised) icon + per-shot bitmap URLs
    // so the offline `--shots`/first-run catalog drives the same async
    // fetch→decode→paint pipeline the live Flathub icons/screenshots use —
    // the detail page shows real decoded images, not a permanent placeholder.
    let mut shots = shots;
    for (i, s) in shots.iter_mut().enumerate() {
        s.url = Some(images::demo_shot(id, i));
    }
    AppEntry {
        id: id.into(),
        name: name.into(),
        developer: developer.into(),
        summary: summary.into(),
        description: description.into(),
        category,
        source,
        latest_version: latest.into(),
        size_mib,
        rating,
        rating_count,
        permissions,
        shots,
        featured,
        icon_tone,
        monogram: monogram.into(),
        icon_url: Some(images::demo_icon(id)),
        reviews: Vec::new(),
        compat: Vec::new(),
        download_count: None,
        remote: if source == Source::Oci { "podman".into() } else { "flathub".into() },
    }
}

/// Attach reviews to an entry (builder-style).
fn with_reviews(mut e: AppEntry, reviews: Vec<Review>) -> AppEntry {
    e.reviews = reviews;
    e
}

/// Attach compatibility notices to an entry (builder-style).
fn with_compat(mut e: AppEntry, compat: Vec<Compat>) -> AppEntry {
    e.compat = compat;
    e
}

/// Attach a download count (builder-style).
fn with_downloads(mut e: AppEntry, n: u64) -> AppEntry {
    e.download_count = Some(n);
    e
}

/// The curated sample catalog — a dozen-plus apps across every category,
/// with a couple deliberately seeded a version behind so the Updates tab
/// is populated (see [`crate::doc`]'s default seed).
pub fn sample_catalog() -> Vec<AppEntry> {
    vec![
        // ---- Graphics ----
        // GIMP is the one fully-fleshed detail app: screenshots + reviews +
        // compatibility notices, so the `detail:org.gimp.GIMP` shot is a
        // complete, populated detail page offline.
        with_downloads(
            with_compat(
                with_reviews(
                    app(
                        "org.gimp.GIMP",
                        "GIMP",
                        "The GIMP Team",
                        "Powerful image editing and compositing",
                        "GIMP is a free, cross-platform image editor with layers, masks, \
                         scriptable filters and a deep brush engine. It handles photo \
                         retouching, illustration and batch processing, and reads almost \
                         every raster format you can name.\n\nThe 3.x series adds non-destructive \
                         editing, a refreshed dark theme and a GEGL-backed pipeline for \
                         higher bit-depth work.",
                        Category::Graphics,
                        Source::Flatpak,
                        "3.0.2",
                        612.0,
                        4.6,
                        12480,
                        true,
                        6,
                        "G",
                        vec![
                            Permission::normal("Read and write your Pictures folder"),
                            Permission::normal("Wayland windowing"),
                            Permission::sensitive("Full home filesystem access"),
                        ],
                        vec![
                            shot(5, 2, "Single-window mode"),
                            shot(7, 3, "Filter gallery"),
                            shot(4, 1, "Layers & masks"),
                        ],
                    ),
                    vec![
                        Review::new(
                            "marta_k",
                            5.0,
                            "The 3.0 non-destructive workflow finally clicked for me — \
                             this is the only raster editor I keep installed.",
                        ),
                        Review::new(
                            "devnull",
                            4.0,
                            "Script-fu plus the new GEGL pipeline is unbeatable for batch \
                             work. UI still takes a day to learn but worth it.",
                        ),
                        Review::new(
                            "pix_el",
                            5.0,
                            "Runs perfectly sandboxed on qOS. Granted it the Pictures \
                             folder and never had to widen access.",
                        ),
                    ],
                ),
                vec![
                    Compat::info("Runs fully inside the cluster sandbox"),
                    // A caution, not a clean pass — surfaced with the warning
                    // affordance so "review before granting" reads as one.
                    Compat::warn("Requests broad filesystem access — review before granting"),
                ],
            ),
            41_200_000,
        ),
        app(
            "org.inkscape.Inkscape",
            "Inkscape",
            "Inkscape Project",
            "Professional vector graphics editor",
            "Inkscape is a full-featured SVG editor for illustration, icon \
             design, typography and technical drawing. Boolean operations, \
             path effects, a node editor and live filters round out a tool \
             that scales from a quick logo to a 200-layer poster.",
            Category::Graphics,
            Source::Flatpak,
            "1.3.2",
            498.0,
            4.5,
            7320,
            false,
            8,
            "Ik",
            vec![
                Permission::normal("Read and write your Documents folder"),
                Permission::normal("Printing"),
            ],
            vec![shot(8, 2, "Node editor"), shot(6, 3, "Path effects")],
        ),
        app(
            "org.blender.Blender",
            "Blender",
            "Blender Foundation",
            "3D creation suite — model, render, animate",
            "Blender is the free and open-source 3D creation suite. It supports \
             the entirety of the 3D pipeline — modelling, rigging, animation, \
             simulation, rendering, compositing and motion tracking — plus video \
             editing and a Python scripting API.",
            Category::Graphics,
            Source::Flatpak,
            "4.2.1",
            1180.0,
            4.8,
            21900,
            true,
            5,
            "B",
            vec![
                Permission::normal("GPU acceleration"),
                Permission::sensitive("Full home filesystem access"),
                Permission::normal("Read and write your Projects folder"),
            ],
            vec![
                shot(5, 1, "Sculpt mode"),
                shot(7, 2, "Cycles render"),
                shot(3, 1, "Geometry nodes"),
            ],
        ),
        // ---- Productivity ----
        app(
            "org.libreoffice.LibreOffice",
            "LibreOffice",
            "The Document Foundation",
            "The complete office productivity suite",
            "LibreOffice bundles a word processor, spreadsheet, presentation \
             tool, vector drawing app and database front-end. It reads and writes \
             Microsoft Office formats and ships a native ODF stack, with strong \
             style and template support for long documents.",
            Category::Productivity,
            Source::Flatpak,
            "24.8.0",
            720.0,
            4.4,
            15230,
            true,
            7,
            "Lo",
            vec![
                Permission::normal("Read and write your Documents folder"),
                Permission::normal("Printing"),
            ],
            vec![shot(7, 2, "Writer"), shot(5, 3, "Calc"), shot(8, 1, "Impress")],
        ),
        app(
            "md.obsidian.Obsidian",
            "Obsidian",
            "Obsidian.md",
            "A knowledge base on local Markdown files",
            "Obsidian is a note-taking app that works on a folder of plain \
             Markdown files. Backlinks, a graph view, canvas boards and a deep \
             plugin ecosystem turn scattered notes into a connected knowledge \
             base — all stored locally, yours to sync however you like.",
            Category::Productivity,
            Source::Flatpak,
            "1.6.7",
            210.0,
            4.7,
            9810,
            false,
            9,
            "Ob",
            vec![
                Permission::normal("Read and write your Notes folder"),
                Permission::sensitive("Network access"),
            ],
            vec![shot(9, 2, "Graph view"), shot(6, 3, "Canvas")],
        ),
        app(
            "org.gnome.Calendar",
            "GNOME Calendar",
            "The GNOME Project",
            "Simple, beautiful calendar for your day",
            "GNOME Calendar is a clean scheduling app that integrates with your \
             online accounts, shows day / week / month views, and keeps the \
             chrome out of the way so the events stay the focus.",
            Category::Productivity,
            Source::Flatpak,
            "47.0",
            64.0,
            4.2,
            3120,
            false,
            6,
            "Ca",
            vec![
                Permission::sensitive("Network access"),
                Permission::normal("Online accounts"),
            ],
            vec![shot(6, 2, "Month view")],
        ),
        // ---- Development ----
        with_compat(
            app(
                "com.visualstudio.code",
                "VS Code",
                "Microsoft",
                "Code editing, redefined",
                "Visual Studio Code is a lightweight but powerful source editor with \
                 built-in Git, debugging, an integrated terminal and a vast \
                 extension marketplace. IntelliSense, remote development and a tuned \
                 Electron shell make it the default editor for millions.",
                Category::Development,
                Source::Flatpak,
                "1.92.0",
                340.0,
                4.6,
                44120,
                true,
                5,
                "VS",
                vec![
                    Permission::sensitive("Network access"),
                    Permission::sensitive("Full home filesystem access"),
                    Permission::normal("Run host commands (sandbox escape)"),
                ],
                vec![
                    shot(5, 2, "Editor + terminal"),
                    shot(7, 3, "Debugger"),
                    shot(4, 1, "Source control"),
                ],
            ),
            vec![
                Compat::warn("Requests host command execution — runs outside the cluster sandbox"),
                Compat::info("Proprietary build; an open-source 'Code - OSS' variant is also available"),
            ],
        ),
        app(
            "io.podman_desktop.PodmanDesktop",
            "Podman Desktop",
            "Red Hat",
            "Manage containers and pods, graphically",
            "Podman Desktop gives a graphical home to your containers, images \
             and pods. Build from a Containerfile, inspect layers, wire up \
             Kubernetes contexts and watch logs stream — all on a rootless, \
             daemonless engine that fits qOS's immutable base.",
            Category::Development,
            Source::Oci,
            "1.12.0",
            268.0,
            4.3,
            2870,
            false,
            8,
            "Pd",
            vec![
                Permission::sensitive("Network access"),
                Permission::sensitive("Access to the container socket"),
            ],
            vec![shot(8, 2, "Containers"), shot(6, 1, "Image build")],
        ),
        app(
            "org.qbittorrent.qBittorrent",
            "qBittorrent",
            "The qBittorrent Project",
            "Lightweight, ad-free BitTorrent client",
            "qBittorrent is a fast, open-source torrent client with a built-in \
             search engine, sequential download, RSS automation and a remote \
             web UI. No ads, no bundled toolbars — just transfers.",
            Category::Internet,
            Source::Flatpak,
            "4.6.5",
            96.0,
            4.5,
            6190,
            false,
            7,
            "qB",
            vec![Permission::sensitive("Network access"), Permission::normal("Read and write your Downloads folder")],
            vec![shot(7, 2, "Transfers")],
        ),
        // ---- Internet ----
        app(
            "org.mozilla.firefox",
            "Firefox",
            "Mozilla",
            "Fast, private and independent browser",
            "Firefox is a modern, standards-first browser with a tracker-blocking \
             engine, container tabs and a sync service that keeps your tabs and \
             passwords across devices without handing them to an ad network.",
            Category::Internet,
            Source::Flatpak,
            "129.0",
            286.0,
            4.5,
            38210,
            true,
            6,
            "Fx",
            vec![
                Permission::sensitive("Network access"),
                Permission::normal("Read and write your Downloads folder"),
                Permission::normal("Webcam and microphone"),
            ],
            vec![shot(6, 2, "Browsing"), shot(8, 3, "Reader mode")],
        ),
        app(
            "com.discordapp.Discord",
            "Discord",
            "Discord Inc.",
            "Voice, video and text chat for communities",
            "Discord is a chat platform built around persistent servers and \
             channels, with low-latency voice, screen sharing and a rich \
             bot ecosystem. Popular with gaming, study and open-source \
             communities alike.",
            Category::Internet,
            Source::Flatpak,
            "0.0.66",
            172.0,
            4.1,
            18740,
            false,
            9,
            "Ds",
            vec![
                Permission::sensitive("Network access"),
                Permission::normal("Webcam and microphone"),
                Permission::normal("Audio playback"),
            ],
            vec![shot(9, 2, "Channels"), shot(5, 3, "Voice")],
        ),
        app(
            "org.signal.Signal",
            "Signal",
            "Signal Foundation",
            "End-to-end encrypted private messenger",
            "Signal is a private messenger with end-to-end encryption for \
             messages, calls and group chats. No ads, no trackers, and an open \
             protocol audited by cryptographers worldwide.",
            Category::Internet,
            Source::Flatpak,
            "7.20.0",
            148.0,
            4.6,
            11200,
            false,
            5,
            "Sg",
            vec![
                Permission::sensitive("Network access"),
                Permission::normal("Webcam and microphone"),
                Permission::normal("Notifications"),
            ],
            vec![shot(5, 2, "Conversations")],
        ),
        // ---- Games ----
        app(
            "com.valvesoftware.Steam",
            "Steam",
            "Valve",
            "The biggest PC games library",
            "Steam is Valve's storefront and launcher for thousands of PC games, \
             with cloud saves, a friends network, Proton-powered Windows-game \
             compatibility and a built-in overlay. The Linux client pairs well \
             with a controller and a couch.",
            Category::Games,
            Source::Flatpak,
            "1.0.0.81",
            1480.0,
            4.4,
            52310,
            true,
            8,
            "St",
            vec![
                Permission::sensitive("Network access"),
                Permission::sensitive("Full home filesystem access"),
                Permission::normal("Game controllers"),
                Permission::normal("GPU acceleration"),
            ],
            vec![shot(8, 2, "Library"), shot(6, 1, "Store"), shot(5, 3, "Big Picture")],
        ),
        app(
            "net.pcsx2.PCSX2",
            "PCSX2",
            "PCSX2 Team",
            "PlayStation 2 emulator",
            "PCSX2 emulates the PlayStation 2 with upscaling, texture filtering \
             and save states. A mature graphics backend renders classic titles \
             at modern resolutions while staying faithful to the original \
             hardware quirks.",
            Category::Games,
            Source::Flatpak,
            "2.0.2",
            58.0,
            4.3,
            4410,
            false,
            7,
            "Px",
            vec![
                Permission::normal("Game controllers"),
                Permission::normal("GPU acceleration"),
                Permission::normal("Read and write your Games folder"),
            ],
            vec![shot(7, 2, "Upscaled output")],
        ),
        app(
            "org.zealdocs.Zeal",
            "Zeal",
            "Oleg Shparber",
            "Offline developer documentation browser",
            "Zeal is an offline documentation browser that downloads docsets for \
             hundreds of languages and frameworks, so API reference is one \
             keystroke away even without a network — handy on a locked-down \
             workstation.",
            Category::Development,
            Source::Flatpak,
            "0.7.1",
            44.0,
            4.2,
            1980,
            false,
            6,
            "Z",
            vec![Permission::normal("Read and write your Documents folder")],
            vec![shot(6, 2, "Docset browser")],
        ),
        // ---- qOS-native apps ----
        // These use the bespoke "Open directly" / "Add" model instead of a
        // permanent Flatpak install. The affordances render in the UI with
        // explanatory tooltips; the install backend is a documented stub.
        with_compat(
            app(
                "os.qos.Whiteboard",
                "qOS Whiteboard",
                "qOS Project",
                "Collaborative infinite canvas, runs without installing",
                "A qOS-native collaborative whiteboard built on the CRDT toolkit. \
                 Because it's a qOS app, you can open it directly from a cached, \
                 strongly-sandboxed bundle — like a web app — without a permanent \
                 install, or add it to keep it cached and prioritised in search \
                 and auto-updates.",
                Category::Productivity,
                Source::QosNative,
                "0.9.0",
                12.0,
                4.7,
                640,
                true,
                4,
                "Wb",
                vec![
                    Permission::normal("Collaborate within this cluster"),
                    Permission::normal("Read and write your Boards folder"),
                ],
                vec![shot(4, 2, "Infinite canvas"), shot(6, 3, "Live cursors")],
            ),
            vec![
                Compat::info("Runs from a cached bundle in a strong per-launch sandbox"),
                Compat::info("No permanent install — 'Open directly' or 'Add' to keep it"),
            ],
        ),
        with_compat(
            app(
                "os.qos.Snap",
                "qOS Snap",
                "qOS Project",
                "Lightweight screenshot + annotate, web-app style",
                "A qOS-native capture-and-annotate tool. Open it directly when you \
                 just need a quick screenshot, or add it so it stays cached and \
                 ready in global search.",
                Category::Graphics,
                Source::QosNative,
                "0.4.1",
                6.0,
                4.5,
                210,
                false,
                8,
                "Sn",
                vec![Permission::normal("Capture the screen within this cluster")],
                vec![shot(8, 2, "Annotate")],
            ),
            vec![Compat::info("qOS app — cached, sandboxed, no permanent install")],
        ),
    ]
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::Category;

    #[test]
    fn catalog_spans_every_category() {
        let apps = sample_catalog();
        for c in Category::ALL {
            assert!(
                apps.iter().any(|a| a.category == c),
                "no app in category {:?}",
                c
            );
        }
    }

    #[test]
    fn catalog_has_a_dozen_plus_apps() {
        assert!(sample_catalog().len() >= 12);
    }

    #[test]
    fn catalog_has_featured_apps() {
        assert!(sample_catalog().iter().filter(|a| a.featured).count() >= 3);
    }

    #[test]
    fn ids_are_unique() {
        let apps = sample_catalog();
        let mut ids: Vec<_> = apps.iter().map(|a| a.id.clone()).collect();
        ids.sort();
        ids.dedup();
        assert_eq!(ids.len(), apps.len(), "duplicate app id in catalog");
    }
}
