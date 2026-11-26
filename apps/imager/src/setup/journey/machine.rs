//! The install plan an "on another computer" preparation makes: which OS that
//! computer installs, which choices this plan makes now and which it leaves for
//! the computer to answer against its own hardware, how the plan reaches it,
//! and the plan itself once it exists.
//!
//! The plan and its single-use token come from [`install_plan`], the same code
//! the command writes, so the window and `tilo machine add` write one document.

use std::path::{Path, PathBuf};

use install_profile_model::plan::{self, DiskRule, Network, Plan};
use tilo_ui_base::InvocationCall;
use ui::tilo_actions::remote::InvokeArgument;
use ui::tilo_actions::remote::machines;

/// The OS the other computer installs. Tilo Server is a destination OS like
/// Tilo OS, not a purpose for a desktop.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Os {
    TiloOs,
    TiloServer,
}

impl Os {
    pub(super) fn key(self) -> &'static str {
        match self {
            Self::TiloOs => "os",
            Self::TiloServer => "server",
        }
    }

    pub(super) fn from_key(key: &str) -> Option<Self> {
        match key {
            "os" => Some(Self::TiloOs),
            "server" => Some(Self::TiloServer),
            _ => None,
        }
    }

    pub(super) fn destination(self) -> &'static str {
        match self {
            Self::TiloOs => "tilo-os",
            Self::TiloServer => "tilo-server",
        }
    }
}

/// What carries the plan to the computer.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(super) enum Delivery {
    #[default]
    Usb,
    Netboot,
    Alongside,
}

impl Delivery {
    pub(super) fn key(self) -> &'static str {
        match self {
            Self::Usb => "usb",
            Self::Netboot => "netboot",
            Self::Alongside => "alongside",
        }
    }

    pub(super) fn from_key(key: &str) -> Option<Self> {
        match key {
            "usb" => Some(Self::Usb),
            "netboot" => Some(Self::Netboot),
            "alongside" => Some(Self::Alongside),
            _ => None,
        }
    }
}

/// The disk rule the plan names: a rule, because the computer's real drives
/// are only known when it installs.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Disks {
    Largest,
    Empty,
    Only,
}

impl Disks {
    pub(super) fn key(self) -> &'static str {
        match self {
            Self::Largest => "largest",
            Self::Empty => "empty",
            Self::Only => "only",
        }
    }

    pub(super) fn from_key(key: &str) -> Option<Self> {
        match key {
            "largest" => Some(Self::Largest),
            "empty" => Some(Self::Empty),
            "only" => Some(Self::Only),
            _ => None,
        }
    }

    pub(super) fn rule(self) -> DiskRule {
        DiskRule {
            kind: self.key().to_owned(),
            identity: String::new(),
        }
    }
}

/// How its network is set up.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Networks {
    Automatic,
}

impl Networks {
    pub(super) fn key(self) -> &'static str {
        match self {
            Self::Automatic => "dhcp",
        }
    }

    pub(super) fn from_key(key: &str) -> Option<Self> {
        match key {
            "dhcp" => Some(Self::Automatic),
            _ => None,
        }
    }
}

/// How it unlocks after a boot.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Unlock {
    Hardware,
    OwnerDevice,
    Passphrase,
}

impl Unlock {
    pub(super) fn key(self) -> &'static str {
        match self {
            Self::Hardware => "tpm",
            Self::OwnerDevice => "owner-device",
            Self::Passphrase => "passphrase",
        }
    }

    pub(super) fn from_key(key: &str) -> Option<Self> {
        match key {
            "tpm" => Some(Self::Hardware),
            "owner-device" => Some(Self::OwnerDevice),
            "passphrase" => Some(Self::Passphrase),
            _ => None,
        }
    }
}

/// The updates it follows.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Updates {
    Stable,
    Development,
}

