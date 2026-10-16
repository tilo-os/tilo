//! Disk unlock: how each encrypted volume enrolled for sibling unlock keeps
//! its secrets, and which Wi-Fi networks early boot may join to ask the
//! owner's other devices (docs/device-reach.md, "Unlocking it after a
//! reboot").
//!
//! The enrolment is described under `/var/lib/tilo/sibling-unlock`, readable
//! by root alone because it names the networks the owner trusts to unlock
//! this machine. Settings never reads the files: it asks `tilo-sibling-unlock
//! volumes` through `pkexec`, which the image's polkit rule grants an
//! administrator at this machine's seat without a prompt. Choosing the
//! networks again runs the same enrolment, `tilo-sibling-unlock enrol
//! --replace-networks`, after administrator authentication.
//!
//! A change is a device-local [`DiskUnlockEffect`]: the buttons here and
//! the Ctrl+K entries build it with [`network_change`], and both reach
//! [`DiskUnlockModel::start`].

use crate::channel;
use crate::effects::{SettingsEffect, effect};
use crate::host::PaneCx;
use crate::panes::model::{PaneModel, Replies, Shared};
use crate::resource::Settings;
use crate::settings_app::SettingsApp;
use serde::Deserialize;
use std::sync::Arc;
#[cfg(test)]
use std::sync::Mutex;
use std::time::Duration;
use ui::framework::ActionControl;
use ui::framework::{EffectAnswer, EffectCx, EffectReply, Effected, LiveRow};
use ui::i18n::ArgValue;
use ui::tilo_resource::{self, Effect};
use ui::widget::{Button, Row, Widget};

/// Read the enrolled disks or replace one disk's whole unlock-network list.
#[derive(Clone, Debug, PartialEq, Eq, tilo_resource::Effect)]
#[effect(operation = "settings.effect")]
pub enum DiskUnlockEffect {
    /// Read this device's enrolled volumes again.
    #[effect(job, idempotent, device_only)]
    RefreshDiskUnlock,
    /// Choose the whole list of Wi-Fi connections a volume may use at boot.
    #[effect(job, row, idempotent, device_only)]
    SetDiskUnlockNetworks {
        #[param("The enrolled volume's LUKS UUID")]
        volume: String,
        #[param("The complete list of saved Wi-Fi connection names")]
        networks: Vec<String>,
    },
}

#[expect(
    clippy::needless_pass_by_value,
    reason = "each area receives its owned variant from the shared Settings effect vocabulary"
)]
pub(crate) fn answer(
    app: &SettingsApp,
    operation: DiskUnlockEffect,
    cx: &mut EffectCx<Settings, SettingsApp>,
) -> EffectAnswer {
    let model = app.disk_unlock().model();
    let mut model = model.borrow_mut();
    if model.busy() {
        return Err(t("settings-disk-unlock-changing"));
    }
    model.start(&operation, Some(cx.reply()));
    Ok(Effected::Pending)
}

/// The host keeps the disk-unlock model when its pane is closed.
pub struct DiskUnlockArea {
    cx: PaneCx,
    model: std::cell::OnceCell<Shared<DiskUnlockModel>>,
}

impl DiskUnlockArea {
    pub fn new(cx: &PaneCx) -> Self {
        Self {
            cx: cx.clone(),
            model: std::cell::OnceCell::new(),
        }
    }

    pub fn model(&self) -> &Shared<DiskUnlockModel> {
        self.model.get_or_init(|| {
            let authority: Arc<dyn UnlockAuthority> = if self.cx.reaches_devices() {
                Arc::new(PolkitAuthority)
            } else {
                Arc::new(DetachedAuthority)
            };
            std::rc::Rc::new(std::cell::RefCell::new(DiskUnlockModel::with_authority(
                self.cx.clone(),
                authority,
            )))
        })
    }

    pub(crate) fn poll(&self) {
        if let Some(model) = self.model.get() {
            model.borrow_mut().poll();
        }
    }

    pub(crate) fn rows(&self) -> Vec<LiveRow> {
        let Some(model) = self.model.get() else {
            return Vec::new();
        };
        let model = model.borrow();
        model
            .shown_volumes()
            .iter()
            .map(|volume| {
                let mut row = LiveRow::new(
                    format!("settings.disk-unlock-volume:{}", volume.volume),
                    "settings.disk-unlock-volume",
                    volume.short_name(),
                );
                for network in candidates(volume, model.saved_wifi()) {
                    row = row.row(network_resource(volume, &network));
                }
                row
            })
            .collect()
    }
}

