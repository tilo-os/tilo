//! The notice a device of the person's at another peer protocol version raises for them.
//!
//! Two devices sync when their peer protocol versions are at most one apart. One outside that
//! window is refused by the workspace state, and the person is told from here, once for each
//! device and version: the notice says which side updates, and names the device when the record of
//! their devices holds it. It is kept in that same record's notices, the path the daemon already
//! carries the person's notices on, so every device of theirs shows it.

use std::sync::Arc;
use std::sync::Mutex;

use tilo_lock::MutexRecover as _;
use tilo_server_protocol::servers::person_workspace_id;
use tilo_workspace_service::workspaces::Workspaces;
use tilo_workspace_service::workspaces::person_workspace;
use tilo_workspace_service::workspaces::person_workspace::devices::{
    DEVICES, Devices, MAX_NOTICE_SHOWN_MS, SharedNotice,
};

use super::workspaces::ServiceWorkspaces;

/// What the notice says: its title and its body, `device` named when the person's record holds it.
#[must_use]
pub(crate) fn text(theirs: u32, device: Option<&str>) -> (String, String) {
    let behind = tilo_server_protocol::auth::this_device_is_behind(theirs);
    match (behind, device) {
        (true, Some(device)) => (
            crate::i18n::t_args(
                "version-mismatch-updating-title",
                &[("device", device.into())],
            ),
            crate::i18n::t("version-mismatch-updating-body"),
        ),
        (false, Some(device)) => (
            crate::i18n::t_args(
                "version-mismatch-needs-update-title",
                &[("device", device.into())],
            ),
            crate::i18n::t("version-mismatch-needs-update-body"),
        ),
        (true, None) => (
            crate::i18n::t("version-mismatch-unknown-updating-title"),
            crate::i18n::t("version-mismatch-unknown-updating-body"),
        ),
        (false, None) => (
            crate::i18n::t("version-mismatch-unknown-needs-update-title"),
            crate::i18n::t("version-mismatch-unknown-needs-update-body"),
        ),
    }
}

/// The devices and versions the person was told about already, each once.
#[derive(Default)]
pub(crate) struct Reported(Mutex<std::collections::BTreeSet<([u8; 32], u32)>>);

impl Reported {
    /// Whether this is the first refusal of `device` at `theirs`: a refusal said again says
    /// nothing new.
    pub(crate) fn first(&self, device: [u8; 32], theirs: u32) -> bool {
        self.0.lock_recover().insert((device, theirs))
    }
}

/// The notice's id in the record of the person's devices: one for each device and version, so
/// saying the same thing again replaces it rather than stacking another.
fn id_of(device: [u8; 32], theirs: u32) -> String {
    let mut hasher = blake3::Hasher::new();
    hasher.update(b"tilo-peer-version-notice-v1\0");
    hasher.update(&device);
    hasher.update(&theirs.to_le_bytes());
    hasher.finalize().to_hex().to_string()
}

/// Tell `person` that their device `device` speaks peer protocol `theirs`, as a notice in the
/// record of their devices kept under `own`.
async fn post(copies: &Workspaces, own: [u8; 32], person: [u8; 32], device: [u8; 32], theirs: u32) {
    let record = person_workspace::devices::of(copies, &person).await;
    let key = hex::encode(device);
    let name = record
        .listed(&person)
        .into_iter()
        .find(|listed| listed.key == key)
        .map(|listed| listed.device.called(&listed.cert));
    let (title, body) = text(theirs, name.as_deref());
    let now = i64::try_from(tilo_clock::unix_now_ms()).unwrap_or(i64::MAX);
    let notice = SharedNotice {
        from: hex::encode(own),
        app_id: tilo_app_id::apps::SHELL.to_owned(),
        app_name: crate::i18n::t("system-update-app-name"),
        title,
        body,
        redacted: false,
        actions: Vec::new(),
        urgency: 1,
        shown_ms: now,
        until_ms: now.saturating_add(MAX_NOTICE_SHOWN_MS),
        ..SharedNotice::default()
    };
    let id = id_of(device, theirs);
    let workspace = person_workspace_id(&person);
    if let Err(error) = person_workspace::apply_on(
        copies,
        &workspace,
        DEVICES,
        Devices::showing_notice(&id, &notice),
    )
    .await
    {
        tracing::warn!(%error, device = %key, "the peer version notice did not reach the person's record");
    }
}