impl Updates {
    pub(super) fn key(self) -> &'static str {
        match self {
            Self::Stable => "stable",
            Self::Development => "dev",
        }
    }

    pub(super) fn from_key(key: &str) -> Option<Self> {
        match key {
            "stable" => Some(Self::Stable),
            "dev" => Some(Self::Development),
            _ => None,
        }
    }
}

/// What a server holds: nothing answered here, or left for the person's
/// devices to answer in its Servers entry.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Hosting {
    HoldsNone,
    Open,
}

impl Hosting {
    pub(super) fn key(self) -> &'static str {
        match self {
            Self::HoldsNone => "none",
            Self::Open => "open",
        }
    }

    pub(super) fn from_key(key: &str) -> Option<Self> {
        match key {
            "none" => Some(Self::HoldsNone),
            "open" => Some(Self::Open),
            _ => None,
        }
    }
}

/// The plan's choices as the window keeps them: `None` is one left open, for
/// the computer to answer against its real hardware.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct Choices {
    pub os: Option<Os>,
    /// The computer asks its own name when it installs.
    pub name_open: bool,
    pub disk: Option<Disks>,
    pub network: Option<Networks>,
    pub unlock: Option<Unlock>,
    pub updates: Option<Updates>,
    pub hosting: Option<Hosting>,
    pub delivery: Delivery,
}

impl Default for Choices {
    /// What `tilo machine add` does when it is told nothing: the useful
    /// choices are made, and what only the computer can answer stays open.
    fn default() -> Self {
        Self {
            os: Some(Os::TiloOs),
            name_open: false,
            disk: Some(Disks::Largest),
            network: Some(Networks::Automatic),
            unlock: Some(Unlock::Hardware),
            updates: Some(Updates::Stable),
            hosting: Some(Hosting::Open),
            delivery: Delivery::Usb,
        }
    }
}

impl Choices {
    /// The OS the plan installs, Tilo OS unless the person chose Tilo Server.
    pub(super) fn os(&self) -> Os {
        self.os.unwrap_or(Os::TiloOs)
    }

    /// The choices this plan leaves open, in [`plan::OPEN_CHOICES`] order.
    pub(super) fn open(&self) -> Vec<&'static str> {
        let mut open = Vec::new();
        if self.name_open {
            open.push("name");
        }
        if self.network.is_none() {
            open.push("network");
        }
        if self.disk.is_none() {
            open.push("disk");
        }
        if self.unlock.is_none() {
            open.push("unlock");
        }
        if self.updates.is_none() {
            open.push("updates");
        }
        if self.os() == Os::TiloServer && !matches!(self.hosting, Some(Hosting::HoldsNone)) {
            open.push("hosting");
        }
        open
    }

    /// The plan document's choices, with `name` as the computer name the plan
    /// carries when the plan is not leaving it open.
    pub(super) fn install(&self, name: &str) -> install_plan::Choices {
        install_plan::Choices {
            destination: self.os().destination().to_owned(),
            name: (!self.name_open)
                .then(|| name.trim().to_owned())
                .filter(|name| !name.is_empty()),
            network: self.network.map(|_| Network {
                addressing: "dhcp".to_owned(),
                ..Network::default()
            }),
            disk: self.disk.map(Disks::rule),
            unlock: self.unlock.map(|unlock| unlock.key().to_owned()),
            updates: self.updates.map(|updates| updates.key().to_owned()),
            hosting: if self.os() == Os::TiloServer {
                match self.hosting {
                    Some(Hosting::HoldsNone) => Some(Vec::new()),
                    Some(Hosting::Open) | None => None,
                }
            } else {
                Some(Vec::new())
            },
        }
    }
}

/// A plan this computer made, and what carries it to its computer.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct Created {
    pub plan: Plan,
    pub token_file: PathBuf,
    pub commands: plan::Commands,
}

/// What the plan still asks, and what answers it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct Question {
    pub choice: &'static str,
    pub text: String,
}

