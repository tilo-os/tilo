//! What a host offers a person's account, and why it refuses an action.
//!
//! Every place a workspace runs is a host: the person's own devices, hosts
//! they run themselves, and hosting providers that sell compute. Hosting is
//! not a separate dashboard: a person's account with a host is what this
//! module describes, and a limit is the host accepting or refusing an action.
//! Billing hangs off that acceptance, never off an app.
//!
//! A host answers [`ACCOUNT`] under [`PROVIDER`] with a [`HostAccount`]:
//!
//! - the plans it offers ([`PlanOffer`]); a host that sells nothing offers
//!   none and answers no [`Standing`];
//! - the account's [`Standing`]: its plan, whether a payment method is on
//!   file, what it is entitled to, its usage (Linux VM seconds and bytes
//!   stored per workspace) and its limits, and its invoices;
//! - where the person manages billing ([`HostAccount::manage_billing`]): the
//!   provider's own page, never a form Tilo draws.
//!
//! The read is live: a host sends a new answer whenever the account changes
//! (a card added, a plan changed, usage recorded), so a view or a waiting
//! command follows it without asking again.
//!
//! When the host refuses an action that needs what the account lacks, it
//! says so with a [`Refusal`]: a typed [`Reason`], the words for whoever ran
//! it, and the [`NextStep`]s that would let it through, each with the
//! provider's URL. The first action that needs a Linux machine on a host
//! that sells them, with no card on file, is refused with
//! [`Reason::NoPaymentMethod`] and [`NextStepKind::AddPaymentMethod`]:
//! nothing asks for a card before that.
//!
//! Every type here is written field for field, with enums tagged by their
//! variant, so it crosses both JSON (the action door) and postcard (a
//! host's own sockets) as the same value.
//!
//! Payment itself (checkout, prices, webhooks, credit, rate limits, fraud
//! checks) is the provider's. All it does here is answer this contract:
//! write the account's standing where its host reads it, and name its pages.
//! The account is the person's: their Tilo identity carries it to every
//! device, so Settings and the overview of their Tilo show the same standing.

use serde::{Deserialize, Serialize};

use super::{ActionAddress, RemoteActionTarget};

/// The provider a host answers its accounts under, the one it answers its
/// other hosting steps under.
pub const PROVIDER: &str = super::home_relay_hosting::PROVIDER;
/// The resource the account's actions are actions of.
pub const RESOURCE: &str = "host.account";
/// Live: the calling person's account with this host, as a [`HostAccount`].
pub const ACCOUNT: &str = "account";

/// The target of action `action` of [`RESOURCE`].
#[must_use]
pub fn target(action: &str) -> RemoteActionTarget {
    RemoteActionTarget {
        provider: PROVIDER.to_owned(),
        address: ActionAddress::new(RESOURCE, action),
    }
}

/// What kind of place a host is, as the person thinks of it.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum HostKind {
    /// One of the person's own devices.
    #[default]
    Device,
    /// A host the person runs themselves: it sells nothing.
    SelfHosted,
    /// A hosting provider, such as Tilo Cloud.
    Provider,
}

/// The limits of a plan, or of an account on it.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct Limits {
    /// Linux VMs of the account running at once.
    pub max_running: u32,
    /// Linux VMs (with their disks) the account keeps, running or not.
    pub max_vms: u32,
    /// The VM sizes it may use, by name (`small`, `default`, `large`,
    /// `xlarge`).
    pub sizes: Vec<String>,
    /// VM seconds included per billing period; `None` when the plan bills
    /// every second or sets no such bound.
    pub included_vm_seconds: Option<u64>,
    /// Bytes the account's workspaces may keep on the host together; `None`
    /// when the host sets no such bound.
    pub storage_bytes: Option<u64>,
    /// Workspaces of the account's own the host keeps; `None` when the host
    /// sets no such bound.
    pub max_workspaces: Option<u32>,
}

/// One plan a host offers.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct PlanOffer {
    /// What [`Standing::plan`] names it by.
    pub id: String,
    /// Its name, in the provider's words ("Pro").
    pub name: String,
    /// Its price, in the provider's words ("$20 a month, then $0.04 per VM
    /// hour"); `None` for a plan without one.
    pub price: Option<String>,
    pub limits: Limits,
}

