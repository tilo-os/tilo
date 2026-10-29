//! Every scene the driver can set up, by name.

use super::ShellDriver;

impl ShellDriver {
    /// Catalogue of scene names supported by `scene_setup`. Used by the
    /// legacy `--scenes` flag for default expansion + validation.
    pub const ALL_SCENES: &'static [&'static str] = &[
        "desktop",
        "room-member-window",
        "offline",
        "multi-group",
        "multi-strip",
        "maximized",
        "two-groups-focus",
        "overview",
        "overview-shared",
        "palette",
        "palette_typed",
        "palette_in_app_query",
        "app_search_files",
        "app_search_files_query",
        "app_search_code",
        "app_search_code_query",
        "app_search_notes",
        "app_search_notes_query",
        "palette_app_query",
        "palette_records",
        "palette_no_results",
        // Before `palette_ask_agent`, which chooses a model in this run's
        // settings.
        "palette_ask_agent_no_model",
        "palette_ask_agent",
        "palette_rechner",
        "palette_sum",
        "palette_move",
        "palette_keyboard_layout",
        "palette_files",
        "palette_finder",
        "palette_spotify",
        "palette_youtube_cats",
        "palette_github",
        "palette_workbook",
        "palette_downloaded",
        "palette_downloaded_missing",
        "launcher_downloaded",
        "palette_open_app",
        "palette_record_actions",
        "downloaded_add_appimage",
        "downloaded_add_program",
        "downloaded_add_no_artwork",
        "quick-settings",
        "qs-edit",
        "qs-edit-drag",
        "qs-hardware-wired",
        "qs-compact",
        "qs-muted",
        "qs-scrolled",
        "qs-wifi",
        "qs-wifi-two-displays",
        "qs-wifi-off",
        "qs-wifi-hardware-switch",
        "qs-wifi-turning-on",
        "qs-wifi-scanning",
        "qs-wifi-password",
        "qs-wifi-password-rejected",
        "qs-wifi-long-labels",
        "qs-bt",
        "qs-bt-long-labels",
        "qs-vpn-long-labels",
        "qs-battery",
        "qs-power-mode",
        "qs-sound",
        "qs-devices",
        "qs-clipboard",
        "qs-clipboard-paste",
        "power-menu",
        "ws-settings-general",
        "ws-settings-members",
        "ws-settings-permissions",
        "ws-settings-apps",
        "ws-settings-emoji",
        "ws-settings-emoji-empty",
        "ws-settings-agents",
        "ws-settings-agent-access",
        "ws-settings-agent-access-keyboard",
        "collaborators",
        "launchpad",
        "saved-window-missing-app",
        "saved-window-reopen",
        "launchpad-notes-card",
        "peer-sync",
        "app-uninstall-flatpak",
        "app-uninstall-tilo",
        "app-uninstall-downloaded",
        "app-uninstall-downloaded-copies",
        "app-uninstall-system",
        "app-uninstall-system-tilo",
        "app-uninstall-device-flatpak",
        "invite-empty",
        "invite-typing",
        "invite-error",
        "invite-pending",
        "join-empty",
        "overview-lounge",
        "join-field",
        "pairing-sheet",
        "device-asking",
        "lounge-open",
        "projection",
        "window-switcher",
        "emoji-picker",
        "display-confirm",
        "portal-permission",
        "secret-prompt-authentication",
        "secret-prompt-authentication-rejected",
        "secret-prompt-authentication-accounts",
        "secret-prompt-wifi",
        "secret-prompt-sign-in",
        "secret-prompt-sim-pin",
        "secret-prompt-sim-puk",
        "desktop-control-ask",
        "desktop-control-chip",
        "desktop-control-menu",
        "workspace-menu",
        "window-menu",
        "window-menu-typed",
        "window-menu-ask",
        "tab-rename",
        "window-share-menu",
        "window-shared",
        "window-shared-app",
        "window-held-write",
        "profile-card",
        "dictation-consent",
        "dictation-listening",
        "dictation-finishing",
        "dictation-model-required",
        "agent-composer",
        "agent-composer-typed",
        "agent-composer-answer",
        "agent-composer-working",
        "agent-composer-steps",
        "agent-composer-answer-view",
        "notifications",
        "notifications-one",
        "notifications-dnd",
        "notifications-graphics",
        "notifications-call",
    ];
}

#[cfg(test)]
mod tests {
    use super::ShellDriver;

    /// The scene catalogue is driven in this many parts, one test each, so
    /// the parts run side by side: a fresh driver takes about ten seconds per
    /// scene in a debug build on a busy runner, and six parts of 22 scenes
    /// each averaged 205 s and reached 479 s, crossing the hang bound.
    const SCENE_PARTS: usize = 44;