struct DetachedAuthority;
impl UnlockAuthority for DetachedAuthority {
    fn volumes(&self) -> Result<Vec<UnlockVolume>, String> {
        Ok(Vec::new())
    }
    fn saved_wifi(&self) -> Result<Vec<String>, String> {
        Ok(Vec::new())
    }
    fn set_networks(&self, _: &UnlockVolume, _: &[String]) -> Result<(), String> {
        Err("disk unlock is unavailable outside a live device host".into())
    }
}

use crate::i18n::{t, t_args};
use crate::widgets::{
    Item, SettingsSection, StatusTone, card, card_with_action, error_caption, note, row, rows,
    semantic_status_pill, setting_row, setting_row_described, value_row,
};

/// The sibling-unlock client; polkit's `org.tilo.disk-unlock.manage` names it.
pub const HELPER: &str = "/usr/bin/tilo-sibling-unlock";
const PKEXEC: &str = "pkexec";
const READ_TIMEOUT: Duration = Duration::from_secs(30);
/// Long enough for the person to answer the authentication dialog.
const CHANGE_TIMEOUT: Duration = Duration::from_secs(300);
const OUTPUT_LIMIT: usize = 256 * 1024;
/// More than any machine has disks; a longer answer is refused.
const MAX_VOLUMES: usize = 64;
/// The unlock networks one volume may name.
pub const MAX_NETWORKS: usize = 16;

/// How a volume's device half and unlock-network passwords are stored in its
/// LUKS2 header.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Sealing {
    /// Sealed to this machine's TPM.
    Tpm2,
    /// In the clear: anyone holding the disk can read them.
    Cleartext,
}

/// One enrolled volume, as `tilo-sibling-unlock volumes` prints it.
#[derive(Clone, Debug, PartialEq, Eq, Deserialize)]
pub struct UnlockVolume {
    /// The LUKS UUID.
    pub volume: String,
    /// `/dev/disk/by-uuid/<volume>`.
    pub device: String,
    pub keyslot: u32,
    pub sealing: Sealing,
    /// The Wi-Fi networks early boot may join, by connection name.
    #[serde(default)]
    pub networks: Vec<String>,
    /// The devices asked at boot, by label.
    #[serde(default)]
    pub siblings: Vec<String>,
}

impl UnlockVolume {
    /// A short name for the volume: the start of its UUID.
    #[must_use]
    pub fn short_name(&self) -> String {
        t_args(
            "settings-disk-unlock-volume",
            &[(
                "id",
                ArgValue::Text(self.volume.chars().take(8).collect::<String>()),
            )],
        )
    }
}

/// A LUKS UUID in the form `/dev/disk/by-uuid` uses.
#[must_use]
pub fn valid_volume(volume: &str) -> bool {
    volume.len() == 36
        && volume.bytes().enumerate().all(|(i, b)| {
            if matches!(i, 8 | 13 | 18 | 23) {
                b == b'-'
            } else {
                b.is_ascii_hexdigit()
            }
        })
}

/// A connection name the enrolment accepts (the rule early boot applies to
/// the file it writes).
#[must_use]
pub fn valid_network(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= 64
        && name
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.' | ' '))
        && !name.starts_with('.')
}

/// Parses the helper's answer, refusing anything that is not a well-formed
/// enrolment: a volume Settings would hand back to `pkexec` must be one.
///
/// # Errors
/// When the JSON is malformed or names an unusable volume or network.
pub fn parse_volumes(json: &str) -> Result<Vec<UnlockVolume>, String> {
    let volumes: Vec<UnlockVolume> = serde_json::from_str(json).map_err(|error| {
        t_args(
            "settings-disk-unlock-malformed",
            &[("error", error.to_string().into())],
        )
    })?;
    if volumes.len() > MAX_VOLUMES {
        return Err(t("settings-disk-unlock-too-many"));
    }
    for volume in &volumes {
        let device_ok = volume
            .device
            .strip_prefix("/dev/disk/by-uuid/")
            .is_some_and(|rest| rest == volume.volume);
        if !valid_volume(&volume.volume)
            || !device_ok
            || volume.networks.len() > MAX_NETWORKS
            || !volume.networks.iter().all(|name| valid_network(name))
        {
            return Err(t_args(
                "settings-disk-unlock-malformed",
                &[("error", volume.volume.clone().into())],
            ));
        }
    }
    Ok(volumes)
}