/// Something an account may do once its host says so.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Entitlement {
    /// Run Linux programs in a VM of a workspace's own: a non-Tilo app, or
    /// a shell command Tilo does not implement itself.
    LinuxVms,
}

/// How long one workspace's VMs ran in the period.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct WorkspaceUsage {
    /// The workspace's id.
    pub workspace: String,
    /// Seconds its VMs ran.
    pub vm_seconds: u64,
    /// Seconds its suspended VMs held a snapshot.
    pub snapshot_seconds: u64,
    /// Whether one of its VMs runs now.
    pub running: bool,
    /// Bytes the host keeps of the workspace now: its history and state.
    pub stored_bytes: u64,
}

/// What the account used in the current billing period.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct Usage {
    /// When the period started and ends, Unix milliseconds.
    pub period_start_ms: u64,
    pub period_end_ms: u64,
    /// Per workspace, most used first.
    pub workspaces: Vec<WorkspaceUsage>,
}

impl Usage {
    /// Seconds every workspace's VMs ran.
    #[must_use]
    pub fn vm_seconds(&self) -> u64 {
        self.workspaces
            .iter()
            .fold(0_u64, |sum, usage| sum.saturating_add(usage.vm_seconds))
    }

    /// Bytes every workspace keeps on the host now.
    #[must_use]
    pub fn stored_bytes(&self) -> u64 {
        self.workspaces
            .iter()
            .fold(0_u64, |sum, usage| sum.saturating_add(usage.stored_bytes))
    }

    /// The usage of `workspace`, if its VMs ran or it keeps anything.
    #[must_use]
    pub fn of(&self, workspace: &str) -> Option<&WorkspaceUsage> {
        self.workspaces
            .iter()
            .find(|usage| usage.workspace == workspace)
    }
}

/// Where an invoice stands.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum InvoiceStatus {
    Paid,
    /// Issued and not paid yet.
    #[default]
    Open,
    /// Payment failed; the provider's page says what to do.
    PastDue,
    Void,
}

/// One invoice, as the provider issued it.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct Invoice {
    pub id: String,
    /// When it was issued, Unix milliseconds.
    pub issued_ms: u64,
    /// Its total, in the provider's words ("$24.10").
    pub amount: String,
    pub status: InvoiceStatus,
    /// The provider's page for it.
    pub url: String,
}

/// A payment method on file, as the provider describes it: enough for the
/// person to recognise it ("Visa ·· 4242"), never enough to charge it.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct PaymentMethod {
    /// Its brand, in the provider's words ("Visa"); `None` when the
    /// provider does not say.
    pub brand: Option<String>,
    /// The last four digits of its number; `None` when the provider does
    /// not say.
    pub last4: Option<String>,
}

/// The account's standing with a host that sells something.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct Standing {
    /// The account's id with the host.
    pub account: String,
    /// The [`PlanOffer::id`] it is on; `None` before it chose one.
    pub plan: Option<String>,
    /// The payment method on file; `None` while there is none.
    pub payment_method: Option<PaymentMethod>,
    pub entitlements: Vec<Entitlement>,
    /// What the account may use now; `None` while it is entitled to nothing.
    pub limits: Option<Limits>,
    pub usage: Usage,
    /// Credit the account has left, as the provider writes the amount
    /// ("$12.40"); `None` when it has none.
    pub credit: Option<String>,
    /// Newest first.
    pub invoices: Vec<Invoice>,
}

impl Standing {
    #[must_use]
    pub fn entitled(&self, entitlement: Entitlement) -> bool {
        self.entitlements.contains(&entitlement)
    }
}

/// A person's account with one host.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct HostAccount {
    /// The host's name, as the person knows it ("Tilo Cloud", "Ada's
    /// laptop").
    pub host: String,
    pub kind: HostKind,
    /// The plans it offers; none when it sells nothing.
    pub plans: Vec<PlanOffer>,
    /// The account's standing; `None` on a host that sells nothing.
    pub standing: Option<Standing>,
    /// The provider's page where the person manages billing: payment
    /// methods, plan and invoices.
    pub manage_billing: Option<String>,
}

impl HostAccount {
    /// A host named `host` that sells nothing: its programs run as its
    /// person's own.
    #[must_use]
    pub fn sells_nothing(host: impl Into<String>, kind: HostKind) -> Self {
        Self {
            host: host.into(),
            kind,
            ..Self::default()
        }
    }

