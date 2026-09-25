//! First-party package ids renamed when qOS became Tilo.
//!
//! The single rename table shared by the UI runtime and the collaboration
//! state crates that cannot depend on it (`qos-peer-state` sits below
//! `tilo-ui`). Consult it only when resolving an installed application or a
//! persisted record written under a retired id; new writes use the current
//! `so.tilo.*` id.

/// First-party package ids renamed when qOS became Tilo.
///
/// This table is deliberately finite. It is used only when resolving an
/// installed application definition or its persisted record namespace; it is
/// never consulted by capability, signature, or document-route validation.
#[derive(Clone, Copy)]
struct FirstPartyAppIdFamily {
    qos: &'static str,
    tilo: &'static str,
    current: &'static str,
}

impl FirstPartyAppIdFamily {
    fn contains(self, app_id: &str) -> bool {
        app_id == self.qos || app_id == self.tilo || app_id == self.current
    }

    fn ids(self) -> [&'static str; 3] {
        [self.current, self.tilo, self.qos]
    }
}

const RENAMED_FIRST_PARTY_APP_IDS: &[FirstPartyAppIdFamily] = &[
    FirstPartyAppIdFamily {
        qos: "dev.qos.agent",
        tilo: "dev.tilo.agent",
        current: "so.tilo.agent",
    },
    FirstPartyAppIdFamily {
        qos: "dev.qos.api-tester",
        tilo: "dev.tilo.api-tester",
        current: "so.tilo.api-tester",
    },
    FirstPartyAppIdFamily {
        qos: "dev.qos.backups",
        tilo: "dev.tilo.backups",
        current: "so.tilo.backups",
    },
    FirstPartyAppIdFamily {
        qos: "dev.qos.browser",
        tilo: "dev.tilo.browser",
        current: "so.tilo.browser",
    },
    FirstPartyAppIdFamily {
        qos: "dev.qos.calculator",
        tilo: "dev.tilo.calculator",
        current: "so.tilo.calculator",
    },
    FirstPartyAppIdFamily {
        qos: "dev.qos.calendar",
        tilo: "dev.tilo.calendar",
        current: "so.tilo.calendar",
    },
    FirstPartyAppIdFamily {
        qos: "dev.qos.camera",
        tilo: "dev.tilo.camera",
        current: "so.tilo.camera",
    },
    FirstPartyAppIdFamily {
        qos: "dev.qos.chat",
        tilo: "dev.tilo.chat",
        current: "so.tilo.chat",
    },
    FirstPartyAppIdFamily {
        qos: "dev.qos.code-editor",
        tilo: "dev.tilo.code-editor",
        current: "so.tilo.code-editor",
    },
    FirstPartyAppIdFamily {
        qos: "dev.qos.contacts",
        tilo: "dev.tilo.contacts",
        current: "so.tilo.contacts",
    },
    FirstPartyAppIdFamily {
        qos: "dev.qos.containers",
        tilo: "dev.tilo.containers",
        current: "so.tilo.containers",
    },
    FirstPartyAppIdFamily {
        qos: "dev.qos.db-client",
        tilo: "dev.tilo.db-client",
        current: "so.tilo.db-client",
    },
    FirstPartyAppIdFamily {
        qos: "dev.qos.diffmerge",
        tilo: "dev.tilo.diffmerge",
        current: "so.tilo.diffmerge",
    },
    FirstPartyAppIdFamily {
        qos: "dev.qos.disks",
        tilo: "dev.tilo.disks",
        current: "so.tilo.disks",
    },
    FirstPartyAppIdFamily {
        qos: "dev.qos.ebook",
        tilo: "dev.tilo.ebook",
        current: "so.tilo.ebook",
    },
    FirstPartyAppIdFamily {
        qos: "dev.qos.feeds",
        tilo: "dev.tilo.feeds",
        current: "so.tilo.feeds",
    },
    FirstPartyAppIdFamily {
        qos: "dev.qos.file-explorer",
        tilo: "dev.tilo.file-explorer",
        current: "so.tilo.file-explorer",
    },
    FirstPartyAppIdFamily {
        qos: "dev.qos.firewall",
        tilo: "dev.tilo.firewall",
        current: "so.tilo.firewall",
    },
    FirstPartyAppIdFamily {
        qos: "dev.qos.git-client",
        tilo: "dev.tilo.git-client",
        current: "so.tilo.git-client",
    },
    FirstPartyAppIdFamily {
        qos: "dev.qos.hex-editor",
        tilo: "dev.tilo.hex-editor",
        current: "so.tilo.hex-editor",
    },
    FirstPartyAppIdFamily {
        qos: "dev.qos.imager",
        tilo: "dev.tilo.imager",
        current: "so.tilo.imager",
    },
    FirstPartyAppIdFamily {
        qos: "dev.qos.logs",
        tilo: "dev.tilo.logs",
        current: "so.tilo.logs",
    },
    FirstPartyAppIdFamily {
        qos: "dev.qos.mail",
        tilo: "dev.tilo.mail",
        current: "so.tilo.mail",
    },
    FirstPartyAppIdFamily {
        qos: "dev.qos.maps",
        tilo: "dev.tilo.maps",
        current: "so.tilo.maps",
    },
    FirstPartyAppIdFamily {
        qos: "dev.qos.monitor",
        tilo: "dev.tilo.monitor",
        current: "so.tilo.monitor",
    },
    FirstPartyAppIdFamily {
        qos: "dev.qos.music",
        tilo: "dev.tilo.music",
        current: "so.tilo.music",
    },
    FirstPartyAppIdFamily {
        qos: "dev.qos.notes",
        tilo: "dev.tilo.notes",
        current: "so.tilo.notes",
    },
    FirstPartyAppIdFamily {
        qos: "dev.qos.osk",
        tilo: "dev.tilo.osk",
        current: "so.tilo.osk",
    },
    FirstPartyAppIdFamily {
        qos: "dev.qos.pdf-viewer",
        tilo: "dev.tilo.pdf-viewer",
        current: "so.tilo.pdf-viewer",
    },
    FirstPartyAppIdFamily {
        qos: "dev.qos.photos",
        tilo: "dev.tilo.photos",
        current: "so.tilo.photos",
    },
    FirstPartyAppIdFamily {
        qos: "dev.qos.remote",
        tilo: "dev.tilo.remote",
        current: "so.tilo.remote",
    },
    FirstPartyAppIdFamily {
        qos: "dev.qos.scratchpad",
        tilo: "dev.tilo.scratchpad",
        current: "so.tilo.scratchpad",
    },
    FirstPartyAppIdFamily {
        qos: "dev.qos.settings",
        tilo: "dev.tilo.settings",
        current: "so.tilo.settings",
    },
    FirstPartyAppIdFamily {
        qos: "dev.qos.sheets",
        tilo: "dev.tilo.sheets",
        current: "so.tilo.sheets",
    },
    FirstPartyAppIdFamily {
        qos: "dev.qos.slides",
        tilo: "dev.tilo.slides",
        current: "so.tilo.slides",
    },
    FirstPartyAppIdFamily {
        qos: "dev.qos.snippets",
        tilo: "dev.tilo.snippets",
        current: "so.tilo.snippets",
    },
    FirstPartyAppIdFamily {
        qos: "dev.qos.store",
        tilo: "dev.tilo.store",
        current: "so.tilo.store",
    },
    FirstPartyAppIdFamily {
        qos: "dev.qos.terminal",
        tilo: "dev.tilo.terminal",
        current: "so.tilo.terminal",
    },
    FirstPartyAppIdFamily {
        qos: "dev.qos.themes",
        tilo: "dev.tilo.themes",
        current: "so.tilo.themes",
    },
    FirstPartyAppIdFamily {
        qos: "dev.qos.totp",
        tilo: "dev.tilo.totp",
        current: "so.tilo.totp",
    },
    FirstPartyAppIdFamily {
        qos: "dev.qos.tube",
        tilo: "dev.tilo.tube",
        current: "so.tilo.tube",
    },
    FirstPartyAppIdFamily {
        qos: "dev.qos.typst-editor",
        tilo: "dev.tilo.typst-editor",
        current: "so.tilo.typst-editor",
    },
    FirstPartyAppIdFamily {
        qos: "dev.qos.video-editor",
        tilo: "dev.tilo.video-editor",
        current: "so.tilo.video-editor",
    },
    FirstPartyAppIdFamily {
        qos: "dev.qos.video-player",
        tilo: "dev.tilo.video-player",
        current: "so.tilo.video-player",
    },
    FirstPartyAppIdFamily {
        qos: "dev.qos.vpn",
        tilo: "dev.tilo.vpn",
        current: "so.tilo.vpn",
    },
    FirstPartyAppIdFamily {
        qos: "dev.qos.weather",
        tilo: "dev.tilo.weather",
        current: "so.tilo.weather",
    },
    FirstPartyAppIdFamily {
        qos: "dev.qos.windows",
        tilo: "dev.tilo.windows",
        current: "so.tilo.windows",
    },
    FirstPartyAppIdFamily {
        qos: "dev.qos.word-processor",
        tilo: "dev.tilo.word-processor",
        current: "so.tilo.word-processor",
    },
];