/// The operation that uses (`using`) or stops using `network` to unlock
/// `volume`: the whole new list, so a replay asks for one definite state.
#[must_use]
pub fn network_change(volume: &UnlockVolume, network: &str, using: bool) -> DiskUnlockEffect {
    let (volume, networks) = network_arguments(volume, network, using);
    DiskUnlockEffect::SetDiskUnlockNetworks { volume, networks }
}

fn network_arguments(volume: &UnlockVolume, network: &str, using: bool) -> (String, Vec<String>) {
    let mut networks: Vec<String> = volume
        .networks
        .iter()
        .filter(|name| name.as_str() != network)
        .cloned()
        .collect();
    if using {
        networks.push(network.to_owned());
    }
    (volume.volume.clone(), networks)
}

fn network_resource(volume: &UnlockVolume, network: &str) -> LiveRow {
    LiveRow::new(
        format!(
            "settings.disk-unlock-network:{}.{}",
            volume.volume,
            hex::encode(network.as_bytes())
        ),
        "settings.disk-unlock-network",
        network.to_owned(),
    )
    .operation(
        SettingsEffect::from(network_change(
            volume,
            network,
            !volume.networks.iter().any(|name| name == network),
        ))
        .bound(),
    )
}

/// The Wi-Fi networks a volume's controls offer: the chosen ones first, then
/// every other saved Wi-Fi connection the enrolment could take.
#[must_use]
pub fn candidates(volume: &UnlockVolume, saved_wifi: &[String]) -> Vec<String> {
    let mut out = volume.networks.clone();
    for name in saved_wifi {
        if valid_network(name) && !out.contains(name) {
            out.push(name.clone());
        }
    }
    out
}

/// The privileged reads and the enrolment, behind a seam for tests.
pub trait UnlockAuthority: Send + Sync {
    /// Every enrolled volume.
    ///
    /// # Errors
    /// When the helper cannot run or is not authorised.
    fn volumes(&self) -> Result<Vec<UnlockVolume>, String>;

    /// The saved Wi-Fi connections, by name.
    ///
    /// # Errors
    /// When `NetworkManager` cannot be asked.
    fn saved_wifi(&self) -> Result<Vec<String>, String>;

    /// Chooses `volume`'s unlock networks again.
    ///
    /// # Errors
    /// When authentication is refused or the enrolment fails.
    fn set_networks(&self, volume: &UnlockVolume, networks: &[String]) -> Result<(), String>;
}

/// The real authority: `pkexec tilo-sibling-unlock …` and `nmcli`.
pub struct PolkitAuthority;

fn helper_failure(output: &bounded_command::Output) -> String {
    // pkexec: the dialog was dismissed or authentication was refused.
    if let Some(126 | 127) = output.status.code() {
        return t("settings-disk-unlock-not-authorised");
    }
    let stderr = String::from_utf8_lossy(&output.stderr);
    let said = stderr
        .lines()
        .rev()
        .find(|line| !line.trim().is_empty())
        .unwrap_or("")
        .trim()
        .trim_start_matches("Error: ");
    if said.is_empty() {
        format!("exited with {}", output.status)
    } else {
        said.to_owned()
    }
}

impl UnlockAuthority for PolkitAuthority {
    fn volumes(&self) -> Result<Vec<UnlockVolume>, String> {
        let output = bounded_command::run(
            PKEXEC,
            &[HELPER, "volumes"],
            None,
            OUTPUT_LIMIT,
            READ_TIMEOUT,
        )
        .map_err(|error| error.to_string())?;
        if !output.status.success() {
            return Err(helper_failure(&output));
        }
        parse_volumes(&String::from_utf8_lossy(&output.stdout))
    }

    fn saved_wifi(&self) -> Result<Vec<String>, String> {
        Ok(os_actions::network::list_saved_network_profiles()?
            .into_iter()
            .filter(|profile| profile.kind == "802-11-wireless")
            .map(|profile| profile.name)
            .collect())
    }

    fn set_networks(&self, volume: &UnlockVolume, networks: &[String]) -> Result<(), String> {
        let mut args: Vec<&str> = vec![HELPER, "enrol", &volume.device, "--replace-networks"];
        for network in networks {
            args.extend(["--network", network.as_str()]);
        }
        let output = bounded_command::run(PKEXEC, &args, None, OUTPUT_LIMIT, CHANGE_TIMEOUT)
            .map_err(|error| error.to_string())?;
        if output.status.success() {
            Ok(())
        } else {
            Err(helper_failure(&output))
        }
    }
}