    /// Whether the host sells anything.
    #[must_use]
    pub fn sells(&self) -> bool {
        !self.plans.is_empty()
    }

    /// The plan the account is on.
    #[must_use]
    pub fn plan(&self) -> Option<&PlanOffer> {
        let id = self.standing.as_ref()?.plan.as_deref()?;
        self.plans.iter().find(|plan| plan.id == id)
    }
}

/// What would let a refused action through.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum NextStepKind {
    /// Add a payment method on the provider's page.
    AddPaymentMethod,
    /// Raise the account's limits: another plan, or a higher bound.
    RaiseLimit,
    /// Choose a VM size the plan includes for the workspace.
    ChooseSize,
    /// Wait for one of the account's VMs to stop on its own, or stop one.
    WaitForIdle,
    /// Use a workspace the account already has.
    UseExisting,
}

/// One way through a refusal.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct NextStep {
    pub kind: NextStepKind,
    /// The provider's page for it; `None` for a step taken in Tilo itself.
    #[serde(default)]
    pub url: Option<String>,
}

/// Why a host refused an action.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Reason {
    /// The action needs a Linux machine, the host sells them, and the
    /// account has no payment method on file.
    NoPaymentMethod,
    /// The workspace's VM size is not one the plan includes.
    SizeNotInPlan { size: String, allowed: Vec<String> },
    /// The account already runs as many VMs at once as its plan allows.
    TooManyRunning {
        limit: u32,
        /// The workspaces whose VMs run now.
        running: Vec<String>,
    },
    /// The account already keeps as many VMs as its plan allows.
    TooManyVms { limit: u32, existing: u32 },
    /// The account's workspaces keep as many bytes on the host as it allows:
    /// the write was not kept.
    StorageFull { limit_bytes: u64, used_bytes: u64 },
    /// The account already has as many workspaces of its own on the host as
    /// it allows.
    TooManyWorkspaces {
        limit: u32,
        /// The ids of the workspaces it has.
        existing: Vec<String>,
    },
}

/// A host's refusal of an action, for whoever ran it.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Refusal {
    pub reason: Reason,
    /// What happened and what to do, in plain words for whoever ran the
    /// action, a person or an agent.
    pub message: String,
    /// What would let it through, the likeliest first.
    #[serde(default)]
    pub next: Vec<NextStep>,
}

impl Refusal {
    /// The step of `kind`, if the refusal offers it.
    #[must_use]
    pub fn step(&self, kind: NextStepKind) -> Option<&NextStep> {
        self.next.iter().find(|step| step.kind == kind)
    }

    /// Whether the account's standing changing on the host is what would let
    /// the action through, so whoever ran it can wait for that rather than
    /// run it again.
    #[must_use]
    pub const fn awaits_the_account(&self) -> bool {
        matches!(self.reason, Reason::NoPaymentMethod)
    }

    /// The refusal as an action's refusal reason
    /// ([`super::ActionHostError::Refused`]): its JSON, which
    /// [`Self::from_reason`] reads back typed, and which a caller that does
    /// not read it still shows as plain data.
    #[must_use]
    pub fn to_reason(&self) -> String {
        serde_json::to_string(self).unwrap_or_else(|_| self.message.clone())
    }

    /// The refusal an action's refusal reason carries, when it carries one
    /// ([`Self::to_reason`]).
    #[must_use]
    pub fn from_reason(reason: &str) -> Option<Self> {
        serde_json::from_str(reason).ok()
    }
}

