#[cfg(feature = "appimage")]
use super::squashfs::tests::Writer;
use super::*;

/// A tiny PNG: the signature is all `Icon::sniff` and the entry need.
const PNG: &[u8] = b"\x89PNG\r\n\x1a\n\0\0\0\rIHDR fixture";
const SVG: &[u8] = b"<?xml version=\"1.0\"?><svg xmlns=\"http://www.w3.org/2000/svg\"/>";

/// A type 2 `AppImage`: a 64-byte ELF64 header marked `AI\x02` whose
/// section headers end where it ends, then the squashfs image. That is the
/// layout the runtime has; nothing here runs.
pub(crate) fn appimage(image: &[u8]) -> Vec<u8> {
    let mut elf = vec![0u8; 64];
    elf[..4].copy_from_slice(b"\x7fELF");
    elf[4] = 2; // 64-bit
    elf[5] = 1; // little-endian
    elf[6] = 1;
    elf[8..11].copy_from_slice(b"AI\x02");
    elf[0x28..0x30].copy_from_slice(&64u64.to_le_bytes()); // e_shoff
    elf[0x3A..0x3C].copy_from_slice(&64u16.to_le_bytes()); // e_shentsize
    elf[0x3C..0x3E].copy_from_slice(&0u16.to_le_bytes()); // e_shnum
    elf.extend_from_slice(image);
    elf
}

#[cfg(feature = "appimage")]
pub(crate) fn krita_image(version: &str) -> Vec<u8> {
    appimage(
        &Writer::new()
            .file(
                "org.kde.krita.desktop",
                format!(
                    "[Desktop Entry]\nType=Application\nName=Krita\nComment=Digital painting\n\
                     Exec=krita %F\nIcon=krita\nCategories=Graphics;\nX-AppImage-Version={version}\n"
                )
                .as_bytes(),
            )
            .file("usr/share/icons/hicolor/scalable/apps/krita.svg", SVG)
            .symlink(".DirIcon", "usr/share/icons/hicolor/scalable/apps/krita.svg")
            .file("AppRun", b"#!/bin/sh\n")
            .build(),
    )
}

fn executable(path: &Path, bytes: &[u8]) {
    use std::os::unix::fs::PermissionsExt as _;
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(path, bytes).unwrap();
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o755)).unwrap();
}

fn store(root: &Path) -> (Store, PathBuf) {
    let applications = root.join("data/applications");
    (
        Store::at(&applications, &root.join("data/tilo/downloaded-apps")),
        applications,
    )
}

fn scanned(applications: &Path) -> Vec<crate::DesktopEntry> {
    crate::scan::scan_dirs(
        &[applications.to_path_buf()],
        &crate::scan::current_desktops(),
    )
    .entries
}

#[test]
#[cfg(feature = "appimage")]
fn an_appimage_names_itself_from_its_embedded_entry_without_running() {
    let tmp = tempfile::tempdir().unwrap();
    let file = tmp.path().join("home/Downloads/Krita-5.2-x86_64.AppImage");
    executable(&file, &krita_image("5.2"));
    assert!(appimage::is_appimage(&file));

    let program = classify(
        &ProcessFacts {
            exe: PathBuf::from("/tmp/.mount_KritaXYZ/usr/bin/krita"),
            appimage: Some(file.clone()),
            parent_script: None,
        },
        &tmp.path().join("home"),
    )
    .expect("an AppImage's runtime names its image");
    assert_eq!(program.kind, ProgramKind::AppImage);
    assert_eq!(program.path, file);

    let identity = identify(
        &program,
        &WindowFacts {
            app_id: "krita".into(),
            icon: None,
        },
    );
    assert_eq!(identity.key, "org.kde.krita");
    assert_eq!(identity.name, "Krita");
    assert_eq!(identity.comment.as_deref(), Some("Digital painting"));
    assert_eq!(identity.field_code.as_deref(), Some("%F"));
    let icon = identity.icon.as_ref().expect("the embedded icon");
    assert_eq!(icon.format, IconFormat::Svg);
    assert_eq!(icon.bytes, SVG);

    let (store, applications) = store(tmp.path());
    let id = store.add(&identity, &program).unwrap();
    assert_eq!(id, "tilo-downloaded-org.kde.krita");
    let entries = scanned(&applications);
    let entry = entries
        .iter()
        .find(|e| e.id == id)
        .expect("in the applications model");
    assert_eq!(entry.name, "Krita");
    let argv = crate::exec::split_exec(entry.exec.as_deref().unwrap()).unwrap();
    assert_eq!(argv, [file.to_string_lossy().as_ref(), "%F"]);
    assert!(
        entry
            .icon
            .as_deref()
            .unwrap()
            .ends_with("org.kde.krita.svg")
    );
    assert_eq!(entry.startup_wm_class.as_deref(), Some("krita"));
    let downloaded = entry.downloaded.as_ref().unwrap();
    assert!(downloaded.present);
    assert_eq!(downloaded.kind, ProgramKind::AppImage);
    assert_eq!(downloaded.file, file_identity(&file));
}