struct Snapshot {
    volumes: Result<Vec<UnlockVolume>, String>,
    saved_wifi: Result<Vec<String>, String>,
}

fn read(authority: &dyn UnlockAuthority) -> Snapshot {
    Snapshot {
        volumes: authority.volumes(),
        saved_wifi: authority.saved_wifi(),
    }
}

/// Runs one change against a fresh read, so a stale card or a replayed
/// request acts on the volume as it is now.
fn change(authority: &dyn UnlockAuthority, operation: &DiskUnlockEffect) -> Result<String, String> {
    match operation {
        DiskUnlockEffect::RefreshDiskUnlock => Ok(t("settings-disk-unlock-refreshed")),
        DiskUnlockEffect::SetDiskUnlockNetworks { volume, networks } => {
            if !valid_volume(volume)
                || networks.len() > MAX_NETWORKS
                || !networks.iter().all(|name| valid_network(name))
            {
                return Err(t("settings-disk-unlock-bad-request"));
            }
            let current = authority.volumes()?;
            let target = current
                .iter()
                .find(|candidate| &candidate.volume == volume)
                .ok_or_else(|| t("settings-disk-unlock-gone"))?;
            let mut unique: Vec<String> = Vec::new();
            for name in networks {
                if !unique.contains(name) {
                    unique.push(name.clone());
                }
            }
            authority.set_networks(target, &unique)?;
            Ok(if unique.is_empty() {
                t("settings-disk-unlock-wired-only")
            } else {
                t_args(
                    "settings-disk-unlock-saved",
                    &[("networks", unique.join(", ").into())],
                )
            })
        }
    }
}

/// The Disk unlock section of Privacy & Security.
pub struct DiskUnlockModel {
    authority: Arc<dyn UnlockAuthority>,
    /// `None` until the first read answers.
    volumes: Option<Result<Vec<UnlockVolume>, String>>,
    saved_wifi: Result<Vec<String>, String>,
    read_rx: Option<channel::Receiver<Snapshot>>,
    change_rx: Option<channel::Receiver<(DiskUnlockEffect, Result<String, String>)>>,
    /// The last change's outcome: `Ok` said plainly, `Err` as a failure.
    outcome: Option<Result<String, String>>,
    cx: PaneCx,
    replies: Replies,
    structure_dirty: bool,
}

impl DiskUnlockModel {
    /// A model over `authority`, reading it once.
    pub fn with_authority(cx: PaneCx, authority: Arc<dyn UnlockAuthority>) -> Self {
        let mut card = Self {
            authority,
            cx,
            replies: Replies::default(),
            volumes: None,
            saved_wifi: Ok(Vec::new()),
            read_rx: None,
            change_rx: None,
            outcome: None,
            structure_dirty: false,
        };
        card.request_read();
        card
    }

    fn request_read(&mut self) {
        if self.read_rx.is_some() {
            return;
        }
        let authority = Arc::clone(&self.authority);
        let (tx, rx) = channel::channel();
        match crate::device_read::thread(
            "tilo-disk-unlock-read",
            Box::new(move || {
                let _ = tx.send(read(authority.as_ref()));
            }),
        ) {
            Ok(()) => self.read_rx = Some(rx),
            Err(error) => self.volumes = Some(Err(error.to_string())),
        }
    }

    /// Starts `operation`: what a button and a Ctrl+K entry both reach.
    pub fn start(&mut self, operation: &DiskUnlockEffect, reply: Option<EffectReply>) {
        if let Some(reply) = reply {
            self.replies.wait(operation.id(), reply);
        }
        if self.change_rx.is_some() {
            self.finish(operation, Err(t("settings-disk-unlock-busy")));
            return;
        }
        if *operation == DiskUnlockEffect::RefreshDiskUnlock {
            self.request_read();
            self.finish(operation, Ok(t("settings-disk-unlock-refreshed")));
            return;
        }
        let authority = Arc::clone(&self.authority);
        let (tx, rx) = channel::channel();
        let sent = operation.clone();
        match crate::device_read::thread(
            "tilo-disk-unlock-change",
            Box::new(move || {
                let result = change(authority.as_ref(), &sent);
                let _ = tx.send((sent, result));
            }),
        ) {
            Ok(()) => {
                self.change_rx = Some(rx);
                self.outcome = None;
                self.structure_dirty = true;
            }
            Err(error) => self.finish(operation, Err(error.to_string())),
        }
    }

