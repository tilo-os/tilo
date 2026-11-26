//! The person's Tilo as the device publishes it: their workspaces this session does not hold,
//! which their host offers to go to, and the hosts running the workspaces they do hold — the plan
//! on the account, what its Linux machines used, the credit it has left and the page that manages
//! it.
//!
//! The person's own workspaces are the layout's (`session.layout`), and the agents they let in are
//! their keys (`device.devices`): this reading carries neither, so a page reads each fact from the
//! one place that publishes it. Neither does it carry invoices: billing is Settings' page, and the
//! overview says the plan, the usage and the credit, each its own fact.
//!
//! Hosting is not a dashboard of its own. A host is a place a workspace runs; what a host offers
//! the person's account, and what it used, is its answer to the session's account read. The page
//! shows that answer only once it means something ([`Host::matters`]); someone with one computer
//! that sells nothing sees no host at all.

use serde::{Deserialize, Serialize};

/// The person's Tilo as a view of another language reads it (`device.yours`).
#[derive(
    Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize, tilo_resource::JsonSchema,
)]
pub struct Yours {
    /// The person's workspaces this session does not hold, as the host offers them, in the
    /// host's order.
    pub elsewhere: Vec<Elsewhere>,
    /// The hosts running the person's workspaces; empty when hosting means nothing to show.
    pub hosts: Vec<Host>,
}

/// One of the person's workspaces elsewhere, which this device does not hold.
#[derive(
    Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize, tilo_resource::JsonSchema,
)]
pub struct Elsewhere {
    /// The host's action that goes there, as its offers carry it. Never named `id`: a record's
    /// `id` is the address it reads as a resource, and this is an action's name.
    pub action: String,
    /// What the workspace is called, as its host names it.
    pub name: String,
}

/// The account with one host running the person's workspaces.
#[derive(
    Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize, tilo_resource::JsonSchema,
)]
#[serde(default)]
pub struct Host {
    /// The host's name, as the person knows it.
    pub server: String,
    /// What it is: `device` for this computer, `self-hosted` for one of the person's own servers,
    /// or `provider` for a host that sells; empty when the host did not answer.
    pub kind: String,
    /// Where the account stands: `reading`, `available`, `signed-out`, `no-account` or
    /// `unreachable`.
    pub state: String,
    /// What to say when the account is not available, in the host's or this device's words.
    pub words: String,
    /// The plan the account is on, in the provider's words; empty before it chose one.
    pub plan_name: String,
    /// What the plan costs, in the provider's words; empty for a plan without a price.
    pub plan_price: String,
    /// Whether a payment method is on file.
    pub has_card: bool,
    /// Seconds the account's Linux machines ran in the billing period.
    pub usage_seconds: i64,
    /// Seconds the plan includes in the period; none when it bills every second.
    pub included_seconds: Option<i64>,
    /// The credit the account has left, as the provider writes the amount; empty when none.
    pub credit: String,
    /// The provider's page where the person manages a card, a plan and the invoices; empty when
    /// none.
    pub manage: String,
}

impl Host {
    /// Whether hosting means anything to the person yet: a host that is not this computer, a plan,
    /// a card, usage, credit, a host nobody signed in to, or one that could not be reached. A
    /// computer that runs its own workspaces and sells nothing earns no row.
    #[must_use]
    pub fn matters(&self) -> bool {
        self.kind != "device"
            || self.has_card
            || self.usage_seconds > 0
            || !self.plan_name.is_empty()
            || !self.credit.is_empty()
            || self.state == "signed-out"
            || self.state == "unreachable"
    }
}

/// The person's Tilo as a view of another language reads it (`device.yours`).
#[must_use]
pub fn surface() -> tilo_resource::surface::Surface {
    tilo_resource::surface::reading_surface::<Yours, tilo_resource::NoEffect>("yours", "Your Tilo")
}

#[cfg(test)]
mod tests {
    use super::*;

    /// This computer, running its own workspaces and selling nothing, has no hosting to show; a
    /// server of the person's own, a plan, a card, usage, credit and a host that could not be
    /// reached each do.
    #[test]
    fn hosting_shows_once_it_means_something() {
        assert!(
            !Host {
                kind: "device".to_owned(),
                state: "no-account".to_owned(),
                ..Host::default()
            }
            .matters(),
            "one computer that sells nothing shows no hosting"
        );
        let matters = [
            Host {
                kind: "self-hosted".to_owned(),
                ..Host::default()
            },
            Host {
                kind: "device".to_owned(),
                plan_name: "Pro".to_owned(),
                ..Host::default()
            },
            Host {
                kind: "device".to_owned(),
                has_card: true,
                ..Host::default()
            },
            Host {
                kind: "device".to_owned(),
                usage_seconds: 60,
                ..Host::default()
            },
            Host {
                kind: "device".to_owned(),
                credit: "$12.40".to_owned(),
                ..Host::default()
            },
            Host {
                kind: "device".to_owned(),
                state: "signed-out".to_owned(),
                ..Host::default()
            },
            Host {
                kind: String::new(),
                state: "unreachable".to_owned(),
                ..Host::default()
            },
        ];
        for host in matters {
            assert!(host.matters(), "{host:?} is worth showing");
        }
    }
}