/// A computer this person planned, as their devices keep it: the plan, what
/// it asks, and what carries it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct Waiting {
    pub name: String,
    pub destination: String,
    pub plan: String,
    /// The drive the plan picks among the drives the computer reported, when
    /// it has reported any.
    pub disk_answer: Option<String>,
    pub questions: Vec<Question>,
    pub expires_in_hours: u64,
    pub commands: plan::Commands,
}

impl Waiting {
    pub(super) fn title(&self) -> String {
        if self.name.is_empty() {
            crate::setup::strings::t("installer-plan-waiting-unnamed")
        } else {
            self.name.clone()
        }
    }
}

/// Make the plan `choices` describes for the computer named `name`, and name
/// the file its single-use ticket goes to, which only this account reads. The
/// plan itself is published into the person's Servers workspace through the
/// running session ([`publish`]), so every one of their devices reads it there.
///
/// # Errors
///
/// This computer holds no identity, or the plan is refused.
#[cfg(target_os = "linux")]
pub(super) fn prepare(
    choices: &Choices,
    name: &str,
    installer_url: &str,
) -> Result<Created, String> {
    let local = crate::handoff::local_identity_from_env()?;
    let named = install_plan::token()?;
    let id = install_plan::new_id();
    let plan = choices.install(name).plan(&id, named)?;
    let token_file = install_plan::plans_dir(local.identity_dir).join(format!("{id}.token"));
    let commands = plan::commands(
        &plan,
        installer_url.trim_end_matches('/'),
        &token_file.display().to_string(),
        std::env::consts::ARCH,
    );
    Ok(Created {
        plan,
        token_file,
        commands,
    })
}

/// Publish `created`'s plan into the person's Servers workspace, through the
/// session that holds it: the request the window follows until it answered.
pub(super) fn publish(created: &Created, installer_url: &str) -> InvocationCall {
    let plan = &created.plan;
    let label = if plan.name.is_empty() {
        plan.destination.as_str()
    } else {
        plan.name.as_str()
    };
    let mut arguments = serde_json::Map::new();
    arguments.insert(
        "plan".into(),
        serde_json::to_string(plan).unwrap_or_default().into(),
    );
    arguments.insert("label".into(), label.into());
    arguments.insert(
        "token_file".into(),
        created.token_file.display().to_string().into(),
    );
    arguments.insert("installer_url".into(), installer_url.into());
    tilo_ui_base::RemoteActions::current().invoke(
        machines::target(machines::PLAN),
        InvokeArgument::Json(arguments),
        None,
    )
}

/// The computers this person planned and has not installed, asked of the
/// session that holds their Servers workspace: the request the window follows.
pub(super) fn waiting() -> InvocationCall {
    tilo_ui_base::RemoteActions::current().invoke(
        machines::target(machines::LIST),
        InvokeArgument::None,
        None,
    )
}

/// What a followed plan-making answered, once it did: the plan is in the
/// person's workspace.
pub(super) fn planned(call: &InvocationCall) -> Option<Result<(), String>> {
    Some(answered(call)?.map(drop))
}

/// The computers a followed waiting read answered, once it did, each with what
/// it still asks answered against the hardware it reported.
pub(super) fn listed(
    call: &InvocationCall,
    installer_url: &str,
) -> Option<Result<Vec<Waiting>, String>> {
    let value = match answered(call)? {
        Ok(value) => value,
        Err(error) => return Some(Err(error)),
    };
    let planned: Vec<install_profile_model::plan::WaitingPlan> = match serde_json::from_value(value)
    {
        Ok(planned) => planned,
        Err(error) => return Some(Err(format!("the session's plans do not read: {error}"))),
    };
    let token_dir = crate::handoff::local_identity_from_env().map_or_else(
        |_| PathBuf::new(),
        |local| install_plan::plans_dir(local.identity_dir),
    );
    Some(Ok(planned
        .into_iter()
        .map(|planned| {
            let plan = planned.plan;
            let token_file = token_dir.join(format!("{}.token", plan.plan));
            let commands = plan::commands(
                &plan,
                installer_url.trim_end_matches('/'),
                &token_file.display().to_string(),
                std::env::consts::ARCH,
            );
            Waiting {
                name: plan.name.clone(),
                destination: destination_label(&plan.destination),
                plan: plan.plan.clone(),
                disk_answer: disk_answer(&plan, &planned.drives),
                questions: questions(&plan, &planned.drives),
                expires_in_hours: hours_left(plan.token.expires_at_ns).unwrap_or(0),
                commands,
            }
        })
        .collect()))
}

