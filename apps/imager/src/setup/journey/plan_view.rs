//! The plan pages and the plan's rows in the review: each choice as the
//! shared segmented strip, the computer's name, and the values the review
//! takes from the plan.
use ui::widget::toggle_group::Press as SegmentPress;
use ui::widget::{Segment, ToggleGroup};

use super::*;

impl InstallerPreview {
    /// The name the plan carries for the other computer, or that the computer
    /// answers it when it installs.
    pub(super) fn plan_name_row(&self) -> ListRow {
        let value = if self.journey.plan.name_open || self.journey.device_name.trim().is_empty() {
            t("installer-preview-name-open-title")
        } else {
            self.journey.device_name.clone()
        };
        self.review_selection_row(
            "plan-name",
            "machine",
            t("installer-preview-device-name-label"),
            value,
        )
        .icon(Icon::Monitor)
    }

    /// One of the plan's choices as the review shows it: what the row is
    /// about, and the value it takes, or that the computer answers it when it
    /// installs.
    pub(super) fn plan_review_row(
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
    pub(super) fn plan_review_rows(&self) -> ui::widget::Column {
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
                "delivery",
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

    /// One of the plan's choices as the shared segmented strip: one answer of
    /// several, taken now, with the words for the answer it holds underneath.
    pub(super) fn plan_choice(
        &self,
        id: &str,
        heading: &str,
        paint_heading: bool,
        options: &[(&str, bool)],
    ) -> ui::widget::Column {
        let selected = options.iter().position(|(_, chosen)| *chosen).unwrap_or(0);
        let segments = options
            .iter()
            .map(|(key, _)| Segment::new(t(&format!("installer-preview-{key}-title"))))
            .collect();
        let mut strip = ToggleGroup::new(segments, selected)
            .with_id(id)
            .with_accessible_name(t(heading));
        if paint_heading {
            strip = strip.caption(t(heading));
        }
        let presses = options
            .iter()
            .map(|(key, _)| {
                let effect = InstallerPreviewEffect::ChoosePreview {
                    choice: (*key).to_string(),
                };
                let commands = self.commands.clone();
                SegmentPress::View(Box::new(move || {
                    commands.borrow_mut().push(effect.clone());
                }))
            })
            .collect();
        strip.set_presses(presses);
        let chosen = options.get(selected).map_or("", |(key, _)| *key);
        page::related().push(Box::new(strip)).push(Box::new(
            Text::new(&t(&format!("installer-preview-{chosen}-body")))
                .with_id(format!("{id}.body")),
        ))
    }
}