#[test]
fn a_tarball_program_takes_its_name_and_icon_from_its_folder() {
    let tmp = tempfile::tempdir().unwrap();
    let home = tmp.path().join("home");
    let folder = home.join("Downloads/Thing-1.0");
    executable(&folder.join("bin/thing"), b"\x7fELF not an appimage");
    std::fs::write(
        folder.join("thing.desktop"),
        "[Desktop Entry]\nType=Application\nName=Thing Editor\nExec=bin/thing %U\nIcon=thing\n",
    )
    .unwrap();
    std::fs::create_dir_all(folder.join("share/icons/hicolor/256x256/apps")).unwrap();
    std::fs::write(
        folder.join("share/icons/hicolor/256x256/apps/thing.png"),
        PNG,
    )
    .unwrap();

    let program = classify(
        &ProcessFacts {
            exe: folder.join("bin/thing"),
            appimage: None,
            parent_script: None,
        },
        &home,
    )
    .expect("a program in home is a download");
    assert_eq!(program.kind, ProgramKind::Program);
    let identity = identify(
        &program,
        &WindowFacts {
            app_id: "thing-editor".into(),
            icon: None,
        },
    );
    assert_eq!(identity.key, "thing");
    assert_eq!(identity.name, "Thing Editor");
    assert_eq!(identity.icon.as_ref().unwrap().format, IconFormat::Png);
    assert_eq!(identity.startup_wm_class.as_deref(), Some("thing-editor"));

    let (store, applications) = store(tmp.path());
    let id = store.add(&identity, &program).unwrap();
    let entries = scanned(&applications);
    let entry = entries.iter().find(|e| e.id == id).unwrap();
    let argv = crate::exec::split_exec(entry.exec.as_deref().unwrap()).unwrap();
    assert_eq!(
        argv[0],
        folder.join("bin/thing").to_string_lossy(),
        "the entry starts the program inside its folder, which stays where it is"
    );
}

#[test]
fn programs_that_are_not_downloads_are_left_alone() {
    let tmp = tempfile::tempdir().unwrap();
    let home = tmp.path().join("home");
    let facts = |exe: PathBuf| ProcessFacts {
        exe,
        appimage: None,
        parent_script: None,
    };
    executable(&home.join(".cargo/bin/tool"), b"x");
    assert!(classify(&facts(home.join(".cargo/bin/tool")), &home).is_none());
    executable(&home.join("src/project/target/debug/app"), b"x");
    std::fs::create_dir_all(home.join("src/project/.git")).unwrap();
    assert!(
        classify(&facts(home.join("src/project/target/debug/app")), &home).is_none(),
        "a program built in a checkout is not a download"
    );
    assert!(classify(&facts(PathBuf::from("/usr/bin/true")), &home).is_none());
    // An APPIMAGE variable naming something that is not an AppImage proves
    // nothing.
    let fake = home.join("Downloads/not-an-appimage");
    executable(&fake, b"\x7fELF");
    let program = classify(
        &ProcessFacts {
            exe: PathBuf::from("/usr/bin/true"),
            appimage: Some(fake),
            parent_script: None,
        },
        &home,
    );
    assert!(program.is_none());
}