/// What a followed call answered, once it stopped changing: its value, or why
/// it failed. `None` while it is still on its way.
pub(super) fn answered(call: &InvocationCall) -> Option<Result<serde_json::Value, String>> {
    match call.get() {
        tilo_ui_base::Arrival::Pending { .. } => None,
        tilo_ui_base::Arrival::Failed(error) => Some(Err(error.to_string())),
        tilo_ui_base::Arrival::Ready(update) => Some(match update {
            ui::tilo_actions::InvocationUpdate::Succeeded { output, .. } => {
                Ok(output.value.unwrap_or(serde_json::Value::Null))
            }
            ui::tilo_actions::InvocationUpdate::Failed { error, .. } => Err(error.message),
            ui::tilo_actions::InvocationUpdate::Cancelled { message, .. } => {
                Err(message.unwrap_or_else(|| "the session stopped making the plan".to_owned()))
            }
            ui::tilo_actions::InvocationUpdate::Accepted { .. }
            | ui::tilo_actions::InvocationUpdate::Progress { .. }
            | ui::tilo_actions::InvocationUpdate::Item { .. } => {
                return None;
            }
        }),
    }
}

/// What `plan` still asks, each with the hardware fact that answers it:
/// `drives` are the ones the computer reported, empty until it has.
pub(super) fn questions(plan: &Plan, drives: &[plan::Drive]) -> Vec<Question> {
    let mut questions = Vec::new();
    for choice in plan.open_choices() {
        let text = match choice {
            "name" => crate::setup::strings::t("installer-plan-answer-name"),
            "network" => crate::setup::strings::t("installer-plan-answer-network"),
            "disk" if drives.is_empty() => crate::setup::strings::t("installer-plan-answer-disk"),
            "disk" => match plan::choose(&plan.disk, drives) {
                plan::Choice::Drive(drive) => {
                    let size = tilo_ui_base::format::size(drive.bytes);
                    crate::setup::strings::t_args(
                        "installer-plan-answer-disk-drive",
                        &[
                            ("model", drive.model.as_str().into()),
                            ("size", (&size).into()),
                        ],
                    )
                }
                plan::Choice::Open { reason, .. } => crate::setup::strings::t_args(
                    "installer-plan-answer-disk-open",
                    &[("reason", reason.as_str().into())],
                ),
            },

            "unlock" => crate::setup::strings::t("installer-plan-answer-unlock"),
            "updates" => crate::setup::strings::t("installer-plan-answer-updates"),
            "hosting" => crate::setup::strings::t("installer-plan-answer-hosting"),
            _ => crate::setup::strings::t("installer-plan-answer-other"),
        };
        questions.push(Question { choice, text });
    }
    questions
}

/// What `plan` will take on a computer that reported `hardware`: the drive its
/// disk rule picks, or why the question stays open. `None` when the plan made
/// its disk choice and no report answers it.
pub(super) fn disk_answer(plan: &Plan, drives: &[plan::Drive]) -> Option<String> {
    if plan.disk.kind.is_empty() || drives.is_empty() {
        return None;
    }
    Some(match plan::choose(&plan.disk, drives) {
        plan::Choice::Drive(drive) => {
            let size = tilo_ui_base::format::size(drive.bytes);
            crate::setup::strings::t_args(
                "installer-plan-answer-disk-drive",
                &[
                    ("model", drive.model.as_str().into()),
                    ("size", (&size).into()),
                ],
            )
        }
        plan::Choice::Open { reason, .. } => crate::setup::strings::t_args(
            "installer-plan-answer-disk-open",
            &[("reason", reason.as_str().into())],
        ),
    })
}

