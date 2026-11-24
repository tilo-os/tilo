use super::*;

impl InstallerPreview {
    pub(super) fn body(&self, theme: &Theme) -> ui::widget::Column {
        let j = &self.journey;
        let key = if j.page == Page::Storage {
            match j.destination {
                Some(Destination::Another) => "disk",
                Some(Destination::Desktop) => "desktop",
                _ if !self.direct_route_enabled() => "storage-usb-fallback",
                _ => "storage",
            }
        } else {
            j.page.key()
        };
        let heading_role = if matches!(j.page, Page::Destination | Page::Password) {
            TextRole::Display
        } else {
            TextRole::LargeTitle
        };
        let heading = if j.page == Page::Devices && j.destination == Some(Destination::Another) {
            t("installer-preview-devices-another-heading")
        } else {
            t(&format!("installer-preview-{key}-heading"))
        };
        let intro = if j.page == Page::Destination && !self.guided_host_available() {
            t("installer-guided-host-unavailable")
        } else if j.page == Page::Devices && j.destination == Some(Destination::Another) {
            t("installer-preview-devices-another-intro")
        } else if j.page == Page::Password && j.destination == Some(Destination::Desktop) {
            t("installer-desktop-password-intro")
        } else {
            t(&format!("installer-preview-{key}-intro"))
        };
        let heading = Text::new(&heading)
            .with_id("installer.heading")
            .with_align(ui::ParagraphAlign::Start)
            .with_role(heading_role)
            .with_balanced_wrap()
            .with_heading_level(1);
        let mut title = page::stack().push(Box::new(heading));
        if !matches!(j.page, Page::Migrate | Page::Password | Page::Devices)
            && self.rect.width() < self.profile_breakpoint
        {
            title =
                title.with_horizontal_padding(0.0, theme.tokens.spacing.rhythm.get(Rhythm::Group));
        }
        let mut header = page::stack()
            .with_rhythm(Rhythm::Related)
            .push(Box::new(title));
        if !intro.is_empty() {
            let intro = Text::new(&intro)
                .with_id("installer.intro")
                .with_align(ui::ParagraphAlign::Start)
                .with_role(TextRole::Body);
            header = header.push(Box::new(intro));
        }
        let mut body = page::stack()
            .with_rhythm(Rhythm::Sibling)
            .push(Box::new(header));
        match j.page {
            Page::Destination => {
                if self.guided_host_available() {
                    body =
                        body.push(Box::new(
                            self.choice("computer", j.destination == Some(Destination::Computer)),
                        ))
                        .push(Box::new(
                            self.choice("another", j.destination == Some(Destination::Another)),
                        ));
                }
                if j.destination == Some(Destination::Another) {
                    body = body.push(Box::new(self.plan_os()));
                }
                if self.linux {
                    body = body.push(Box::new(
                        self.choice("desktop", j.destination == Some(Destination::Desktop)),
                    ));
                }
                if !self.machine.waiting.is_empty() {
                    body = body.push(Box::new(self.waiting_machines()));
                }
            }
            Page::Migrate => {
                body = body.push(Box::new(self.migrate_content()));
            }
            Page::Target => {
                body = body.push(Box::new(self.target.page(&self.commands)));
            }
            Page::Password => {
                // The walk through the setup never reaches this page: the normal
                // login password belongs to the destination's first boot. Only
                // an explicit edit of the review's password section opens it.
                body = body
                    .push(Box::new(self.field(
                        "password",
                        &j.password,
                        TextInputKind::Password,
                    )))
                    .push(Box::new(self.field(
                        "confirmation",
                        &j.confirmation,
                        TextInputKind::Password,
                    )));
            }
            Page::Devices => {
                body = body.push(Box::new(self.device_name_form()));
            }
            Page::Purpose => {
                body = body
                    .push(Box::new(
                        self.choice("purpose-computer", j.purpose == "computer"),
                    ))
                    .push(Box::new(
                        self.choice("purpose-server", j.purpose == "server"),
                    ));
            }
            Page::Storage => {
                let destination = j.destination.unwrap_or(Destination::Computer);
                if destination == Destination::Computer {
                    body = body.push(Box::new(self.storage_content()));
                } else if destination == Destination::Another {
                    body = body.push(Box::new(self.disk_choice()));
                }
            }
            Page::Method => {
                body = body.push(Box::new(self.plan_choice(
                    "installer.plan.delivery",
                    "installer-preview-delivery-heading",
                    true,
                    &[
                        ("usb", j.plan.delivery == machine::Delivery::Usb),
                        ("netboot", j.plan.delivery == machine::Delivery::Netboot),
                        ("alongside", j.plan.delivery == machine::Delivery::Alongside),
                    ],
                )));
            }
            Page::Machine => {
                body = body
                    .with_rhythm(Rhythm::Section)
                    .push(Box::new(self.plan_name_field()))
                    .push(Box::new(self.plan_choice(
                        "installer.plan.network",
                        "installer-preview-network-heading",
                        true,
                        &[
                            ("network-dhcp", j.plan.network.is_some()),
                            ("network-open", j.plan.network.is_none()),
                        ],
                    )))
                    .push(Box::new(self.plan_choice(
                        "installer.plan.unlock",
                        "installer-preview-unlock-heading",
                        true,
                        &[
                            (
                                "unlock-tpm",
                                j.plan.unlock == Some(machine::Unlock::Hardware),
                            ),
                            (
                                "unlock-owner-device",
                                j.plan.unlock == Some(machine::Unlock::OwnerDevice),
                            ),
                            (
                                "unlock-passphrase",
                                j.plan.unlock == Some(machine::Unlock::Passphrase),
                            ),
                            ("unlock-open", j.plan.unlock.is_none()),
                        ],
                    )))
                    .push(Box::new(self.plan_choice(
                        "installer.plan.updates",
                        "installer-preview-updates-heading",
                        true,
                        &[
                            (
                                "updates-stable",
                                j.plan.updates == Some(machine::Updates::Stable),
                            ),
                            (
                                "updates-dev",
                                j.plan.updates == Some(machine::Updates::Development),
                            ),
                            ("updates-open", j.plan.updates.is_none()),
                        ],
                    )));
                if j.plan.os() == machine::Os::TiloServer {
                    body = body.push(Box::new(self.plan_choice(
                        "installer.plan.hosting",
                        "installer-preview-hosting-heading",
                        true,
                        &[
                            (
                                "hosting-none",
                                j.plan.hosting == Some(machine::Hosting::HoldsNone),
                            ),
                            (
                                "hosting-open",
                                j.plan.hosting != Some(machine::Hosting::HoldsNone),
                            ),
                        ],
                    )));
                }
            }
            Page::Review => {
                let computer_usb_fallback =
                    j.destination == Some(Destination::Computer) && !self.direct_route_enabled();
                if self.live
                    && (computer_usb_fallback
                        || (j.destination == Some(Destination::Another)
                            && !self.usb_backend_available()))
                {
                    body = body.push(Box::new(
                        Text::new(&t(if self.usb_backend_available() {
                            "installer-review-usb-fallback"
                        } else {
                            "installer-review-installation-unavailable"
                        }))
                        .with_id("installer.review.usb-fallback"),
                    ));
                }
                let destination = j.destination.unwrap_or(Destination::Computer);
                let plan_server =
                    destination == Destination::Another && j.plan.os() == machine::Os::TiloServer;
                let mut setup = page::related().push(Box::new(
                    Text::new(&t("installer-review-setup"))
                        .with_role(TextRole::Subheading)
                        .with_heading_level(2),
                ));
                if destination != Destination::Another {
                    setup = setup.push(Box::new(self.review_device_row()));
                }
                setup = setup.push(Box::new(
                    self.review_row(
                        "destination",
                        t(&format!("installer-preview-{}-title", destination.key())),
                        t(
                            if destination == Destination::Computer && !self.direct_route_enabled()
                            {
                                "installer-preview-computer-usb-body"
                            } else {
                                match destination {
                                    Destination::Computer => "installer-preview-computer-body",
                                    Destination::Another => "installer-preview-another-body",
                                    Destination::Desktop => "installer-preview-desktop-body",
                                }
                            },
                        ),
                    )
                    .icon(match destination {
                        Destination::Computer => Icon::Monitor,
                        Destination::Another => Icon::Usb,
                        Destination::Desktop => Icon::MonitorSmartphone,
                    }),
                ));
                if destination == Destination::Another {
                    setup = setup.push(Box::new(self.plan_name_row()));
                    setup = setup.push(Box::new(
                        self.review_selection_row(
                            "target",
                            "target",
                            t("installer-preview-target-model-label"),
                            self.target.review_plan_summary(),
                        )
                        .icon(Icon::Monitor),
                    ));
                    setup = setup.push(Box::new(self.plan_review_rows()));
                }
                if destination != Destination::Desktop && !j.password.is_empty() {
                    setup = setup.push(Box::new(self.review_selection_row(
                        "purpose",
                        "purpose",
                        t(&format!("installer-preview-purpose-{}-title", j.purpose)),
                        t(&format!("installer-preview-purpose-{}-body", j.purpose)),
                    )));
                }
                if destination != Destination::Desktop && self.sensitive_handoff_deferred() {
                    setup = setup.push(Box::new(
                        Text::new(&t(if plan_server {
                            "installer-preview-deferred-handoff-server"
                        } else {
                            "installer-preview-deferred-handoff"
                        }))
                        .with_id("installer.review.handoff"),
                    ));
                }
                if destination == Destination::Computer && self.direct_route_enabled() {
                    setup = setup.push(Box::new(self.review_row(
                        "storage",
                        t("installer-preview-storage-size-label"),
                        self.review_storage_summary(),
                    )));
                }
                body = body.with_rhythm(Rhythm::Group).push(Box::new(setup));
            }
            Page::Ready => {
                body = body.push(Box::new(IconWidget::new(Icon::Check).accent()));
                if self.journey.destination == Some(Destination::Another)
                    && let Some(created) = &self.machine.created
                {
                    let mut plan = page::related().push(Box::new(
                        Text::new(&t("installer-plan-heading"))
                            .with_role(TextRole::Subheading)
                            .with_heading_level(2)
                            .with_id("installer.plan.heading"),
                    ));
                    let token = created.token_file.display().to_string();
                    plan = plan.push(Box::new(
                        Text::new(&t_args(
                            "installer-plan-token",
                            &[("token", token.as_str().into())],
                        ))
                        .with_id("installer.plan.token"),
                    ));
                    if let Some(hours) = machine::hours_left(created.plan.token.expires_at_ns) {
                        let hours = hours.to_string();
                        plan = plan.push(Box::new(
                            Text::new(&t_args(
                                "installer-plan-expiry",
                                &[("hours", hours.as_str().into())],
                            ))
                            .with_id("installer.plan.expiry"),
                        ));
                    }
                    let (label, command) = match self.journey.plan.delivery {
                        machine::Delivery::Usb => {
                            ("installer-plan-carry-usb", &created.commands.medium)
                        }
                        machine::Delivery::Netboot => {
                            ("installer-plan-carry-netboot", &created.commands.netboot)
                        }
                        machine::Delivery::Alongside => (
                            "installer-plan-carry-alongside",
                            &created.commands.alongside,
                        ),
                    };
                    plan = plan
                        .push(Box::new(
                            Text::new(&t(label)).with_id("installer.plan.carry"),
                        ))
                        .push(Box::new(
                            Text::new(command).with_id("installer.plan.command"),
                        ));
                    let questions = machine::questions(&created.plan, &[]);
                    if !questions.is_empty() {
                        plan = plan.push(Box::new(
                            Text::new(&t("installer-plan-open-heading"))
                                .with_role(TextRole::Subheading)
                                .with_heading_level(2),
                        ));
                        for question in questions {
                            plan = plan.push(Box::new(
                                Text::new(&question.text)
                                    .with_id(format!("installer.plan.open.{}", question.choice)),
                            ));
                        }
                    }
                    body = body.push(Box::new(plan));
                }
            }
        }
        body
    }