    fn finish(&mut self, operation: &DiskUnlockEffect, result: Result<String, String>) {
        self.replies.finish(operation.id(), result.clone());
        if *operation != DiskUnlockEffect::RefreshDiskUnlock {
            self.outcome = Some(result);
        }
        self.structure_dirty = true;
    }

    /// Waits for the background work started so far (tests).
    #[cfg(test)]
    fn settle(&mut self) {
        if let Some(rx) = self.change_rx.take()
            && let Ok((operation, result)) = rx.recv()
        {
            self.finish(&operation, result);
            self.request_read();
        }
        if let Some(rx) = self.read_rx.take()
            && let Ok(snapshot) = rx.recv()
        {
            self.adopt(snapshot);
        }
    }

    fn adopt(&mut self, snapshot: Snapshot) {
        self.volumes = Some(snapshot.volumes);
        self.saved_wifi = snapshot.saved_wifi;
        self.structure_dirty = true;
    }

    fn poll(&mut self) {
        if let Some(snapshot) = self.read_rx.as_ref().and_then(|rx| rx.try_recv().ok()) {
            self.read_rx = None;
            self.adopt(snapshot);
        }
        if let Some((operation, result)) = self.change_rx.as_ref().and_then(|rx| rx.try_recv().ok())
        {
            self.change_rx = None;
            self.finish(&operation, result);
            self.request_read();
        }
    }

    fn shown_volumes(&self) -> &[UnlockVolume] {
        match &self.volumes {
            Some(Ok(volumes)) => volumes,
            _ => &[],
        }
    }

    fn saved_wifi(&self) -> &[String] {
        self.saved_wifi.as_deref().unwrap_or(&[])
    }

    fn build_composition(&self) -> Box<dyn Widget> {
        let refresh = Box::new(self.cx.bind_invocation(
            Button::new(
                "settings.disk-unlock.refresh",
                &t("settings-disk-unlock-refresh"),
            ),
            DiskUnlockEffect::RefreshDiskUnlock,
        ));
        let mut body: Vec<Item> = vec![note(&t("settings-disk-unlock-intro"))];
        match &self.volumes {
            None => body.push(note(&t("settings-disk-unlock-loading"))),
            Some(Err(error)) => body.push(row(error_caption(&t_args(
                "settings-disk-unlock-read-error",
                &[("error", error.as_str().into())],
            )))),
            Some(Ok(volumes)) if volumes.is_empty() => {
                body.push(note(&t("settings-disk-unlock-none")));
            }
            Some(Ok(_)) => {}
        }
        if self.change_rx.is_some() {
            body.push(note(&t("settings-disk-unlock-changing")));
        }
        match &self.outcome {
            Some(Ok(message)) => body.push(note(message)),
            Some(Err(message)) => body.push(row(error_caption(&t_args(
                "settings-disk-unlock-change-error",
                &[("error", message.as_str().into())],
            )))),
            None => {}
        }
        let mut items: Vec<SettingsSection> = vec![card_with_action(
            &t("settings-disk-unlock-title"),
            refresh,
            rows(body),
        )];
        for volume in self.shown_volumes() {
            items.extend(self.volume_cards(volume));
        }
        crate::widgets::cards(items)
    }

    /// One volume: how its secrets are kept, then the networks it may unlock over.
    fn volume_cards(&self, volume: &UnlockVolume) -> [SettingsSection; 2] {
        let (pill, tone, explanation) = match volume.sealing {
            Sealing::Tpm2 => (
                t("settings-disk-unlock-sealed"),
                StatusTone::Success,
                t("settings-disk-unlock-sealed-body"),
            ),
            Sealing::Cleartext => (
                t("settings-disk-unlock-cleartext"),
                StatusTone::Warning,
                t("settings-disk-unlock-cleartext-body"),
            ),
        };
        let secrets = vec![
            setting_row_described(
                &t("settings-disk-unlock-secrets"),
                &explanation,
                semantic_status_pill(&pill, tone),
            ),
            value_row(
                &t("settings-disk-unlock-siblings"),
                &if volume.siblings.is_empty() {
                    t("settings-disk-unlock-no-siblings")
                } else {
                    volume.siblings.join(", ")
                },
            ),
        ];
        let mut body: Vec<Item> = vec![note(&t(match volume.sealing {
            Sealing::Tpm2 => "settings-disk-unlock-networks-sealed",
            Sealing::Cleartext => "settings-disk-unlock-networks-cleartext",
        }))];
        if let Err(error) = &self.saved_wifi {
            body.push(row(error_caption(&t_args(
                "settings-disk-unlock-wifi-error",
                &[("error", error.as_str().into())],
            ))));
        }
        let offered = candidates(volume, self.saved_wifi());
        if offered.is_empty() {
            body.push(note(&t("settings-disk-unlock-no-wifi")));
        }
        let busy = self.change_rx.is_some();
        for network in offered {
            body.push(self.network_row(volume, &network, busy));
        }
        [
            card(&volume.short_name(), rows(secrets)),
            card(&t("settings-disk-unlock-networks"), rows(body)),
        ]
    }