#[test]
#[cfg(feature = "appimage")]
fn a_new_version_takes_over_the_entry_instead_of_adding_a_second() {
    let tmp = tempfile::tempdir().unwrap();
    let home = tmp.path().join("home");
    let old = home.join("Downloads/Krita-5.1-x86_64.AppImage");
    let new = home.join("Downloads/Krita-5.2-x86_64.AppImage");
    executable(&old, &krita_image("5.1"));
    executable(&new, &krita_image("5.2"));
    let window = WindowFacts {
        app_id: "krita".into(),
        icon: None,
    };
    let (store, applications) = store(tmp.path());
    let first = Program {
        path: old,
        kind: ProgramKind::AppImage,
    };
    store.add(&identify(&first, &window), &first).unwrap();
    assert_eq!(
        store
            .update_location(&identify(&first, &window), &first)
            .unwrap(),
        None,
        "running the same file again changes nothing"
    );

    let second = Program {
        path: new.clone(),
        kind: ProgramKind::AppImage,
    };
    let updated = store
        .update_location(&identify(&second, &window), &second)
        .unwrap();
    assert_eq!(updated.as_deref(), Some("tilo-downloaded-org.kde.krita"));
    let entries = scanned(&applications);
    let krita: Vec<_> = entries.iter().filter(|e| e.name == "Krita").collect();
    assert_eq!(krita.len(), 1, "one app, one entry: {krita:?}");
    assert_eq!(krita[0].downloaded.as_ref().unwrap().program, new);

    // An app with no entry is never added by running it: that is the
    // person's choice.
    let other = Program {
        path: home.join("Downloads/other"),
        kind: ProgramKind::Program,
    };
    executable(&other.path, b"x");
    let other_identity = identify(
        &other,
        &WindowFacts {
            app_id: "org.example.Other".into(),
            icon: None,
        },
    );
    assert_eq!(
        store.update_location(&other_identity, &other).unwrap(),
        None
    );
    assert!(store.lookup("org.example.Other").is_none());
}

#[test]
fn a_missing_file_is_noticed_and_located_and_remove_keeps_the_file() {
    let tmp = tempfile::tempdir().unwrap();
    let home = tmp.path().join("home");
    let file = home.join("Downloads/tool");
    executable(&file, b"\x7fELF");
    let program = Program {
        path: file.clone(),
        kind: ProgramKind::Program,
    };
    let identity = identify(
        &program,
        &WindowFacts {
            app_id: "org.example.Tool".into(),
            icon: Some(Icon::sniff(PNG.to_vec()).unwrap()),
        },
    );
    assert_eq!(identity.name, "Tool");
    let (store, applications) = store(tmp.path());
    let id = store.add(&identity, &program).unwrap();
    let icon = PathBuf::from(
        scanned(&applications)
            .into_iter()
            .find(|e| e.id == id)
            .unwrap()
            .icon
            .unwrap(),
    );
    assert_eq!(
        std::fs::read(&icon).unwrap(),
        PNG,
        "the window's icon is kept"
    );

    let moved = home.join("Apps/tool");
    std::fs::create_dir_all(moved.parent().unwrap()).unwrap();
    std::fs::rename(&file, &moved).unwrap();
    let entry = scanned(&applications)
        .into_iter()
        .find(|e| e.id == id)
        .unwrap();
    assert!(
        !entry.downloaded.as_ref().unwrap().present,
        "the entry says its file is gone"
    );
    let refused = crate::launch::plan(&entry, &crate::launch::LaunchEnv::default());
    assert!(
        matches!(
            refused,
            Err(crate::launch::HostLaunchError::ProgramMissing { .. })
        ),
        "a missing file is never a dead launch: {refused:?}"
    );

    assert!(store.locate(&id, &home.join("Apps/nothing-here")).is_err());
    store.locate(&id, &moved).unwrap();
    let entry = scanned(&applications)
        .into_iter()
        .find(|e| e.id == id)
        .unwrap();
    let downloaded = entry.downloaded.as_ref().unwrap();
    assert!(downloaded.present);
    assert_eq!(downloaded.program, moved);
    assert_eq!(entry.icon.as_deref(), Some(icon.to_string_lossy().as_ref()));

    store.remove(&id).unwrap();
    assert!(scanned(&applications).iter().all(|e| e.id != id));
    assert!(moved.exists(), "removing the entry never touches the file");
    assert!(!icon.exists(), "the icon Tilo kept for it goes with it");
    assert!(
        store.remove("org.gnome.TextEditor").is_err(),
        "only its own entries"
    );
}