    /// What the other computer installs: Tilo OS or Tilo Server, two destination
    /// OSes, never a purpose question.
    fn plan_os(&self) -> ui::widget::Column {
        self.plan_choice(
            "installer.plan.os",
            "installer-preview-plan-os-heading",
            true,
            &[
                ("os", self.journey.plan.os() == machine::Os::TiloOs),
                ("server", self.journey.plan.os() == machine::Os::TiloServer),
            ],
        )
    }

    /// Which drive the other computer installs to: a rule, because its real
    /// drives are only known when it installs.
    fn disk_choice(&self) -> ui::widget::Column {
        self.plan_choice(
            "installer.plan.disk",
            "installer-preview-disk-heading",
            false,
            &[
                (
                    "disk-largest",
                    self.journey.plan.disk == Some(machine::Disks::Largest),
                ),
                (
                    "disk-empty",
                    self.journey.plan.disk == Some(machine::Disks::Empty),
                ),
                (
                    "disk-only",
                    self.journey.plan.disk == Some(machine::Disks::Only),
                ),
                ("disk-open", self.journey.plan.disk.is_none()),
            ],
        )
    }

    /// The other computer's name: the name the plan carries, and the way to
    /// leave it for the computer to answer when it installs.
    fn plan_name_field(&self) -> ui::widget::Column {
        let mut section = page::related().push(Box::new(self.field(
            "device-name",
            &self.journey.device_name,
            TextInputKind::Name,
        )));
        if self.journey.device_name.trim().is_empty() {
            section = section.push(Box::new(
                Text::new(&t("installer-preview-name-open-body"))
                    .with_id("installer.plan.name-open"),
            ));
        }
        section
    }