    fn network_row(&self, volume: &UnlockVolume, network: &str, busy: bool) -> Item {
        let chosen = volume.networks.iter().any(|name| name == network);
        let saved = self.saved_wifi().iter().any(|name| name == network);
        let key = format!(
            "settings.disk-unlock.network.{}.{}",
            volume.volume,
            hex::encode(network.as_bytes())
        );
        let label = t(if chosen {
            "settings-disk-unlock-stop"
        } else {
            "settings-disk-unlock-use"
        });
        let mut control = Row::new()
            .no_wrap()
            .with_align_items(taffy::AlignItems::Center);
        if chosen {
            control = control.push(semantic_status_pill(
                &t("settings-disk-unlock-chosen"),
                StatusTone::Info,
            ));
        }
        let args = network_arguments(volume, network, !chosen);
        let binding = self.cx.row_binding(
            &network_resource(volume, network).into_resource(),
            effect(DiskUnlockEffect::SET_DISK_UNLOCK_NETWORKS).map(move |()| args.clone()),
        );
        let button = Button::new(&key, &label);
        let button = match binding {
            Some(binding) => button.bind_action(binding).with_enabled(!busy),
            None => button.unavailable(),
        };
        let control = Box::new(control.push(Box::new(button)));
        if chosen && !saved && self.saved_wifi.is_ok() {
            setting_row_described(network, &t("settings-disk-unlock-unsaved"), control)
        } else {
            setting_row(network, control)
        }
    }
}