impl std::fmt::Display for Refusal {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.message)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_host_that_sells_nothing_answers_no_plans_and_no_standing() {
        let host = HostAccount::sells_nothing("Ada's server", HostKind::SelfHosted);
        assert!(!host.sells());
        assert!(host.plan().is_none());
        let json = serde_json::to_value(&host).unwrap();
        assert_eq!(
            json,
            serde_json::json!({
                "host": "Ada's server",
                "kind": "self-hosted",
                "plans": [],
                "standing": null,
                "manage_billing": null
            })
        );
        assert_eq!(serde_json::from_value::<HostAccount>(json).unwrap(), host);
    }

    #[test]
    fn a_refusal_names_its_reason_and_next_steps_on_the_wire() {
        let refusal = Refusal {
            reason: Reason::NoPaymentMethod,
            message: "This needs a Linux machine.".into(),
            next: vec![NextStep {
                kind: NextStepKind::AddPaymentMethod,
                url: Some("https://billing.example/card".into()),
            }],
        };
        let json = serde_json::to_value(&refusal).unwrap();
        assert_eq!(
            json,
            serde_json::json!({
                "reason": "no_payment_method",
                "message": "This needs a Linux machine.",
                "next": [{"kind": "add_payment_method", "url": "https://billing.example/card"}]
            })
        );
        assert!(refusal.awaits_the_account());
        let bytes = postcard::to_allocvec(&refusal).unwrap();
        assert_eq!(postcard::from_bytes::<Refusal>(&bytes).unwrap(), refusal);
        assert_eq!(
            refusal
                .step(NextStepKind::AddPaymentMethod)
                .and_then(|step| step.url.as_deref()),
            Some("https://billing.example/card")
        );
        let limit = Refusal {
            reason: Reason::TooManyRunning {
                limit: 1,
                running: vec!["ws1".into()],
            },
            message: String::new(),
            next: Vec::new(),
        };
        assert!(!limit.awaits_the_account());
        assert_eq!(
            serde_json::to_value(&limit.reason).unwrap(),
            serde_json::json!({"too_many_running": {"limit": 1, "running": ["ws1"]}})
        );
    }

    #[test]
    fn storage_refusals_cross_an_actions_reason_typed() {
        let full = Refusal {
            reason: Reason::StorageFull {
                limit_bytes: 10,
                used_bytes: 12,
            },
            message: "Your workspace is full.".into(),
            next: vec![NextStep {
                kind: NextStepKind::RaiseLimit,
                url: None,
            }],
        };
        assert_eq!(
            serde_json::to_value(&full.reason).unwrap(),
            serde_json::json!({"storage_full": {"limit_bytes": 10, "used_bytes": 12}})
        );
        assert_eq!(Refusal::from_reason(&full.to_reason()), Some(full.clone()));
        assert!(!full.awaits_the_account());
        assert_eq!(Refusal::from_reason("the host refused"), None);
        let second = Reason::TooManyWorkspaces {
            limit: 1,
            existing: vec!["w".into()],
        };
        let bytes = postcard::to_allocvec(&second).unwrap();
        assert_eq!(postcard::from_bytes::<Reason>(&bytes).unwrap(), second);
        let usage = Usage {
            workspaces: vec![
                WorkspaceUsage {
                    workspace: "a".into(),
                    stored_bytes: 7,
                    ..WorkspaceUsage::default()
                },
                WorkspaceUsage {
                    workspace: "b".into(),
                    stored_bytes: 5,
                    ..WorkspaceUsage::default()
                },
            ],
            ..Usage::default()
        };
        assert_eq!(usage.stored_bytes(), 12);
    }

    #[test]
    fn the_plan_an_account_is_on_is_one_of_the_offers() {
        let host = HostAccount {
            host: "Tilo Cloud".into(),
            kind: HostKind::Provider,
            plans: vec![PlanOffer {
                id: "pro".into(),
                name: "Pro".into(),
                ..PlanOffer::default()
            }],
            standing: Some(Standing {
                plan: Some("pro".into()),
                usage: Usage {
                    workspaces: vec![
                        WorkspaceUsage {
                            workspace: "a".into(),
                            vm_seconds: 60,
                            ..WorkspaceUsage::default()
                        },
                        WorkspaceUsage {
                            workspace: "b".into(),
                            vm_seconds: 30,
                            ..WorkspaceUsage::default()
                        },
                    ],
                    ..Usage::default()
                },
                ..Standing::default()
            }),
            manage_billing: Some("https://billing.example".into()),
        };
        assert!(host.sells());
        let bytes = postcard::to_allocvec(&host).unwrap();
        assert_eq!(postcard::from_bytes::<HostAccount>(&bytes).unwrap(), host);
        assert_eq!(host.plan().map(|plan| plan.name.as_str()), Some("Pro"));
        let usage = &host.standing.as_ref().unwrap().usage;
        assert_eq!(usage.vm_seconds(), 90);
        assert_eq!(usage.of("b").map(|usage| usage.vm_seconds), Some(30));
    }
}