    /// The computers this person planned and has not installed, each with what
    /// it still asks answered against the hardware it reported.
    fn waiting_machines(&self) -> ui::widget::Column {
        let mut section = page::related().push(Box::new(
            Text::new(&t("installer-plan-waiting-heading"))
                .with_role(TextRole::Subheading)
                .with_heading_level(2)
                .with_id("installer.waiting.heading"),
        ));
        section = section.push(Box::new(Text::new(&t("installer-plan-waiting-intro"))));
        for (index, waiting) in self.machine.waiting.iter().enumerate() {
            let mut machine = page::related().push(Box::new(
                Text::new(&waiting.title())
                    .with_role(TextRole::Title)
                    .with_id(format!("installer.waiting.{index}")),
            ));
            let hours = waiting.expires_in_hours.to_string();
            machine = machine.push(Box::new(Text::new(&t_args(
                "installer-plan-waiting-summary",
                &[
                    ("os", waiting.destination.as_str().into()),
                    ("hours", hours.as_str().into()),
                ],
            ))));
            for question in &waiting.questions {
                machine = machine.push(Box::new(
                    Text::new(&question.text)
                        .with_id(format!("installer.waiting.{index}.{}", question.choice)),
                ));
            }
            if let Some(answer) = &waiting.disk_answer {
                machine = machine.push(Box::new(
                    Text::new(answer).with_id(format!("installer.waiting.{index}.disk-answer")),
                ));
            }
            if waiting.questions.is_empty() {
                machine = machine.push(Box::new(Text::new(&t(
                    "installer-plan-waiting-everything-answered",
                ))));
            }
            section = section.push(Box::new(machine));
        }
        section
    }