impl PaneModel for DiskUnlockModel {
    fn compose(&self) -> Box<dyn Widget> {
        self.build_composition()
    }
    fn poll(&mut self) -> bool {
        DiskUnlockModel::poll(self);
        std::mem::take(&mut self.structure_dirty)
    }
    fn name(&self) -> &'static str {
        "DiskUnlockCard"
    }
    fn busy(&self) -> bool {
        self.read_rx.is_some() || self.change_rx.is_some()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use automerge::transaction::Transactable;

    const VOLUME: &str = "0f3a9c1e-1111-4222-8333-444455556666";

    fn volume(sealing: Sealing, networks: &[&str]) -> UnlockVolume {
        UnlockVolume {
            volume: VOLUME.into(),
            device: format!("/dev/disk/by-uuid/{VOLUME}"),
            keyslot: 2,
            sealing,
            networks: networks.iter().map(|name| (*name).to_owned()).collect(),
            siblings: vec!["ada's laptop".into()],
        }
    }

    struct FakeAuthority {
        volumes: Mutex<Result<Vec<UnlockVolume>, String>>,
        wifi: Vec<String>,
        calls: Mutex<Vec<(String, Vec<String>)>>,
        refuse: bool,
    }

    impl FakeAuthority {
        fn new(volumes: Result<Vec<UnlockVolume>, String>) -> Arc<Self> {
            Arc::new(Self {
                volumes: Mutex::new(volumes),
                wifi: vec!["Home".into(), "Office 5G".into()],
                calls: Mutex::new(Vec::new()),
                refuse: false,
            })
        }
    }

    impl UnlockAuthority for FakeAuthority {
        fn volumes(&self) -> Result<Vec<UnlockVolume>, String> {
            self.volumes.lock().unwrap().clone()
        }
        fn saved_wifi(&self) -> Result<Vec<String>, String> {
            Ok(self.wifi.clone())
        }
        fn set_networks(&self, volume: &UnlockVolume, networks: &[String]) -> Result<(), String> {
            if self.refuse {
                return Err(t("settings-disk-unlock-not-authorised"));
            }
            self.calls
                .lock()
                .unwrap()
                .push((volume.device.clone(), networks.to_vec()));
            if let Ok(volumes) = self.volumes.lock().unwrap().as_mut() {
                for shown in volumes
                    .iter_mut()
                    .filter(|shown| shown.volume == volume.volume)
                {
                    shown.networks = networks.to_vec();
                }
            }
            Ok(())
        }
    }

    fn texts(widget: &dyn Widget) -> Vec<String> {
        let mut out = Vec::new();
        let mut stack = vec![widget];
        while let Some(next) = stack.pop() {
            if let Some(node) = next.accessibility() {
                out.extend(node.name);
                out.extend(node.value);
            }
            stack.extend(next.children());
        }
        out
    }

    fn test_cx() -> PaneCx {
        PaneCx::in_file(std::env::temp_dir().join("tilo-disk-unlock-test-settings.json"))
    }

    fn laid_out(card: &DiskUnlockModel) -> Vec<String> {
        texts(card.compose().as_ref())
    }

    /// The helper's answer becomes volumes only when every field Settings
    /// would hand back to pkexec is well-formed.
    #[test]
    fn only_well_formed_enrolments_are_read() {
        let json = serde_json::json!([{
            "volume": VOLUME,
            "device": format!("/dev/disk/by-uuid/{VOLUME}"),
            "keyslot": 2,
            "sealing": "cleartext",
            "networks": ["Home"],
            "siblings": ["laptop"],
        }])
        .to_string();
        let volumes = parse_volumes(&json).unwrap();
        assert_eq!(volumes[0].sealing, Sealing::Cleartext);
        assert_eq!(volumes[0].networks, ["Home"]);
        assert!(parse_volumes("[]").unwrap().is_empty());

        let elsewhere = json.replace("/dev/disk/by-uuid/", "/dev/");
        assert!(parse_volumes(&elsewhere).is_err());
        let hostile = json.replace("\"Home\"", "\"../../etc/x\"");
        assert!(parse_volumes(&hostile).is_err());
        assert!(parse_volumes("{\"volume\":1}").is_err());
    }

    /// Cleartext secrets are said to be readable by anyone holding the disk;
    /// TPM-sealed ones are not described that way.
    #[test]
    fn the_card_says_how_secrets_are_kept() {
        let authority = FakeAuthority::new(Ok(vec![volume(Sealing::Cleartext, &["Home"])]));
        let mut card = DiskUnlockModel::with_authority(test_cx(), authority);
        card.settle();
        let shown = laid_out(&card).join("\n");
        assert!(
            shown.contains(&t("settings-disk-unlock-cleartext")),
            "{shown}"
        );
        assert!(
            shown.contains("Anyone holding this disk can read"),
            "{shown}"
        );
        assert!(
            shown.contains("Home") && shown.contains("Office 5G"),
            "{shown}"
        );
        assert!(shown.contains("ada's laptop"), "{shown}");

        let authority = FakeAuthority::new(Ok(vec![volume(Sealing::Tpm2, &[])]));
        let mut card = DiskUnlockModel::with_authority(test_cx(), authority);
        card.settle();
        let shown = laid_out(&card).join("\n");
        assert!(shown.contains(&t("settings-disk-unlock-sealed")), "{shown}");
        assert!(!shown.contains("Anyone holding this disk"), "{shown}");
    }

    /// A read the helper refused is shown as that failure, never as "no
    /// enrolled disks".
    #[test]
    fn a_refused_read_is_not_an_empty_list() {
        let authority = FakeAuthority::new(Err("Not authorized".into()));
        let mut card = DiskUnlockModel::with_authority(test_cx(), authority);
        card.settle();
        let shown = laid_out(&card).join("\n");
        assert!(shown.contains("Not authorized"), "{shown}");
        assert!(!shown.contains(&t("settings-disk-unlock-none")), "{shown}");
    }

    /// The network button and the Ctrl+K entry build one operation; it runs
    /// the enrolment with the whole new list against the volume's device.
    #[test]
    fn a_network_button_runs_the_enrolment_with_the_new_list() {
        let authority = FakeAuthority::new(Ok(vec![volume(Sealing::Cleartext, &["Home"])]));
        let mut app = ui::prelude::TestApp::<crate::SettingsShell>::new().unwrap();
        app.surface::<SettingsApp, _>(|app| {
            let mut model = DiskUnlockModel::with_authority(app.host().clone(), authority.clone());
            model.settle();
            *app.disk_unlock().model().borrow_mut() = model;
            app.select(crate::categories::Category::Permissions);
        })
        .unwrap();
        app.harness().resize((1280, 4096)).unwrap();
        app.settle();
        let heads = app.read_document(Transactable::base_heads);
        let button = format!(
            "settings.disk-unlock.network.{VOLUME}.{}",
            hex::encode("Office 5G")
        );
        app.harness().click_named(&button).unwrap();
        app.harness().wait_until(Duration::from_secs(10), |_| {
            authority.calls.lock().unwrap().len() == 1
        });
        app.surface::<SettingsApp, _>(|app| app.disk_unlock().model().borrow_mut().settle())
            .unwrap();
        assert_eq!(
            authority.calls.lock().unwrap().as_slice(),
            [(
                format!("/dev/disk/by-uuid/{VOLUME}"),
                vec!["Home".to_owned(), "Office 5G".to_owned()]
            )]
        );
        app.surface::<SettingsApp, _>(|app| {
            assert_eq!(
                app.disk_unlock().model().borrow().shown_volumes()[0].networks,
                ["Home", "Office 5G"]
            );
        })
        .unwrap();

        // The palette's operation for "stop using Home" is the same value the
        // button builds, and it reaches the same enrolment.
        let operation = network_change(
            &volume(Sealing::Cleartext, &["Home", "Office 5G"]),
            "Home",
            false,
        );
        assert_eq!(
            operation,
            DiskUnlockEffect::SetDiskUnlockNetworks {
                volume: VOLUME.into(),
                networks: vec!["Office 5G".into()],
            }
        );
        app.effect_on(
            &format!(
                "settings.disk-unlock-network:{VOLUME}.{}",
                hex::encode("Home")
            ),
            &SettingsEffect::from(operation),
        )
        .unwrap();
        assert!(
            app.effect_progress()
                .iter()
                .any(|update| matches!(update, tilo_actions::InvocationUpdate::Succeeded { .. }))
        );
        assert_eq!(app.read_document(Transactable::base_heads), heads);
        assert_eq!(
            authority.calls.lock().unwrap().last().unwrap().1,
            vec!["Office 5G".to_owned()]
        );
        app.surface::<SettingsApp, _>(|app| {
            let model = app.disk_unlock().model().borrow();
            assert!(
                matches!(&model.outcome, Some(Ok(message)) if message == &t_args(
                "settings-disk-unlock-saved", &[("networks", "Office 5G".into())]))
            );
        })
        .unwrap();
    }

    /// A refused authentication or a volume that is gone is reported, and
    /// nothing is enrolled.
    #[test]
    fn a_refused_or_stale_change_reports_and_changes_nothing() {
        let authority = Arc::new(FakeAuthority {
            volumes: Mutex::new(Ok(vec![volume(Sealing::Tpm2, &[])])),
            wifi: vec!["Home".into()],
            calls: Mutex::new(Vec::new()),
            refuse: true,
        });
        let mut card = DiskUnlockModel::with_authority(test_cx(), authority);
        card.settle();
        card.start(
            &network_change(&volume(Sealing::Tpm2, &[]), "Home", true),
            None,
        );
        card.settle();
        assert!(
            matches!(&card.outcome, Some(Err(message)) if message == &t("settings-disk-unlock-not-authorised"))
        );

        let authority = FakeAuthority::new(Ok(Vec::new()));
        assert_eq!(
            change(
                authority.as_ref(),
                &network_change(&volume(Sealing::Tpm2, &[]), "Home", true)
            ),
            Err(t("settings-disk-unlock-gone"))
        );
        assert_eq!(
            change(
                authority.as_ref(),
                &DiskUnlockEffect::SetDiskUnlockNetworks {
                    volume: "../../dev/sda".into(),
                    networks: Vec::new(),
                }
            ),
            Err(t("settings-disk-unlock-bad-request"))
        );
        assert!(authority.calls.lock().unwrap().is_empty());
    }
    #[test]
    fn disk_unlock_effects_refuse_remote_callers_without_enrolling() {
        let authority = FakeAuthority::new(Ok(vec![volume(Sealing::Tpm2, &["Home"])]));
        let mut app = ui::prelude::TestApp::<crate::SettingsShell>::new().unwrap();
        app.surface::<SettingsApp, _>(|app| {
            assert!(app.disk_unlock().model.get().is_none());
            assert!(app.disk_unlock().rows().is_empty());
            let mut model = DiskUnlockModel::with_authority(app.host().clone(), authority.clone());
            model.settle();
            *app.disk_unlock().model().borrow_mut() = model;
        })
        .unwrap();
        let operation = SettingsEffect::from(DiskUnlockEffect::RefreshDiskUnlock);
        app.effect_by("remote-collaborator", &operation)
            .expect_err("device-only read must refuse a remote collaborator");
        assert!(authority.calls.lock().unwrap().is_empty());
    }
}