#[test]
fn awkward_paths_survive_the_exec_line() {
    let tmp = tempfile::tempdir().unwrap();
    let file = tmp.path().join("home/My Apps/100% \"real\" $tool");
    executable(&file, b"x");
    let program = Program {
        path: file.clone(),
        kind: ProgramKind::Program,
    };
    let identity = identify(
        &program,
        &WindowFacts {
            app_id: "../../evil id".into(),
            icon: None,
        },
    );
    let (store, applications) = store(tmp.path());
    let id = store.add(&identity, &program).unwrap();
    assert!(!id.contains('/'));
    let entry = scanned(&applications)
        .into_iter()
        .find(|e| e.id == id)
        .unwrap();
    let plan = crate::launch::plan(&entry, &crate::launch::LaunchEnv::default()).unwrap();
    assert_eq!(plan.program, file);
    assert!(plan.args.is_empty());
}

#[test]
fn a_declined_file_is_not_asked_about_again_but_a_new_version_is() {
    let tmp = tempfile::tempdir().unwrap();
    let (store, _) = store(tmp.path());
    let first = tmp.path().join("home/Downloads/tool-1.0");
    let second = tmp.path().join("home/Downloads/tool-1.1");
    executable(&first, b"\x7fELF one");
    executable(&second, b"\x7fELF two");
    assert!(!store.declined(&first));
    store.decline(&first).unwrap();
    assert!(store.declined(&first));
    assert!(!store.declined(&second), "another file is another question");
    // The same file renamed is still the file that was declined.
    let renamed = tmp.path().join("home/Downloads/tool");
    std::fs::rename(&first, &renamed).unwrap();
    assert!(store.declined(&renamed));
    // A file that is gone is forgotten the next time the list is written.
    std::fs::remove_file(&renamed).unwrap();
    store.decline(&second).unwrap();
    assert!(store.declined(&second));
    let text =
        std::fs::read_to_string(tmp.path().join("data/tilo/downloaded-apps/declined")).unwrap();
    assert!(
        !text.contains("tool-1.0") && !text.contains("Downloads/tool\""),
        "{text}"
    );
}

#[test]
fn moving_into_applications_keeps_the_name_and_never_overwrites() {
    let tmp = tempfile::tempdir().unwrap();
    let applications = tmp.path().join("home/Applications");
    let first = tmp.path().join("home/Downloads/Krita.AppImage");
    executable(&first, b"one");
    let moved = move_into(&first, &applications).unwrap();
    assert_eq!(moved, applications.join("Krita.AppImage"));
    assert!(!first.exists());
    assert_eq!(std::fs::read(&moved).unwrap(), b"one");
    {
        use std::os::unix::fs::PermissionsExt as _;
        let mode = std::fs::metadata(&moved).unwrap().permissions().mode();
        assert_eq!(mode & 0o111, 0o111, "it stays runnable");
    }
    let again = tmp.path().join("home/Downloads/Krita.AppImage");
    executable(&again, b"two");
    let second = move_into(&again, &applications).unwrap();
    assert_eq!(second, applications.join("Krita (2).AppImage"));
    assert_eq!(
        std::fs::read(&moved).unwrap(),
        b"one",
        "nothing is overwritten"
    );
    assert_eq!(move_into(&second, &applications).unwrap(), second);
}

