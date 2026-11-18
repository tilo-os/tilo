//! The plan the review shows for the other computer: what it decides, in the
//! values it takes.

use super::InstallerPreview;
use super::machine;
use super::strings::t;
use crate::page;
use ui::icons::Icon;

impl InstallerPreview {
    /// One of the plan's choices as the review shows it: what the row is
    /// about, and the value it takes, or that the computer answers it when it
    /// installs.
    fn plan_review_row(
        &self,
        id: &str,
        section: &str,
        label: &str,
        chosen: Option<&str>,
        open: &str,
    ) -> ListRow {
        let value = chosen.unwrap_or(open);
        self.review_selection_row(
            id,
            section,
            t(&format!("installer-preview-{label}-heading")),
            t(&format!("installer-preview-{value}-title")),
        )
    }

    /// What the plan decides for the other computer, in the values it takes.
    fn plan_review_rows(&self) -> ui::widget::Column {
        use machine::{Disks, Hosting, Unlock, Updates};
        let plan = &self.journey.plan;
        let mut rows = page::related();
        let os = plan.os();
        rows = rows.push(Box::new(
            self.plan_review_row(
                "plan-os",
                "destination",
                "plan-os",
                Some(os.key()),
                os.key(),
            )
            .icon(Icon::Monitor),
        ));
        let disk = plan.disk.map(|disk| match disk {
            Disks::Largest => "disk-largest",
            Disks::Empty => "disk-empty",
            Disks::Only => "disk-only",
        });
        rows = rows.push(Box::new(
            self.plan_review_row("plan-disk", "storage", "disk", disk, "disk-open")
                .icon(Icon::HardDrive),
        ));
        let unlock = plan.unlock.map(|unlock| match unlock {
            Unlock::Hardware => "unlock-tpm",
            Unlock::OwnerDevice => "unlock-owner-device",
            Unlock::Passphrase => "unlock-passphrase",
        });
        let updates = plan.updates.map(|updates| match updates {
            Updates::Stable => "updates-stable",
            Updates::Development => "updates-dev",
        });
        let hosting = (plan.hosting == Some(Hosting::HoldsNone)).then_some("hosting-none");
        for (id, label, chosen, open, icon) in [
            (
                "plan-network",
                "network",
                plan.network.map(|_| "network-dhcp"),
                "network-open",
                Icon::EthernetPort,
            ),
            ("plan-unlock", "unlock", unlock, "unlock-open", Icon::Lock),
            (
                "plan-updates",
                "updates",
                updates,
                "updates-open",
                Icon::Refresh,
            ),
        ] {
            rows = rows.push(Box::new(
                self.plan_review_row(id, "machine", label, chosen, open)
                    .icon(icon),
            ));
        }
        if plan.os() == machine::Os::TiloServer {
            rows = rows.push(Box::new(
                self.plan_review_row(
                    "plan-hosting",
                    "machine",
                    "hosting",
                    hosting,
                    "hosting-open",
                )
                .icon(Icon::Layers),
            ));
        }
        let delivery = plan.delivery.key();
        rows = rows.push(Box::new(
            self.plan_review_row(
                "plan-delivery",
                "machine",
                "delivery",
                Some(delivery),
                delivery,
            )
            .icon(match plan.delivery {
                machine::Delivery::Usb => Icon::Usb,
                machine::Delivery::Netboot => Icon::Terminal,
                machine::Delivery::Alongside => Icon::Disc3,
            }),
        ));
        rows
    }

    fn machine_status(&self) -> serde_json::Value {
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
}