/// Current package registration for a deployed first-party durable id.
///
/// Lookup compatibility only: callers must keep the original id on persisted
/// tabs, record routes, capabilities, and cryptographic domains.
#[must_use]
pub fn current_package_id_for_legacy(app_id: &str) -> Option<&'static str> {
    RENAMED_FIRST_PARTY_APP_IDS.iter().find_map(|family| {
        (app_id == family.qos || app_id == family.tilo).then_some(family.current)
    })
}

/// The exact finite family used for definition/catalogue lookup.
#[must_use]
pub fn first_party_app_id_lookup_candidates(app_id: &str) -> impl Iterator<Item = &'static str> {
    RENAMED_FIRST_PARTY_APP_IDS
        .iter()
        .find(|family| family.contains(app_id))
        .into_iter()
        .flat_map(|family| family.ids())
}

/// Whether two ids name the same explicitly renamed first-party app for
/// definition or persisted-record lookup.
#[must_use]
pub fn lookup_compatible_first_party_app_ids(left: &str, right: &str) -> bool {
    left == right
        || RENAMED_FIRST_PARTY_APP_IDS
            .iter()
            .any(|family| family.contains(left) && family.contains(right))
}

/// Whether a persisted `app_id` names one of the `current` first-party
/// packages, under its current id or a retired id from the rename table.
///
/// For checks over rows, routes and grants that already exist in a workspace:
/// the caller keeps using the persisted spelling for the exact record lookup.
#[must_use]
pub fn is_any_first_party_app(app_id: &str, current: &[&str]) -> bool {
    current
        .iter()
        .any(|current| lookup_compatible_first_party_app_ids(app_id, current))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn retired_ids_resolve_only_within_their_own_family() {
        for id in ["so.tilo.chat", "dev.tilo.chat", "dev.qos.chat"] {
            assert!(is_any_first_party_app(
                id,
                &["so.tilo.word-processor", "so.tilo.chat"]
            ));
        }
        assert!(!is_any_first_party_app("dev.qos.notes", &["so.tilo.chat"]));
        assert!(!is_any_first_party_app(
            "dev.qos.desktop",
            &["so.tilo.chat"]
        ));
        assert!(!is_any_first_party_app("so.tilo.chat", &[]));
    }
}