fn destination_label(destination: &str) -> String {
    if destination == "tilo-server" {
        crate::setup::strings::t("installer-preview-server-title")
    } else {
        crate::setup::strings::t("installer-preview-os-title")
    }
}

/// The file a plan's token is kept in on the other computer, as that computer's own home
/// directory names it: an example, naming no path this one has.
fn example_token_file(plan: &str) -> PathBuf {
    Path::new("/home/ada")
        .join([".local", "state", "tilo", "identity", "install-plans"].join("/"))
        .join(format!("{plan}.token"))
}

/// The example plan the screenshot scenarios show; it is never kept and its
/// token names nothing.
pub(super) fn example() -> Created {
    let plan = example_plan("0123456789abcdef0123456789abcdef");
    Created {
        token_file: example_token_file(&plan.plan),
        commands: commands_for(
            &plan,
            &example_token_file("0123456789abcdef0123456789abcdef")
                .display()
                .to_string(),
        ),
        plan,
    }
}

/// The example computers waiting, for the screenshot scenarios.
pub(super) fn examples() -> Vec<Waiting> {
    let cupboard = example_plan("0123456789abcdef0123456789abcdef");
    let spare = install_plan::Choices {
        destination: "tilo-os".to_owned(),
        name: None,
        network: None,
        disk: Some(DiskRule {
            kind: "largest".to_owned(),
            identity: String::new(),
        }),
        unlock: None,
        updates: Some("stable".to_owned()),
        hosting: Some(Vec::new()),
    }
    .plan(
        "fedcba9876543210fedcba9876543210",
        plan::Token {
            nonce: "ffeeddccbbaa99887766554433221100".to_owned(),
            expires_at_ns: ui::time::unix_now_ns() + 9 * 60 * 60 * 1_000_000_000,
        },
    )
    .expect("the example plan is one a machine reads");
    let drives = vec![
        plan::Drive {
            model: "Samsung SSD 990".to_owned(),
            identity: "S6XXNJ0T".to_owned(),
            bytes: 2_000_398_934_016,
            holds_data: true,
            boots: true,
        },
        plan::Drive {
            model: "WDC WD40EFZX".to_owned(),
            identity: "WD-WX12D80L".to_owned(),
            bytes: 4_000_787_030_016,
            holds_data: false,
            boots: false,
        },
    ];
    [cupboard, spare]
        .into_iter()
        .map(|plan| Waiting {
            name: plan.name.clone(),
            destination: destination_label(&plan.destination),
            plan: plan.plan.clone(),
            disk_answer: disk_answer(&plan, &drives),
            questions: questions(&plan, &drives),
            expires_in_hours: 18,
            commands: commands_for(
                &plan,
                &example_token_file("0123456789abcdef0123456789abcdef")
                    .display()
                    .to_string(),
            ),
        })
        .collect()
}

fn example_plan(id: &str) -> Plan {
    let expires_at_ns = ui::time::unix_now_ns() + 18 * 60 * 60 * 1_000_000_000;
    install_plan::Choices {
        destination: "tilo-server".to_owned(),
        name: Some("cupboard".to_owned()),
        network: Some(Network {
            addressing: "dhcp".to_owned(),
            ..Network::default()
        }),
        disk: Some(DiskRule {
            kind: "largest".to_owned(),
            identity: String::new(),
        }),
        unlock: Some("tpm".to_owned()),
        updates: Some("stable".to_owned()),
        hosting: Some(Vec::new()),
    }
    .plan(
        id,
        plan::Token {
            nonce: "00112233445566778899aabbccddeeff".to_owned(),
            expires_at_ns,
        },
    )
    .expect("the example plan is one a machine reads")
}