#[test]
fn a_bundled_runtime_is_never_the_program_its_launcher_script_is() {
    let tmp = tempfile::tempdir().unwrap();
    let home = tmp.path().join("home");
    let install = home.join("Downloads/idea-2026.2");
    let java = install.join("jbr/bin/java");
    let script = install.join("bin/idea.sh");
    executable(&java, b"\x7fELF java");
    executable(&script, b"#!/bin/sh\nexec ../jbr/bin/java");
    // Started by its script: the script is the program.
    let from_script = classify(
        &ProcessFacts {
            exe: java.clone(),
            appimage: None,
            parent_script: Some(script.clone()),
        },
        &home,
    )
    .unwrap();
    assert_eq!(from_script.path, script);
    // The script exec'd it away: the folder's desktop entry names it.
    assert_eq!(
        classify(
            &ProcessFacts {
                exe: java.clone(),
                appimage: None,
                parent_script: None,
            },
            &home,
        ),
        None,
        "with neither a script nor an entry, a runtime is never offered"
    );
    std::fs::write(
        install.join("idea.desktop"),
        "[Desktop Entry]\nType=Application\nName=IntelliJ IDEA\nExec=bin/idea.sh %f\nIcon=idea\n",
    )
    .unwrap();
    let from_entry = classify(
        &ProcessFacts {
            exe: java,
            appimage: None,
            parent_script: None,
        },
        &home,
    )
    .unwrap();
    assert_eq!(from_entry.path, script);
    let identity = identify(
        &from_entry,
        &WindowFacts {
            app_id: "jetbrains-idea".into(),
            icon: None,
        },
    );
    assert_eq!(identity.name, "IntelliJ IDEA");
}

#[test]
fn a_record_identity_names_the_record_it_was_written_with() {
    let id = [
        0x12, 0x34, 0x56, 0x78, 0x9a, 0xbc, 0xde, 0xf0, 0x01, 0x23, 0x45, 0x67, 0x89, 0xab, 0xcd,
        0xef,
    ];
    let written = DownloadedProgram::from_keys(
        Some("/home/me/Foo.AppImage"),
        Some("AppImage"),
        Some(&format!("record:{}", super::format_record(&id))),
        None,
    )
    .unwrap();
    assert_eq!(written.record(), Some(id));
    assert_eq!(
        written.file.as_deref(),
        Some("record:12345678-9abc-def0-0123-456789abcdef")
    );
    let inode = DownloadedProgram::from_keys(
        Some("/home/me/Foo.AppImage"),
        Some("AppImage"),
        Some("64769:1234"),
        None,
    )
    .unwrap();
    assert_eq!(inode.record(), None);
}

