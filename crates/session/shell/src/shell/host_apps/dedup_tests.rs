//! Deduplication by display name, against the per-entry scan it replaced.
//!
//! Every fixture runs through both production instantiations and the literal
//! former scan, which must keep the same apps in the same order: the index may
//! only change how much work is done, never which app a window belongs to. The
//! candidate counts pin that work down.

use super::*;

fn app(
    kind: desktop_entry::EntryKind,
    desktop_id: &str,
    name: &str,
    identities: &[&str],
) -> HostApp {
    HostApp {
        desktop_id: desktop_id.to_string(),
        name: name.to_string(),
        kind,
        categories: Vec::new(),
        keywords: Vec::new(),
        window_identities: identities
            .iter()
            .map(|identity| (*identity).to_string())
            .collect(),
        mime_types: Vec::new(),
        terminal: false,
        icon: None,
        downloaded: None,
    }
}

fn system(desktop_id: &str, name: &str, identities: &[&str]) -> HostApp {
    app(
        desktop_entry::EntryKind::System,
        desktop_id,
        name,
        identities,
    )
}

fn flatpak(desktop_id: &str, name: &str, identities: &[&str]) -> HostApp {
    app(
        desktop_entry::EntryKind::Flatpak,
        desktop_id,
        name,
        identities,
    )
}

/// The scan [`deduplicate`] used to run: every entry against every app already
/// kept. It is the oracle, and production code never calls it.
fn former_scan(apps: Vec<HostApp>) -> Vec<HostApp> {
    let mut out: Vec<HostApp> = Vec::new();
    for app in apps {
        let duplicate = out.iter().position(|existing| {
            existing.name.eq_ignore_ascii_case(&app.name)
                && existing.window_identities.iter().any(|left| {
                    app.window_identities
                        .iter()
                        .any(|right| left.eq_ignore_ascii_case(right))
                })
        });
        if let Some(index) = duplicate {
            if let Some(existing) = out.get_mut(index)
                && app.is_flatpak()
                && !existing.is_flatpak()
            {
                *existing = app;
            }
        } else {
            out.push(app);
        }
    }
    out
}

/// Run a fixture through both production instantiations and the oracle,
/// requiring the same apps in the same order, and return the kept apps with
/// the candidates examined by the counted pass.
fn agree(fixture: Vec<HostApp>) -> (Vec<HostApp>, usize) {
    let expected = former_scan(fixture.clone());
    let (counted, candidates) = deduplicate_pass::<true>(fixture.clone());
    assert_eq!(
        counted, expected,
        "the counted pass must keep exactly what the former scan kept, in its order"
    );
    let (uncounted, uncounted_candidates) = deduplicate_pass::<false>(fixture.clone());
    assert_eq!(
        uncounted, expected,
        "the uncounted pass must keep exactly what the former scan kept, in its order"
    );
    assert_eq!(
        uncounted_candidates, 0,
        "the uncounted pass must not count candidate comparisons"
    );
    assert_eq!(
        deduplicate(fixture),
        expected,
        "the entry point callers use must run that same pass"
    );
    (counted, candidates)
}

fn ids(apps: &[HostApp]) -> Vec<&str> {
    apps.iter().map(|app| app.desktop_id.as_str()).collect()
}

#[test]
fn aliases_collapse_as_the_former_scan_collapsed_them() {
    let fixture = vec![
        // The distro's file and the user's export of one app.
        system("vendor-chat", "Chat", &["com.example.Chat", "Chat"]),
        flatpak("org.example.Chat", "chat", &["org.example.Chat", "Chat"]),
        // The replaced slot now speaks only for the Flatpak's identities, so
        // the distro file's own identity is free again.
        system("chat-alias", "CHAT", &["com.example.Chat"]),
        // One display name, two launch modes.
        flatpak("org.example.Editor", "Editor", &["org.example.Editor"]),
        system("editor-safe", "Editor", &["EditorSafe"]),
        // An entry declaring no identity is nobody's alias.
        system("notes-stub", "Notes", &[]),
        flatpak("org.example.Notes", "Notes", &["org.example.Notes"]),
    ];
    let (kept, candidates) = agree(fixture);
    assert_eq!(
        ids(&kept),
        vec![
            "org.example.Chat",
            "chat-alias",
            "org.example.Editor",
            "editor-safe",
            "notes-stub",
            "org.example.Notes",
        ]
    );
    assert!(kept[0].is_flatpak(), "the Flatpak keeps its confinement");
    assert_eq!(
        kept[0].window_identities,
        vec!["org.example.Chat", "Chat"],
        "a replacement carries its own identities, not the ones it replaced"
    );
    assert_eq!(
        candidates, 3,
        "one kept Chat compared with the second and third chats, one Editor \
         with the second editor; the identity-less note compared nothing"
    );
}

#[test]
fn only_ascii_case_folds_two_names_together() {
    let fixture = vec![
        system("fold-lower", "strasse", &["com.example.one"]),
        system("fold-upper", "STRASSE", &["com.example.one"]),
        // 'ß' has no ASCII case: this is a different name, not a fold of one.
        system("sharp-s", "Straße", &["com.example.two"]),
        flatpak("sharp-s-upper", "STRASSE", &["com.example.two"]),
    ];
    let (kept, candidates) = agree(fixture);
    assert_eq!(
        ids(&kept),
        vec!["fold-lower", "sharp-s", "sharp-s-upper"],
        "ASCII case alone folds; ß does not, and a shared name alone is no identity"
    );
    assert_eq!(
        candidates, 2,
        "each STRASSE entry saw the one kept before it"
    );
}

#[test]
fn the_first_matching_slot_wins() {
    let fixture = vec![
        system("mix-x", "Mix", &["x"]),
        system("mix-y", "Mix", &["y"]),
        // Shares an identity with both slots; the first one wins.
        flatpak("mix-xy", "Mix", &["y", "x"]),
        system("mix-x-again", "Mix", &["x"]),
    ];
    let (kept, candidates) = agree(fixture);
    assert_eq!(
        ids(&kept),
        vec!["mix-xy", "mix-y"],
        "the Flatpak took the first slot, leaving the second alone"
    );
    assert!(kept[0].is_flatpak());
    assert_eq!(kept[0].window_identities, vec!["y", "x"]);
    assert_eq!(candidates, 3);
}

#[test]
fn distinct_display_names_compare_nothing() {
    let fixture: Vec<HostApp> = (0..512)
        .map(|i| {
            let identity = format!("org.example.App{i}");
            system(
                &format!("app-{i}"),
                &format!("App {i}"),
                &[identity.as_str()],
            )
        })
        .collect();
    let (kept, candidates) = agree(fixture);
    let expected: Vec<String> = (0..512).map(|i| format!("app-{i}")).collect();
    assert_eq!(ids(&kept), expected);
    assert_eq!(
        candidates, 0,
        "no entry's name was kept before it, so there was nothing to compare"
    );
}

#[test]
fn entries_without_identities_are_never_candidates() {
    let fixture: Vec<HostApp> = (0..512)
        .map(|i| system(&format!("notes-stub-{i}"), "Notes", &[]))
        .collect();
    let (kept, candidates) = agree(fixture);
    assert_eq!(kept.len(), 512, "every identity-less entry is its own app");
    assert_eq!(
        candidates, 0,
        "one name shared by 512 identity-less entries still compares nothing"
    );
}