    pub(super) fn composition(&self, theme: &Theme) -> Box<dyn Widget> {
        if self.full_os_host && self.live {
            return self.full_os_host_view();
        }
        let mut actions = Row::new();
        if (self.journey.page != Page::Destination || self.journey.cancels_edit())
            && self.journey.page != Page::Ready
        {
            actions = actions.push(Box::new(
                self.button(
                    "installer.back",
                    if self.journey.cancels_edit() {
                        "installer-preview-cancel"
                    } else {
                        "installer-preview-back"
                    },
                    InstallerPreviewEffect::BackInPreview,
                )
                .role(ButtonRole::Plain),
            ));
        }
        actions = actions.push(Box::new(Spacer::new()));
        if self.live
            && self.journey.page == Page::Review
            && !self.journey.editing_destination()
            && (self.usb_backend_available()
                || (self.journey.destination == Some(Destination::Another)
                    && self.journey.plan.delivery != machine::Delivery::Usb))
            && matches!(
                self.journey.destination,
                Some(Destination::Computer | Destination::Another)
            )
        {
            actions = actions.push(Box::new(
                self.button(
                    "installer.prepare-usb",
                    if self.journey.destination == Some(Destination::Another)
                        && self.journey.plan.delivery != machine::Delivery::Usb
                    {
                        "installer-plan-make"
                    } else if self.journey.destination == Some(Destination::Another) {
                        "installer-plan-make-and-medium"
                    } else {
                        "installer-preview-prepare-usb"
                    },
                    InstallerPreviewEffect::PrepareUsb,
                )
                .role(
                    if self.journey.destination == Some(Destination::Computer)
                        && self.direct_route_enabled()
                    {
                        ButtonRole::Plain
                    } else {
                        ButtonRole::Primary
                    },
                ),
            ));
        }
        if self.live
            && self.journey.page == Page::Review
            && !self.journey.editing_destination()
            && self.journey.destination == Some(Destination::Computer)
            && self.direct_route_enabled()
        {
            actions = actions.push(Box::new(
                self.button(
                    "installer.prepare-direct",
                    "installer-preview-prepare-direct",
                    InstallerPreviewEffect::PrepareDirectInstallation,
                )
                .role(ButtonRole::Primary)
                .with_enabled(self.direct_available()),
            ));
        }
        if self.live
            && self.journey.page == Page::Review
            && !self.journey.editing_destination()
            && self.journey.destination == Some(Destination::Desktop)
        {
            actions = actions.push(Box::new(
                self.button(
                    "installer.prepare-desktop",
                    "installer-desktop-prepare",
                    InstallerPreviewEffect::PrepareDesktopInstallation,
                )
                .role(ButtonRole::Primary)
                .with_enabled(!self.busy()),
            ));
        }
        let (label, effect) = match self.journey.page {
            _ if self.journey.saving_edit() => (
                "installer-preview-save",
                InstallerPreviewEffect::ContinuePreview,
            ),
            Page::Review if self.live => (
                "installer-preview-continue",
                InstallerPreviewEffect::ContinuePreview,
            ),
            Page::Ready => (
                "installer-preview-restart",
                InstallerPreviewEffect::RestartPreview,
            ),
            Page::Review if self.journey.destination == Some(Destination::Another) => (
                "installer-plan-make",
                InstallerPreviewEffect::ContinuePreview,
            ),
            Page::Review => (
                "installer-preview-finish",
                InstallerPreviewEffect::ContinuePreview,
            ),
            _ => (
                "installer-preview-continue",
                InstallerPreviewEffect::ContinuePreview,
            ),
        };
        let chooses_path =
            self.journey.page == Page::Destination && !self.journey.editing_destination();
        if !chooses_path
            && !(self.journey.page == Page::Ready
                && self.journey.destination == Some(Destination::Another))
            && !(self.live && self.journey.page == Page::Review && !self.journey.editing_choices())
        {
            actions = actions.push(Box::new(
                self.button("installer.next", label, effect)
                    .role(ButtonRole::Primary),
            ));
        }
        let mut content = page::stack()
            .with_rhythm(Rhythm::Group)
            .push(Box::new(self.body(theme)));
        if let Some(error) = self.error
            && self.password_validation().is_none()
        {
            content = content.push(Box::new(
                ui::widget::Banner::error(t(error)).with_id("installer.error"),
            ));
        }
        if matches!(
            self.journey.page,
            Page::Migrate | Page::Password | Page::Devices
        ) {
            content = page::stack().push(Box::new(
                Container::new()
                    .with_no_padding()
                    .with_centered_max_width(theme.tokens.metrics.content_min_width)
                    .with_child(Box::new(content)),
            ));
        }
        let frame = page::Frame::new(content).with_cursor(self.cursor);
        let frame = if matches!(self.journey.page, Page::Devices | Page::Review) {
            // A review is a page of many rows: it starts at the top like every
            // page with content to read, never floated to the middle of a tall
            // window.
            frame
        } else {
            frame.center_short_body()
        };
        let review_blocker = (self.live
            && self.journey.page == Page::Review
            && self.journey.destination == Some(Destination::Computer)
            && self.direct_route_enabled())
        .then(|| self.direct_blocker())
        .flatten();
        let notice = if self.demo || review_blocker.is_some() {
            let mut notice = page::related();
            if self.demo {
                notice = notice.push(Box::new(
                    ui::widget::Banner::info(t("installer-demo-notice-title"))
                        .with_message(t("installer-demo-notice-body"))
                        .with_id("installer.demo-notice"),
                ));
            }
            if let Some(reason) = review_blocker {
                notice = notice.push(Box::new(
                    Text::new(&reason).with_id("installer.review.blocker"),
                ));
            }
            Some(Box::new(notice) as Box<dyn Widget>)
        } else {
            None
        };
        let frame = frame.notice(notice);
        if self.journey.page != Page::Destination || self.journey.cancels_edit() {
            frame.actions(actions).build()
        } else {
            frame.build()
        }
    }
}