/// A real, decodable `edge`-pixel PNG: a filled square in `rgb`.
#[cfg(feature = "appimage")]
fn real_png(edge: u32, rgb: [u8; 3]) -> Vec<u8> {
    use std::io::Write as _;
    fn crc32(bytes: &[u8]) -> u32 {
        let mut crc = !0_u32;
        for byte in bytes {
            crc ^= u32::from(*byte);
            for _ in 0..8 {
                crc = if crc & 1 == 1 {
                    (crc >> 1) ^ 0xEDB8_8320
                } else {
                    crc >> 1
                };
            }
        }
        !crc
    }
    fn chunk(out: &mut Vec<u8>, kind: [u8; 4], data: &[u8]) {
        out.extend_from_slice(&u32::try_from(data.len()).unwrap().to_be_bytes());
        let mut body = kind.to_vec();
        body.extend_from_slice(data);
        out.extend_from_slice(&body);
        out.extend_from_slice(&crc32(&body).to_be_bytes());
    }
    let mut header = Vec::new();
    header.extend_from_slice(&edge.to_be_bytes());
    header.extend_from_slice(&edge.to_be_bytes());
    header.extend_from_slice(&[8, 2, 0, 0, 0]); // 8-bit RGB
    // Each row: filter type 0, then its pixels.
    let mut row = vec![0_u8];
    for _ in 0..edge {
        row.extend_from_slice(&rgb);
    }
    let rows = row.repeat(usize::try_from(edge).unwrap());
    let mut zlib = flate2::write::ZlibEncoder::new(Vec::new(), flate2::Compression::default());
    zlib.write_all(&rows).unwrap();
    let mut out = b"\x89PNG\r\n\x1a\n".to_vec();
    chunk(&mut out, *b"IHDR", &header);
    chunk(&mut out, *b"IDAT", &zlib.finish().unwrap());
    chunk(&mut out, *b"IEND", &[]);
    out
}

#[test]
#[cfg(feature = "appimage")]
fn an_appimage_without_the_named_icon_shows_its_dir_icon_and_keeps_it_with_its_entry() {
    let tmp = tempfile::tempdir().unwrap();
    let dir_icon = real_png(64, [0x2a, 0x3f, 0xd1]);
    let file = tmp.path().join("home/Downloads/Tool-x86_64.AppImage");
    executable(
        &file,
        &appimage(
            &Writer::new()
                .file(
                    "org.example.Tool.desktop",
                    b"[Desktop Entry]\nType=Application\nName=Tool\nExec=tool\nIcon=tool\n",
                )
                .file(".DirIcon", &dir_icon)
                .file("AppRun", b"#!/bin/sh\n")
                .build(),
        ),
    );
    let program = Program {
        path: file,
        kind: ProgramKind::AppImage,
    };
    let identity = identify(
        &program,
        &WindowFacts {
            app_id: "tool".into(),
            icon: None,
        },
    );
    let icon = identity
        .icon
        .as_ref()
        .expect(".DirIcon is the image's icon");
    assert_eq!(icon.format, IconFormat::Png);
    assert_eq!(icon.bytes, dir_icon);

    // The entry keeps its own copy, so the list, search and the question
    // all show the same picture, and it outlives the image being moved.
    let (store, applications) = store(tmp.path());
    let id = store.add(&identity, &program).unwrap();
    let entry = scanned(&applications)
        .into_iter()
        .find(|entry| entry.id == id)
        .unwrap();
    let kept = PathBuf::from(entry.icon.expect("the entry names its icon"));
    assert!(kept.starts_with(tmp.path().join("data/tilo/downloaded-apps/icons")));
    assert_eq!(std::fs::read(kept).unwrap(), dir_icon);
}

#[test]
fn a_program_with_no_artwork_shows_the_generic_application_icon() {
    let tmp = tempfile::tempdir().unwrap();
    let file = tmp.path().join("home/Downloads/mytool");
    executable(&file, b"\x7fELF");
    let program = Program {
        path: file,
        kind: ProgramKind::Program,
    };
    let identity = identify(
        &program,
        &WindowFacts {
            app_id: "org.example.MyTool".into(),
            icon: None,
        },
    );
    assert_eq!(identity.icon, None);
    let (store, applications) = store(tmp.path());
    let id = store.add(&identity, &program).unwrap();
    let entry = scanned(&applications)
        .into_iter()
        .find(|entry| entry.id == id)
        .unwrap();
    assert_eq!(entry.icon.as_deref(), Some(GENERIC_ICON));
}

fn named(key: &str, name: &str, class: Option<&str>) -> Identity {
    Identity {
        key: key.into(),
        name: name.into(),
        icon: None,
        comment: None,
        categories: Vec::new(),
        keywords: Vec::new(),
        mime_types: Vec::new(),
        startup_wm_class: class.map(str::to_owned),
        field_code: None,
    }
}