/// Tell the person, once for each device and version, that one of their devices speaks another
/// peer protocol version the daemon refused: the person reads which side updates.
pub(crate) fn watch(workspaces: &Arc<ServiceWorkspaces>) {
    let reported = Arc::new(Reported::default());
    let told = Arc::clone(workspaces);
    let hook: Arc<tilo_peer_state::direct::Versions> = Arc::new(move |device, theirs| {
        if !reported.first(device, theirs) {
            return;
        }
        let cert = told.device_cert();
        let copies = told.copies().clone();
        tilo_rt::app().spawn(async move {
            post(
                &copies,
                cert.device_public_key,
                cert.user_public_key,
                device,
                theirs,
            )
            .await;
        });
    });
    workspaces.observing_versions(hook);
}

#[cfg(test)]
mod tests {
    use super::*;

    /// One device at another version is reported once: a refusal said again says nothing new, and
    /// a device moving to another version is news again.
    #[test]
    fn another_versions_peer_is_reported_once() {
        let reported = Reported::default();
        let device = [0x11; 32];
        let ours = neighbour_wire::PEER_PROTOCOL_VERSION;
        assert!(
            reported.first(device, ours.wrapping_add(2)),
            "the first refusal is news"
        );
        assert!(
            !reported.first(device, ours.wrapping_add(2)),
            "the same device at the same version is not told twice"
        );
        assert!(
            reported.first(device, ours.wrapping_sub(3)),
            "the same device at another version is news again"
        );
        assert!(
            reported.first([0x22; 32], ours.wrapping_add(2)),
            "another device at the same version is news"
        );
    }

    /// The notice names the device and tells the person which side updates, and one the record
    /// does not hold is said plainly rather than spelled out.
    #[test]
    fn a_device_on_another_version_is_named_and_told_what_to_do() {
        let ours = neighbour_wire::PEER_PROTOCOL_VERSION;
        let newer = ours.wrapping_add(2);
        let older = ours.wrapping_sub(2);
        let (title, body) = text(newer, Some("Framework 16"));
        assert!(
            title.contains("Framework 16") && title.contains("newer Tilo"),
            "the device names the title: {title}"
        );
        assert!(
            body.contains("restarts"),
            "the body says what happens here: {body}"
        );
        let (title, body) = text(older, Some("Framework 16"));
        assert!(
            title.contains("Framework 16") && title.contains("must update"),
            "the one behind is the one that updates: {title}"
        );
        assert!(
            body.contains("Update Tilo there"),
            "the body says what to do: {body}"
        );
        for theirs in [newer, older] {
            let (title, body) = text(theirs, None);
            assert!(
                title.to_lowercase().contains("another device"),
                "a device the record cannot name is still named plainly: {title}"
            );
            assert!(
                !title.contains(&hex::encode([0x11; 32]))
                    && !body.contains(&hex::encode([0x11; 32])),
                "an endpoint key is not the person's language"
            );
        }
    }

    /// The banner a person reads: the title names the device on one line and the body says what
    /// happens here in one sentence. Both are painted whole at the banner's own width, never cut
    /// with an ellipsis.
    #[test]
    fn a_version_notice_reads_whole_on_its_banner() {
        let ours = neighbour_wire::PEER_PROTOCOL_VERSION;
        for theirs in [ours.wrapping_add(2), ours.wrapping_sub(2)] {
            for device in [Some("Framework 16"), None] {
                let (title, body) = text(theirs, device);
                let mut text = ui::TextRenderer::new();
                let theme = ui::Theme::dark();
                let words = |text: &mut ui::TextRenderer, width| {
                    ui::widget::toast::painted_words(text, &theme, &title, &body, "Updates", width)
                };
                // A banner card is 360 points wide; at 2000 points nothing needs eliding or
                // clamping. A line break may drop the space that ended it, so the banner may paint
                // exactly one glyph fewer per extra line and not one more.
                let (banner_glyphs, banner_lines) = words(&mut text, 360.0);
                let (whole_glyphs, whole_lines) = words(&mut text, 2000.0);
                assert_eq!(
                    banner_glyphs + (banner_lines - whole_lines),
                    whole_glyphs,
                    "the banner cut the notice: {title} / {body}"
                );
            }
        }
    }
}