    /// Drive every scene of part `part` of [`SCENE_PARTS`] and check each
    /// returns Ok.
    fn drive_scene_part(part: usize) {
        let mut env = tilo_test_env::ScopedEnv::new();
        env.set("TILO_UI_DEBUG", "1");
        for &name in scene_part(part) {
            let mut d = ShellDriver::new((1600, 900));
            d.scene_setup(name)
                .unwrap_or_else(|e| panic!("scene_setup({name}) failed: {e}"));
            let _ = d.paint_settled();
        }
    }

    /// The scenes of part `part` of [`SCENE_PARTS`].
    fn scene_part(part: usize) -> &'static [&'static str] {
        let per_part = ShellDriver::ALL_SCENES.len().div_ceil(SCENE_PARTS);
        ShellDriver::ALL_SCENES
            .chunks(per_part)
            .nth(part)
            .unwrap_or(&[])
    }

    /// One test per part, numbered from 1.
    macro_rules! scene_part_tests {
        ($($name:ident = $part:expr),+ $(,)?) => {
            $(#[test]
            fn $name() {
                drive_scene_part($part);
            })+
            /// Every part has a test, and the parts drive the whole catalogue.
            #[test]
            fn driver_scene_parts_cover_the_catalogue() {
                let parts = [$($part),+];
                assert_eq!(parts.len(), SCENE_PARTS);
                let driven: Vec<&str> = parts
                    .iter()
                    .flat_map(|&part| scene_part(part))
                    .copied()
                    .collect();
                assert_eq!(driven, ShellDriver::ALL_SCENES);
            }
        };
    }

    scene_part_tests!(
        driver_scene_setup_covers_the_catalogue_part_1 = 0,
        driver_scene_setup_covers_the_catalogue_part_2 = 1,
        driver_scene_setup_covers_the_catalogue_part_3 = 2,
        driver_scene_setup_covers_the_catalogue_part_4 = 3,
        driver_scene_setup_covers_the_catalogue_part_5 = 4,
        driver_scene_setup_covers_the_catalogue_part_6 = 5,
        driver_scene_setup_covers_the_catalogue_part_7 = 6,
        driver_scene_setup_covers_the_catalogue_part_8 = 7,
        driver_scene_setup_covers_the_catalogue_part_9 = 8,
        driver_scene_setup_covers_the_catalogue_part_10 = 9,
        driver_scene_setup_covers_the_catalogue_part_11 = 10,
        driver_scene_setup_covers_the_catalogue_part_12 = 11,
        driver_scene_setup_covers_the_catalogue_part_13 = 12,
        driver_scene_setup_covers_the_catalogue_part_14 = 13,
        driver_scene_setup_covers_the_catalogue_part_15 = 14,
        driver_scene_setup_covers_the_catalogue_part_16 = 15,
        driver_scene_setup_covers_the_catalogue_part_17 = 16,
        driver_scene_setup_covers_the_catalogue_part_18 = 17,
        driver_scene_setup_covers_the_catalogue_part_19 = 18,
        driver_scene_setup_covers_the_catalogue_part_20 = 19,
        driver_scene_setup_covers_the_catalogue_part_21 = 20,
        driver_scene_setup_covers_the_catalogue_part_22 = 21,
        driver_scene_setup_covers_the_catalogue_part_23 = 22,
        driver_scene_setup_covers_the_catalogue_part_24 = 23,
        driver_scene_setup_covers_the_catalogue_part_25 = 24,
        driver_scene_setup_covers_the_catalogue_part_26 = 25,
        driver_scene_setup_covers_the_catalogue_part_27 = 26,
        driver_scene_setup_covers_the_catalogue_part_28 = 27,
        driver_scene_setup_covers_the_catalogue_part_29 = 28,
        driver_scene_setup_covers_the_catalogue_part_30 = 29,
        driver_scene_setup_covers_the_catalogue_part_31 = 30,
        driver_scene_setup_covers_the_catalogue_part_32 = 31,
        driver_scene_setup_covers_the_catalogue_part_33 = 32,
        driver_scene_setup_covers_the_catalogue_part_34 = 33,
        driver_scene_setup_covers_the_catalogue_part_35 = 34,
        driver_scene_setup_covers_the_catalogue_part_36 = 35,
        driver_scene_setup_covers_the_catalogue_part_37 = 36,
        driver_scene_setup_covers_the_catalogue_part_38 = 37,
        driver_scene_setup_covers_the_catalogue_part_39 = 38,
        driver_scene_setup_covers_the_catalogue_part_40 = 39,
        driver_scene_setup_covers_the_catalogue_part_41 = 40,
        driver_scene_setup_covers_the_catalogue_part_42 = 41,
        driver_scene_setup_covers_the_catalogue_part_43 = 42,
        driver_scene_setup_covers_the_catalogue_part_44 = 43,
    );
}