#[test]
fn the_newest_copy_is_started_and_the_others_stay_known_until_they_are_gone() {
    let tmp = tempfile::tempdir().unwrap();
    let appimage = tmp.path().join("home/Downloads/Tool-x86_64.AppImage");
    let tarball = tmp.path().join("home/Downloads/tool-linux/tool");
    executable(&appimage, b"\x7fELF");
    executable(&tarball, b"\x7fELF");
    let (store, applications) = store(tmp.path());
    let identity = named("org.example.Tool", "Tool", None);
    let id = store
        .add(
            &identity,
            &Program {
                path: appimage.clone(),
                kind: ProgramKind::AppImage,
            },
        )
        .unwrap();
    let same = store
        .add(
            &identity,
            &Program {
                path: tarball.clone(),
                kind: ProgramKind::Program,
            },
        )
        .unwrap();
    assert_eq!(same, id, "one app, one entry");
    let downloaded = |applications: &Path| {
        scanned(applications)
            .into_iter()
            .find(|entry| entry.id == id)
            .and_then(|entry| entry.downloaded)
            .unwrap()
    };
    let now = downloaded(&applications);
    assert_eq!(
        now.program, tarball,
        "the copy added last is the one started"
    );
    assert_eq!(now.locations, std::slice::from_ref(&appimage));
    assert_eq!(now.copies(), [tarball.clone(), appimage.clone()]);

    // Phase `a_gone_copy_falls_back_to_one_still_there`.
    std::fs::remove_file(&tarball).unwrap();
    assert_eq!(store.fall_back(&id).unwrap(), Some(appimage.clone()));
    let now = downloaded(&applications);
    assert_eq!(now.program, appimage);
    assert!(now.locations.is_empty(), "a gone copy is forgotten");
    assert_eq!(
        store.fall_back(&id).unwrap(),
        None,
        "nothing to do while the file is there"
    );
}

#[test]
fn a_copy_only_its_window_names_joins_the_entry_another_copy_made() {
    let tmp = tempfile::tempdir().unwrap();
    let home = tmp.path().join("home");
    let appimage = home.join("Downloads/Tool-x86_64.AppImage");
    let bare = home.join("Downloads/tool");
    executable(&appimage, b"\x7fELF");
    executable(&bare, b"\x7fELF");
    let (store, applications) = store(tmp.path());
    let id = store
        .add(
            &named("org.example.Tool", "Tool", Some("tool-window")),
            &Program {
                path: appimage.clone(),
                kind: ProgramKind::AppImage,
            },
        )
        .unwrap();
    let program = Program {
        path: bare.clone(),
        kind: ProgramKind::Program,
    };
    // By the entry's StartupWMClass ...
    let by_class = store.identify(
        &program,
        &WindowFacts {
            app_id: "tool-window".into(),
            icon: None,
        },
    );
    assert_eq!(by_class.key, "org.example.Tool");
    assert_eq!(by_class.name, "Tool");
    // ... or by the desktop file name it was keyed by.
    let by_stem = store.identify(
        &program,
        &WindowFacts {
            app_id: "org.example.Tool".into(),
            icon: None,
        },
    );
    assert_eq!(by_stem.key, "org.example.Tool");
    assert_eq!(
        store.update_location(&by_class, &program).unwrap(),
        Some(id)
    );
    let entries = scanned(&applications);
    let tools: Vec<_> = entries
        .iter()
        .filter(|entry| entry.name == "Tool")
        .collect();
    assert_eq!(tools.len(), 1, "one app, one entry: {tools:?}");
    let downloaded = tools[0].downloaded.as_ref().unwrap();
    assert_eq!(downloaded.program, bare);
    assert_eq!(downloaded.locations, [appimage]);
    // A window no entry knows is its own app.
    let other = store.identify(
        &program,
        &WindowFacts {
            app_id: "org.example.Other".into(),
            icon: None,
        },
    );
    assert_eq!(other.key, "org.example.Other");
}
