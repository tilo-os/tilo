//! The native session: `tilo-session [--backend drm|headless] [--size WxH]
//! [-- command args...]`. Listens on a fresh `wayland-N` socket, prints its
//! name and starts `command` with `WAYLAND_DISPLAY` set to it; with a
//! command, the session lasts as long as it does.

#[cfg(target_os = "linux")]
fn main() -> std::process::ExitCode {
    use std::process::ExitCode;
    use tilo_compositor::{DisplayConfig, OutputConfig};
    use tilo_session::{Options, Session};

    let args: Vec<String> = std::env::args().skip(1).collect();
    let (ours, command) = match args.iter().position(|arg| arg == "--") {
        Some(split) => (
            args.get(..split).unwrap_or_default(),
            args.get(split.saturating_add(1)..).unwrap_or_default(),
        ),
        None => (args.as_slice(), &[][..]),
    };
    let value = |flag: &str| {
        ours.iter()
            .position(|arg| arg == flag)
            .and_then(|index| ours.get(index.saturating_add(1)))
    };
    let backend = value("--backend").map_or("drm", String::as_str);
    let size = value("--size")
        .and_then(|size| size.split_once('x'))
        .and_then(|(w, h)| Some((w.parse().ok()?, h.parse().ok()?)))
        .unwrap_or((1280, 720));

    let options = Options {
        search: search_options(),
        ..Options::default()
    };
    let started = match backend {
        "headless" => {
            Session::headless(options, &[OutputConfig::new("HEADLESS-1", size.0, size.1)])
        }
        "drm" => Session::drm(options, DisplayConfig::default()),
        other => {
            eprintln!("tilo-session: unknown backend {other} (drm, headless)");
            return ExitCode::FAILURE;
        }
    };
    let mut session = match started {
        Ok(session) => session,
        Err(error) => {
            eprintln!("tilo-session: {error}");
            return ExitCode::FAILURE;
        }
    };
    let socket = match session.compositor().listen() {
        Ok(socket) => socket,
        Err(error) => {
            eprintln!("tilo-session: {error}");
            return ExitCode::FAILURE;
        }
    };
    println!("WAYLAND_DISPLAY={socket}");
    let mut child = None;
    if let Some((program, rest)) = command.split_first() {
        match std::process::Command::new(program)
            .args(rest)
            .env("WAYLAND_DISPLAY", &socket)
            .spawn()
        {
            Ok(spawned) => child = Some(spawned),
            Err(error) => eprintln!("tilo-session: {program}: {error}"),
        }
    }
    let result = session.run(|_| {
        child
            .as_mut()
            .is_some_and(|child| child.try_wait().is_ok_and(|status| status.is_some()))
    });
    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("tilo-session: {error}");
            ExitCode::FAILURE
        }
    }
}

/// What search reads of this device: the Tilo apps installed here and the
/// machine's own, the apps the person can get, what apps hold, the
/// person's files, and the search sources they switched off.
#[cfg(target_os = "linux")]
fn search_options() -> tilo_session::SearchOptions {
    use std::sync::Arc;
    use tilo_desktop_search::providers::AppEntry;
    use tilo_desktop_search::providers::files::FileNames;
    use tilo_desktop_search::providers::host_apps::HostAppEntry;

    let installed = tilo_installed_apps::read_from_env().unwrap_or_else(|error| {
        eprintln!("tilo-session: installed apps: {error}");
        Vec::new()
    });
    let applications = installed
        .iter()
        .filter(|app| app.has_gui)
        .map(|app| AppEntry {
            id: app.package_id.clone(),
            title: app.display_name.clone(),
            category: app.category.clone(),
            description: None,
        })
        .collect();
    let scan = desktop_entry::scan::scan();
    let host_apps = scan
        .entries
        .iter()
        .filter(|entry| !entry.no_display && !entry.hidden)
        .map(|entry| HostAppEntry {
            desktop_id: entry.id.clone(),
            name: entry.name.clone(),
            generic_name: entry.generic_name.clone(),
            comment: entry.comment.clone(),
            keywords: entry.keywords.clone(),
        })
        .collect();
    let present = app_catalogue::Installed::from_ids(
        installed
            .iter()
            .map(|app| app.package_id.clone())
            .chain(scan.entries.iter().map(|entry| entry.id.clone())),
    );
    let catalogue = app_catalogue::Catalogue::from_sources(
        Vec::new(),
        app_catalogue::appstream::load_cached("en"),
    );
    let blocks = app_catalogue::BlockList::load();
    let catalogue = catalogue.offered(&present, &blocks).cloned().collect();
    let files = tilo_paths::home().map(|home| {
        let content = tilo_desktop_search::files_index::FilesIndex::open_or_create(&home)
            .ok()
            .map(Arc::new);
        (home.clone(), FileNames::scan(home), content)
    });
    tilo_session::SearchOptions {
        applications,
        host_apps,
        catalogue,
        content: Some(tilo_desktop_search::content_index::session()),
        files,
        preferences: tilo_desktop_search::SearchPreferences::from_values(
            &settings_store::load_section(tilo_desktop_search::preferences::SECTION),
        ),
        places: tilo_desktop_search::providers::places::RecentPlaces::default(),
    }
}

#[cfg(not(target_os = "linux"))]
fn main() {}