fn commands_for(plan: &Plan, token_file: &str) -> plan::Commands {
    plan::commands(
        plan,
        install_plan::INSTALLER_URL,
        token_file,
        std::env::consts::ARCH,
    )
}

/// Whole hours until `expires_at_ns`, from now.
pub(super) fn hours_left(expires_at_ns: u64) -> Option<u64> {
    let left = expires_at_ns.checked_sub(ui::time::unix_now_ns())?;
    Some(left / (60 * 60 * 1_000_000_000))
}

/// The plan an "on another computer" preparation makes, followed from the
/// window's own passes: the request that writes it, and the list of the
/// computers this person planned.
impl super::InstallerPreview {
    /// Make the plan the review describes, or answer `false` while the one
    /// made here is still being written into the person's Servers workspace:
    /// the window follows it ([`Self::poll_plan`]) and continues once it
    /// answered.
    pub(super) fn make_plan(&mut self) -> Result<bool, &'static str> {
        if self.machine.created.is_some() {
            return Ok(true);
        }
        if self.machine.making.is_none() {
            #[cfg(target_os = "linux")]
            {
                if let Ok(created) = prepare(
                    &self.journey.plan,
                    &self.journey.device_name,
                    install_plan::INSTALLER_URL,
                ) {
                    let call = publish(&created, install_plan::INSTALLER_URL);
                    self.machine.making = Some((created, call));
                    self.machine.plan_continue = true;
                    self.machine.error = None;
                } else {
                    self.machine.error = Some("installer-plan-unavailable");
                    return Err("installer-plan-unavailable");
                }
            }
            #[cfg(not(target_os = "linux"))]
            {
                self.machine.error = Some("installer-plan-unavailable");
                return Err("installer-plan-unavailable");
            }
        }
        Ok(self.machine.created.is_some())
    }

    /// Follow the plan being written and the list of waiting computers once
    /// each, continuing the review that waited for the plan.
    pub(super) fn machine_status(&self) -> serde_json::Value {
        let created = self.machine.created.as_ref().map(|created| {
            serde_json::json!({
                "plan": created.plan.plan,
                "destination": created.plan.destination,
                "open": created.plan.open_choices(),
                "token_file": created.token_file.display().to_string(),
                "expires_at_ns": created.plan.token.expires_at_ns,
                "medium": created.commands.medium,
                "netboot": created.commands.netboot,
                "alongside": created.commands.alongside,
            })
        });
        serde_json::json!({
            "created": created,
            "error": self.machine.error,
            "waiting": self.machine.waiting.iter().map(|waiting| serde_json::json!({
                "plan": waiting.plan,
                "name": waiting.name,
                "destination": waiting.destination,
                "expires_in_hours": waiting.expires_in_hours,
                "questions": waiting.questions.iter()
                    .map(|question| serde_json::json!({
                        "choice": question.choice,
                        "text": question.text,
                    }))
                    .collect::<Vec<_>>(),
            })).collect::<Vec<_>>(),
        })
    }

    pub(super) fn poll_plan(&mut self) -> bool {
        let mut changed = false;
        if let Some((created, call)) = self.machine.making.take() {
            match planned(&call) {
                Some(Ok(())) => {
                    self.machine.created = Some(created);
                    self.machine.error = None;
                    if std::mem::take(&mut self.machine.plan_continue)
                        && let Err(error) = self.apply(super::InstallerPreviewEffect::PrepareUsb)
                    {
                        self.error = Some(error);
                    }
                    changed = true;
                }
                Some(Err(_)) => {
                    self.machine.error = Some("installer-plan-unavailable");
                    changed = true;
                }
                None => self.machine.making = Some((created, call)),
            }
        }
        if let Some(call) = self.machine.listing.take() {
            match listed(&call, install_plan::INSTALLER_URL) {
                Some(Ok(waiting)) => {
                    self.machine.waiting = waiting;
                    changed = true;
                }
                Some(Err(_)) => changed = true,
                None => self.machine.listing = Some(call),
            }
        }
        changed
    }
}
