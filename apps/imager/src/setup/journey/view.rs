use super::*;

impl InstallerPreview {
    pub(super) fn body(&self, theme: &Theme) -> ui::widget::Column {
        let j = &self.journey;
        let key = if j.page == Page::Storage {
            match j.destination {
                Some(Destination::Another) => "usb",
                Some(Destination::Desktop) => "desktop",
                _ if !self.direct_route_enabled() => "storage-usb-fallback",
                _ => "storage",
            }
        } else {
            j.page.key()
        };
        let heading_role = if matches!(j.page, Page::Destination | Page::Profile | Page::Password) {
            TextRole::Display
        } else {
            TextRole::LargeTitle
        };
        let choosing_pair_name = imager_backend::pairing::Pairing::supported()
            && self.live
            && j.page == Page::Connect
            && j.transfer.as_deref() == Some("device")
            && !self.pairing.started();
        let mounted_step = self.live
            && j.page == Page::Belongings
            && j.destination == Some(Destination::Another)
            && j.categories[0]
            && self.portable_source.mounted_permission_active();
        let reviewing_sessions = j.page == Page::Belongings && self.sessions_review;
        let heading = if reviewing_sessions {
            t("installer-service-sessions-title")
        } else if mounted_step {
            t("installer-mounted-home-permission-heading")
        } else if self.unlocking_preparation() {
            t("installer-restart-unlock-heading")
        } else if choosing_pair_name {
            t("installer-pairing-name-heading")
        } else if j.page == Page::Accounts {
            self.accounts.heading()
        } else {
            t(&format!("installer-preview-{key}-heading"))
        };
        let intro = if reviewing_sessions {
            t("installer-logins-source-body")
        } else if matches!(j.page, Page::Destination | Page::Migration)
            && !self.guided_host_available()
        {
            t("installer-guided-host-unavailable")
        } else if mounted_step {
            t("installer-mounted-home-grant-intro")
        } else if self.unlocking_preparation() {
            t("installer-restart-unlock-intro")
        } else if j.page == Page::Accounts {
            self.accounts.subheading()
        } else if j.page == Page::Belongings && j.destination == Some(Destination::Desktop) {
            t("installer-desktop-belongings-intro")
        } else if j.page == Page::Belongings
            && j.destination == Some(Destination::Computer)
            && j.categories[0]
        {
            t("installer-preview-belongings-shared-intro")
        } else if j.page == Page::Migration {
            t("installer-preview-migration-intro")
        } else if matches!(j.page, Page::Identity | Page::Connect) {
            String::new()
        } else if j.page == Page::Password && j.destination == Some(Destination::Desktop) {
            t("installer-desktop-password-intro")
        } else {
            t(&format!("installer-preview-{key}-intro"))
        };
        let intro =
            if self.live && j.page == Page::Connect && j.transfer.as_deref() == Some("device") {
                self.pairing.intro()
            } else {
                intro
            };
        let heading = Text::new(&heading)
            .with_id("installer.heading")
            .with_align(ui::ParagraphAlign::Start)
            .with_role(heading_role)
            .with_balanced_wrap()
            .with_heading_level(1);
        let mut title = page::stack().push(Box::new(heading));
        if !matches!(j.page, Page::Profile | Page::Password | Page::Devices)
            && self.rect.width() < self.profile_breakpoint
        {
            // Give a compact page title breathing room before the scrolling
            // edge. The form and explanatory copy keep their full measure.
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
        if self.media_preparation_admitted() {
            let phase = self.media_preparation.phase_key();
            let mut status = ui::widget::Banner::info(t(phase
                .as_deref()
                .unwrap_or("installer-media-phase-review")))
            .with_id("installer.preparation-status");
            if let Some(bytes) = self.media_preparation.progress_label() {
                status = status.with_message(bytes);
            }
            body = body.push(Box::new(status));
        }
        match j.page {
            Page::Migration if !self.guided_host_available() => {}
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
                if self.linux {
                    body = body.push(Box::new(
                        self.choice("desktop", j.destination == Some(Destination::Desktop)),
                    ));
                }
            }
            Page::Migration => {
                body = body
                    .push(Box::new(self.choice("migrate", j.migrate == Some(true))))
                    .push(Box::new(self.choice("fresh", j.migrate == Some(false))));
            }
            Page::Target => {
                body = body.push(Box::new(self.target.page(&self.commands)));
            }
            Page::Identity => {
                body = body
                    .push(Box::new(
                        self.choice("new", j.identity == Some(Identity::New)),
                    ))
                    .push(Box::new(
                        self.choice("existing", j.identity == Some(Identity::Existing)),
                    ));
            }
            Page::Profile => {
                // Bind the current name before constructing its retained avatar.
                let name = self.field("name", &j.name, TextInputKind::Name);
                let picture = page::related()
                    .push(Box::new(
                        Row::new()
                            .with_justify(taffy::JustifyContent::Center)
                            .push(Box::new(
                                Container::new()
                                    .with_no_padding()
                                    .hug()
                                    .with_id("installer.profile.avatar")
                                    .with_child(Box::new(
                                        self.avatar(&j.name)
                                            .with_name_state(&self.field_values[0])
                                            .size(AvatarSize::Portrait),
                                    )),
                            )),
                    ))
                    .push(Box::new(
                        Row::new()
                            .with_justify(taffy::JustifyContent::Center)
                            .push(Box::new(
                                self.button(
                                    "installer.avatar",
                                    "installer-preview-avatar-change",
                                    InstallerPreviewEffect::ChoosePreviewAvatar,
                                )
                                .role(ButtonRole::Plain),
                            )),
                    ));
                body = body
                    .with_rhythm(Rhythm::Section)
                    .push(Box::new(picture))
                    .push(Box::new(name));
            }
            Page::Password => {
                // New interactive setups never reach this page: the normal
                // login password belongs to the destination's first boot. This
                // remains only for retained ciphertext from an older build.
                body = body.push(Box::new(self.field(
                    "password",
                    &j.password,
                    TextInputKind::Password,
                )));
                if !self.unlocking_preparation() {
                    body = body.push(Box::new(self.field(
                        "confirmation",
                        &j.confirmation,
                        TextInputKind::Password,
                    )));
                }
            }
            Page::Connect => {
                if self.live
                    && self.journey.transfer.as_deref() == Some("device")
                    && self.identity_route_available("device")
                {
                    if !self.pairing.started() {
                        body = body.push(Box::new(self.field(
                            "device-name",
                            &j.device_name,
                            TextInputKind::Name,
                        )));
                    }
                    let commands = self.commands.clone();
                    let cancel_commands = self.commands.clone();
                    body = body.push(self.pairing.view(
                        Rc::new(move || {
                            commands
                                .borrow_mut()
                                .push(InstallerPreviewEffect::StartExistingDevicePairing)
                        }),
                        Rc::new(move || {
                            cancel_commands
                                .borrow_mut()
                                .push(InstallerPreviewEffect::CancelExistingDevicePairing)
                        }),
                    ));
                    return body;
                }

                if self.live && self.journey.transfer.as_deref() == Some("backup") {
                    let commands = self.commands.clone();
                    let edit_commands = self.commands.clone();
                    let submits = self.commands.clone();
                    body = body.push(self.existing_identity.view(
                        Rc::new(move || {
                            commands
                                .borrow_mut()
                                .push(InstallerPreviewEffect::ChooseExistingIdentityBackup)
                        }),
                        Rc::new(move |value| {
                            edit_commands
                                .borrow_mut()
                                .push(InstallerPreviewEffect::SetExistingIdentityPassword { value })
                        }),
                        Rc::new(move || queue_continue(&submits)),
                    ));
                    return body;
                }
                if self.live && self.journey.transfer.as_deref() == Some("current") {
                    body = body.push(self.existing_identity.current_view());
                    return body;
                }
                if self.identity_route_available("current") {
                    body = body.push(Box::new(
                        self.choice("current", j.transfer.as_deref() == Some("current")),
                    ));
                }
                if self.identity_route_available("device") {
                    body = body.push(Box::new(
                        self.choice("device", j.transfer.as_deref() == Some("device")),
                    ));
                }
                if self.identity_route_available("backup") {
                    body = body.push(Box::new(
                        self.choice("backup", j.transfer.as_deref() == Some("backup")),
                    ));
                }
                if !self.identity_route_available("device")
                    && !self.identity_route_available("backup")
                    && !self.identity_route_available("current")
                {
                    body = body.push(Box::new(Text::new(&t(
                        "installer-identity-route-unavailable",
                    ))));
                }
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
            Page::Belongings => {
                if self.sessions_review {
                    let individual = self.commands.clone();
                    let group = self.commands.clone();
                    let expand = self.commands.clone();
                    return body.push(self.belongings.sessions.logins_view(
                        j.categories[2],
                        move |source_id, service, included| {
                            individual.borrow_mut().push(
                                InstallerPreviewEffect::SetPreviewServiceSession {
                                    source_id,
                                    service: service.as_str().into(),
                                    included,
                                },
                            )
                        },
                        move |source_id, included| {
                            group.borrow_mut().push(
                                InstallerPreviewEffect::SetPreviewServiceSessionGroup {
                                    source_id,
                                    included,
                                },
                            )
                        },
                        move |source_id, expanded| {
                            expand.borrow_mut().push(
                                InstallerPreviewEffect::ExpandServiceSessionGroup {
                                    source_id,
                                    expanded,
                                },
                            )
                        },
                    ));
                }
                let mounted_step = self.live
                    && j.destination == Some(Destination::Another)
                    && j.categories[0]
                    && self.portable_source.mounted_permission_active();
                if mounted_step {
                    {
                        let choices = self.portable_source.mounted_choices();
                        if self.portable_source.mounted_needs_restart() {
                            body = body.push(Box::new(self.button(
                                "installer.mounted-locations.restart",
                                "installer-mounted-home-restart",
                                InstallerPreviewEffect::RestartMountedHomeReview,
                            )));
                        } else if let Some((id, name, _)) =
                            choices.iter().find(|(_, _, approved)| !approved)
                        {
                            body = body.push(Box::new(
                                ListRow::new("installer.mounted-location.current", name.clone())
                                    .secondary(t("installer-mounted-home-needs-grant"))
                                    .two_line_secondary()
                                    .icon(Icon::Folder),
                            ));
                            body = body.push(Box::new(self.button(
                                "installer.mounted-location.current.grant",
                                "installer-mounted-home-grant",
                                InstallerPreviewEffect::GrantMountedHomeLocation {
                                    location: id.clone(),
                                },
                            )));
                        } else if !choices.is_empty() {
                            body = body.push(Box::new(Text::new(&t(
                                "installer-mounted-home-all-granted",
                            ))));
                            body = body.push(Box::new(self.button(
                                "installer.mounted-locations.continue",
                                "installer-mounted-home-review",
                                InstallerPreviewEffect::ContinueMountedHomeReview,
                            )));
                        }
                        if self.portable_source.busy() {
                            body = body.push(Box::new(
                                self.button(
                                    "installer.mounted-locations.cancel",
                                    "installer-mounted-home-cancel",
                                    InstallerPreviewEffect::CancelMountedHomePermission,
                                )
                                .role(ButtonRole::Plain),
                            ));
                        }
                    }
                } else {
                    if self.migration_busy() {
                        body = body.push(Box::new(Text::new(&t("installer-migration-preparing"))));
                    }
                    if j.destination == Some(Destination::Desktop) {
                        for (id, title, description, icon) in [
                            (
                                "files",
                                "installer-preview-files-shared-title",
                                "installer-desktop-files-in-place",
                                Icon::Folder,
                            ),
                            (
                                "apps",
                                "installer-flatpak-apps-existing-heading",
                                "installer-desktop-apps-in-place",
                                Icon::Monitor,
                            ),
                        ] {
                            body = body.push(Box::new(
                                ListRow::new(format!("installer.desktop.{id}.in-place"), t(title))
                                    .secondary(t(description))
                                    .two_line_secondary()
                                    .icon(icon),
                            ));
                        }
                    }
                    if self.live && self.belongings_worker.is_none() {
                        body = body.push(self.your_data_view());
                    }
                    for (index, key) in CATEGORIES.iter().enumerate() {
                        if !self.category_available(index) || (!j.details && !j.categories[index]) {
                            continue;
                        }
                        let title = if index == 0 && j.destination == Some(Destination::Computer) {
                            t("installer-preview-files-shared-title")
                        } else if index == 1
                            && j.categories[0]
                            && j.destination == Some(Destination::Another)
                        {
                            t("installer-preview-flatpak-system-title")
                        } else {
                            t(&format!("installer-preview-{key}-title"))
                        };
                        let subtitle = if self.live && self.belongings_worker.is_some() {
                            t("installer-preview-folders-checking")
                        } else if self.live && index == 1 {
                            if self.belongings.flatpak_unavailable {
                                t("installer-preview-flatpak-unavailable")
                            } else {
                                let count = self.flatpak_count();
                                t_args(
                                    if j.categories[0]
                                        && j.destination == Some(Destination::Another)
                                    {
                                        "installer-preview-flatpak-system-found"
                                    } else if j.destination == Some(Destination::Computer) {
                                        "installer-preview-flatpak-computer-found"
                                    } else {
                                        "installer-preview-flatpak-found"
                                    },
                                    &[("count", count.to_string().into())],
                                )
                            }
                        } else if self.live && index == 2 {
                            if j.details
                                || (self.belongings.accounts.candidates.is_empty()
                                    && self.belongings.accounts.tilo_accounts.is_empty())
                            {
                                t("installer-preview-accounts-body")
                            } else {
                                self.belongings.accounts_overview_hint()
                            }
                        } else if self.live && self.belongings.home_unavailable {
                            t("installer-preview-home-unavailable")
                        } else if index == 0 && j.destination == Some(Destination::Computer) {
                            t("installer-preview-files-shared-body")
                        } else if index == 1 && j.destination == Some(Destination::Computer) {
                            t("installer-preview-flatpak-computer-body")
                        } else if index == 1
                            && j.categories[0]
                            && j.destination == Some(Destination::Another)
                        {
                            t("installer-preview-flatpak-system-body")
                        } else {
                            t(&format!("installer-preview-{key}-body"))
                        };
                        if j.details {
                            let checked = ui::LocalState::new(j.categories[index]);
                            let commands = self.commands.clone();
                            let category = key.to_string();
                            let mut category_group = page::related()
                                .push(Box::new(
                                    Checkbox::from_state(&checked)
                                        .with_id(&format!("installer.category.{key}"))
                                        .with_label(title)
                                        .on_change(move |included| {
                                            commands.borrow_mut().push(
                                                InstallerPreviewEffect::SetPreviewCategory {
                                                    category: category.clone(),
                                                    included,
                                                },
                                            )
                                        }),
                                ))
                                .push(Box::new(Text::new(&subtitle)));
                            if self.live
                                && index == 0
                                && j.categories[0]
                                && let Some(home) = &self.belongings.home
                            {
                                category_group = category_group
                                    .push(Box::new(
                                        Text::new(&t("installer-files-source-home"))
                                            .with_role(TextRole::Caption),
                                    ))
                                    .push(Box::new(
                                        self.button(
                                            "installer.files.source-location",
                                            if self.migration_source_location {
                                                "installer-files-hide-location"
                                            } else {
                                                "installer-files-show-location"
                                            },
                                            InstallerPreviewEffect::ToggleMigrationSourceLocation,
                                        )
                                        .role(ButtonRole::Plain),
                                    ));
                                if self.migration_source_location {
                                    category_group = category_group.push(Box::new(
                                        Text::new(&t_args(
                                            "installer-preview-files-location",
                                            &[("path", home.display().to_string().into())],
                                        ))
                                        .with_role(TextRole::Caption),
                                    ));
                                }
                            }
                            body = body.push(Box::new(category_group));
                        } else {
                            body = body.push(Box::new(
                                ListRow::new(format!("installer.category.{key}"), title)
                                    .secondary(subtitle)
                                    .two_line_secondary()
                                    .icon(match index {
                                        0 => Icon::Folder,
                                        1 => Icon::Monitor,
                                        _ => Icon::Users,
                                    }),
                            ));
                        }
                    }
                    if self.live && j.details {
                        let apps = self.flatpak_apps();
                        if !apps.is_empty() {
                            let mut app_group = page::related().push(Box::new(
                                Text::new(&t(if j.destination == Some(Destination::Desktop) {
                                    "installer-flatpak-apps-existing-heading"
                                } else {
                                    "installer-flatpak-apps-heading"
                                }))
                                .with_role(TextRole::Title),
                            ));
                            for (index, app) in apps.iter().enumerate() {
                                if app.selectable {
                                    let checked = ui::LocalState::new(app.included);
                                    let commands = self.commands.clone();
                                    let app_id = app.id.clone();
                                    app_group = app_group.push(Box::new(
                                        Checkbox::from_state(&checked)
                                            .with_id(&format!("installer.flatpak.app.{index}"))
                                            .with_label(&app.name)
                                            .with_enabled(j.categories[1])
                                            .on_change(move |included| {
                                                commands.borrow_mut().push(
                                                    InstallerPreviewEffect::SetPreviewFlatpakApp {
                                                        app: app_id.clone(),
                                                        included,
                                                    },
                                                );
                                            }),
                                    ));
                                } else {
                                    app_group = app_group.push(Box::new(
                                        ListRow::new(
                                            format!("installer.flatpak.app.{index}"),
                                            &app.name,
                                        )
                                        .secondary(t(if app.already_here {
                                            "installer-flatpak-app-existing"
                                        } else if app.with_home {
                                            "installer-flatpak-app-with-home"
                                        } else if j.destination == Some(Destination::Another) {
                                            "installer-flatpak-app-needs-home"
                                        } else {
                                            "installer-flatpak-app-transfer-unavailable"
                                        }))
                                        .two_line_secondary()
                                        .icon(Icon::Monitor),
                                    ));
                                }
                            }
                            body = body.push(Box::new(app_group));
                        }
                    }
                    if self.live
                        && j.details
                        && j.categories[1]
                        && self.belongings.flatpak.as_ref().is_some_and(|inventory| {
                            inventory.refs.iter().any(|item| {
                                item.app_id().is_some()
                                    && item.installation
                                        != super::super::flatpak_transfer::Installation::User
                            })
                        })
                    {
                        if let Some(notices) = &self.belongings.administrator_settings {
                            let affected: Vec<_> = notices
                                .iter()
                                .filter(|notice| !notice.domains().is_empty())
                                .collect();
                            if !affected.is_empty() {
                                let commands = self.commands.clone();
                                body = body.push(Box::new(
                                    ListRow::new(
                                        "installer.flatpak.access-details",
                                        t("installer-flatpak-settings-title"),
                                    )
                                    .secondary(t("installer-flatpak-settings-note"))
                                    .two_line_secondary()
                                    .disclosure()
                                    .on_activate(move || {
                                        commands.borrow_mut().push(
                                            InstallerPreviewEffect::ToggleFlatpakAccessDetails,
                                        )
                                    }),
                                ));
                            }
                            for (index, notice) in affected
                                .iter()
                                .enumerate()
                                .filter(|_| self.flatpak_access_details)
                            {
                                let settings = notice
                                    .domains()
                                    .into_iter()
                                    .map(|domain| t(domain.key()))
                                    .collect::<Vec<_>>()
                                    .join(", ");
                                body = body.push(Box::new(
                                    ListRow::new(
                                        format!("installer.flatpak.settings.{index}"),
                                        notice.installed.name.clone(),
                                    )
                                    .secondary(settings)
                                    .two_line_secondary(),
                                ));
                            }
                        } else {
                            body = body.push(Box::new(Text::new(&t(
                                "installer-flatpak-settings-unchecked",
                            ))));
                        }
                    }
                    if self.live
                        && self.belongings.browser.available()
                        && (j.details || j.browser_domains.iter().any(|included| *included))
                    {
                        if j.details {
                            let commands = self.commands.clone();
                            body =
                                body.push(self.belongings.browser.view(move |domain, included| {
                                    let category = match domain {
                                        browser::Domain::Bookmarks => "bookmarks",
                                        browser::Domain::History => "history",
                                    };
                                    commands.borrow_mut().push(
                                        InstallerPreviewEffect::SetPreviewCategory {
                                            category: category.into(),
                                            included,
                                        },
                                    );
                                }));
                        } else {
                            body = body.push(Box::new(
                                ListRow::new(
                                    "installer.browser.overview",
                                    t("installer-browser-title"),
                                )
                                .secondary(t(
                                    match (
                                        self.belongings.browser.chosen(browser::Domain::Bookmarks),
                                        self.belongings.browser.chosen(browser::Domain::History),
                                    ) {
                                        (true, false) => "installer-browser-bookmarks-body",
                                        (false, true) => "installer-browser-history-body",
                                        _ => "installer-browser-body",
                                    },
                                ))
                                .two_line_secondary()
                                .icon(Icon::Globe),
                            ));
                        }
                    }
                    if self.live && self.belongings.sessions.available() {
                        let commands = self.commands.clone();
                        if j.details {
                            body = body.push(self.belongings.sessions.view(
                                j.categories[2],
                                move |source_id, service, included| {
                                    commands.borrow_mut().push(
                                        InstallerPreviewEffect::SetPreviewServiceSession {
                                            source_id,
                                            service: service.as_str().into(),
                                            included,
                                        },
                                    );
                                },
                            ));
                        } else {
                            body = body.push(Box::new(
                                ListRow::new(
                                    "installer.sessions.overview",
                                    t("installer-service-sessions-title"),
                                )
                                .secondary(self.belongings.sessions.overview(j.categories[2]))
                                .two_line_secondary()
                                .icon(Icon::Globe)
                                .disclosure()
                                .on_activate(move || {
                                    commands
                                        .borrow_mut()
                                        .push(InstallerPreviewEffect::ReviewServiceSessions)
                                }),
                            ));
                        }
                    }
                    if self.live && j.details {
                        if j.categories[0] {
                            let profiles = self
                                .belongings
                                .accounts
                                .candidates
                                .iter()
                                .filter(|candidate| {
                                    !candidate.can_import_credentials
                                        && !accounts::can_connect_candidate(candidate)
                                })
                                .collect::<Vec<_>>();
                            if !profiles.is_empty() {
                                body = body
                                    .push(Box::new(
                                        Text::new(&t("installer-preview-profiles-title"))
                                            .with_role(TextRole::Title),
                                    ))
                                    .push(Box::new(Text::new(&t(
                                        "installer-preview-profiles-body",
                                    ))));
                                for candidate in profiles {
                                    body = body.push(Box::new(
                                        ListRow::new(
                                            format!("installer.profile.{}", candidate.id),
                                            &candidate.label,
                                        )
                                        .secondary(t("installer-preview-account-profile-only"))
                                        .two_line_secondary()
                                        .icon(Icon::Folder),
                                    ));
                                }
                            }
                        }
                        if j.categories[2] {
                            for account in &self.belongings.accounts.tilo_accounts {
                                body = body.push(Box::new(
                                    ListRow::new(
                                        format!("installer.account.{}", account.source_id),
                                        accounts::tilo_candidate_label(account),
                                    )
                                    .secondary(t("installer-preview-account-tilo"))
                                    .icon(Icon::Users),
                                ));
                            }
                            for candidate in
                                self.belongings
                                    .accounts
                                    .candidates
                                    .iter()
                                    .filter(|candidate| {
                                        candidate.can_import_credentials
                                            || accounts::can_connect_candidate(candidate)
                                    })
                            {
                                body = body.push(Box::new(
                                    ListRow::new(
                                        format!("installer.account.{}", candidate.id),
                                        accounts::candidate_label(candidate),
                                    )
                                    .secondary(t(if candidate.can_import_credentials {
                                        "installer-preview-account-importable"
                                    } else {
                                        "installer-preview-account-reconnect"
                                    }))
                                    .two_line_secondary()
                                    .icon(Icon::Users),
                                ));
                            }
                        }
                    }
                    body = body.push(Box::new(
                        self.button(
                            "installer.details",
                            if j.details {
                                "installer-preview-hide-details"
                            } else {
                                "installer-preview-choose-belongings"
                            },
                            InstallerPreviewEffect::TogglePreviewDetails,
                        )
                        .with_icon(if j.details {
                            Icon::ChevronUp
                        } else {
                            Icon::ChevronDown
                        })
                        .role(ButtonRole::Plain),
                    ));
                    if self.live {
                        let locations = self.portable_source.mounted_locations();
                        if !locations.is_empty() {
                            body = body
                                .push(Box::new(Text::new(&t("installer-mounted-home-locations"))));
                            for (index, location) in locations.into_iter().enumerate() {
                                body = body.push(Box::new(
                                    ListRow::new(
                                        format!("installer.mounted-location.{index}"),
                                        location,
                                    )
                                    .secondary(t("installer-mounted-home-location-unavailable"))
                                    .two_line_secondary()
                                    .icon(Icon::Folder),
                                ));
                            }
                        }
                    }
                    if self.live && self.belongings_worker.is_none() {
                        let warnings = self.belongings.warning_keys();
                        if !warnings.is_empty() {
                            body = body.push(Box::new(ui::widget::Banner::warning(
                                warnings.into_iter().map(t).collect::<Vec<_>>().join("\n"),
                            )));
                        }
                        body = body.push(Box::new(
                            self.button(
                                "installer.belongings.recheck",
                                "imager-setup-internal-recheck",
                                InstallerPreviewEffect::CheckBelongings,
                            )
                            .role(ButtonRole::Plain),
                        ));
                    }
                }
            }
            Page::Accounts => {
                let commands = self.commands.clone();
                let edits = self.commands.clone();
                body = body.push(Box::new(self.accounts.view(
                    Rc::new(move |request| {
                        let (step, selection) = request.parts();
                        let mut pending = commands.borrow_mut();
                        if !pending.iter().any(|effect| matches!(effect,
                            InstallerPreviewEffect::AccountStep { step: queued, selection: selected }
                                if queued == step && selected == &selection)) {
                            pending.push(InstallerPreviewEffect::AccountStep {
                                step: step.to_string(), selection,
                            });
                        }
                    }),
                    Rc::new(move |field, value| {
                        edits
                            .borrow_mut()
                            .push(InstallerPreviewEffect::SetAccountField {
                                field: field.to_string(),
                                value,
                            });
                    }),
                )));
            }
            Page::Storage => {
                let destination = j.destination.unwrap_or(Destination::Computer);
                if destination == Destination::Computer {
                    body = body.push(Box::new(self.storage_content()));
                } else if destination == Destination::Another {
                    body = body.push(Box::new(Text::new(&t("installer-preview-storage-another"))));
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
                let mut setup = page::related()
                    .push(Box::new(
                        Text::new(&t("installer-review-setup"))
                            .with_role(TextRole::Subheading)
                            .with_heading_level(2),
                    ))
                    .push(Box::new(self.review_identity_row()))
                    .push(Box::new(self.review_device_row()))
                    .push(Box::new(
                        self.review_row(
                            "destination",
                            t(&format!("installer-preview-{}-title", destination.key())),
                            t(
                                if destination == Destination::Computer
                                    && !self.direct_route_enabled()
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
                    // The compatibility decision is part of the reviewed plan,
                    // not a note at the bottom of it.
                    setup = setup.push(Box::new(
                        self.review_selection_row(
                            "target",
                            "destination",
                            self.target.review_title(),
                            self.target.review_summary(),
                        )
                        .icon(Icon::Monitor),
                    ));
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
                        Text::new(&t("installer-preview-deferred-handoff"))
                            .with_id("installer.review.handoff"),
                    ));
                }
                if let Some(state) = &self.source_state {
                    let message = match state {
                        imager_backend::setup::source_handoff::SourceState::Waiting => {
                            t("installer-source-handoff-waiting")
                        }
                        imager_backend::setup::source_handoff::SourceState::Requested {
                            label,
                            code,
                            approval: _,
                        } => t_args(
                            "installer-source-handoff-requested",
                            &[
                                ("name", label.as_str().into()),
                                ("code", code.as_str().into()),
                            ],
                        ),
                        imager_backend::setup::source_handoff::SourceState::JoinRequested {
                            label,
                            code,
                            approval: _,
                        } => t_args(
                            "installer-source-handoff-join-requested",
                            &[
                                ("name", label.as_str().into()),
                                ("code", code.as_str().into()),
                            ],
                        ),
                        imager_backend::setup::source_handoff::SourceState::Joining { label } => {
                            t_args(
                                "installer-source-handoff-joining",
                                &[("name", label.as_str().into())],
                            )
                        }
                        imager_backend::setup::source_handoff::SourceState::Sending {
                            label,
                            completed,
                            total,
                        } => t_args(
                            "installer-source-handoff-sending",
                            &[
                                ("name", label.as_str().into()),
                                ("done", ui::format::size(*completed).into()),
                                ("total", ui::format::size(*total).into()),
                            ],
                        ),
                        imager_backend::setup::source_handoff::SourceState::AwaitingReceipt {
                            label,
                        } => t_args(
                            "installer-source-handoff-receipt",
                            &[("name", label.as_str().into())],
                        ),
                        imager_backend::setup::source_handoff::SourceState::PackageImported {
                            label,
                        } => t_args(
                            "installer-source-handoff-complete",
                            &[("name", label.as_str().into())],
                        ),
                        imager_backend::setup::source_handoff::SourceState::Rejected => {
                            t("installer-source-handoff-rejected")
                        }
                        imager_backend::setup::source_handoff::SourceState::Failed => {
                            t("installer-source-handoff-unavailable")
                        }
                        imager_backend::setup::source_handoff::SourceState::Cancelled => {
                            t("installer-source-handoff-cancelled")
                        }
                        imager_backend::setup::source_handoff::SourceState::Expired => {
                            t("installer-source-handoff-expired")
                        }
                    };
                    setup = setup.push(Box::new(
                        Text::new(&message)
                            .with_id("installer.review.source-handoff")
                            .with_live(ui::a11y::Politeness::Polite),
                    ));
                    if let imager_backend::setup::source_handoff::SourceState::Requested {
                        approval,
                        ..
                    }
                    | imager_backend::setup::source_handoff::SourceState::JoinRequested {
                        approval,
                        ..
                    } = state
                    {
                        let commands = self.commands.clone();
                        let approval = approval.clone();
                        setup = setup.push(Box::new(
                            Button::new(
                                "installer.source-handoff.approve",
                                &t("installer-source-handoff-approve"),
                            )
                            .on_effect(move || {
                                commands.borrow_mut().push(
                                    InstallerPreviewEffect::ApproveSourceHandoff {
                                        approval: approval.clone(),
                                    },
                                );
                            }),
                        ));
                    }
                }
                if destination == Destination::Computer && self.direct_route_enabled() {
                    setup = setup.push(Box::new(self.review_row(
                        "storage",
                        t("installer-preview-storage-size-label"),
                        self.review_storage_summary(),
                    )));
                }
                let mut belongings = page::related()
                    .push(Box::new(
                        Text::new(&t("installer-review-belongings-heading"))
                            .with_role(TextRole::Subheading)
                            .with_heading_level(2),
                    ))
                    .push(Box::new(self.review_selection_row(
                        "migration",
                        "belongings",
                        t(if j.migrate == Some(true) {
                            "installer-preview-migrate-title"
                        } else {
                            "installer-preview-fresh-title"
                        }),
                        t(
                            if j.migrate == Some(true)
                                && j.categories[0]
                                && j.destination == Some(Destination::Computer)
                            {
                                "installer-preview-review-shared"
                            } else if j.migrate == Some(true)
                                && j.destination == Some(Destination::Desktop)
                            {
                                "installer-desktop-review-migrate"
                            } else if j.migrate == Some(true) {
                                "installer-preview-review-migrate"
                            } else {
                                "installer-preview-review-fresh"
                            },
                        ),
                    )));
                if j.migrate == Some(true) {
                    for (index, key) in CATEGORIES.iter().enumerate() {
                        // Accounts also controls website data. Category consent
                        // alone does not mean any cloud connection was prepared.
                        if j.categories[index]
                            && (index >= 2 || j.destination != Some(Destination::Desktop))
                            && (index != 2 || self.accounts.has_prepared_connections())
                        {
                            let title =
                                if index == 0 && j.destination == Some(Destination::Computer) {
                                    t("installer-preview-files-shared-title")
                                } else {
                                    t(&format!("installer-preview-{key}-title"))
                                };
                            belongings = belongings.push(Box::new(self.review_selection_row(
                                key,
                                "belongings-details",
                                title,
                                String::new(),
                            )));
                        }
                    }
                    if self.live {
                        let selected = self.belongings.browser.selected();
                        for (id, key, bookmarks) in [
                            ("bookmarks", "installer-browser-bookmarks", true),
                            ("history", "installer-browser-history", false),
                        ] {
                            let labels = selected
                                .profiles()
                                .iter()
                                .filter(|(_, profile)| {
                                    if bookmarks {
                                        profile.bookmarks
                                    } else {
                                        profile.history
                                    }
                                })
                                .map(|(source, _)| review_source_label(&source.label))
                                .collect::<Vec<_>>();
                            if !labels.is_empty() {
                                belongings = belongings.push(Box::new(self.review_selection_row(
                                    id,
                                    "belongings-details",
                                    t(key),
                                    labels.join(", "),
                                )));
                            }
                        }
                    }
                    #[cfg(target_os = "linux")]
                    if self.live {
                        for (index, label) in self
                            .belongings
                            .sessions
                            .review_labels(j.categories[2])
                            .into_iter()
                            .enumerate()
                        {
                            belongings = belongings.push(Box::new(self.review_selection_row(
                                &format!("session.{index}"),
                                "belongings-details",
                                label,
                                t("installer-review-service-session-body"),
                            )));
                        }
                    }
                }
                body = body
                    .with_rhythm(Rhythm::Group)
                    .push(Box::new(setup))
                    .push(Box::new(belongings));
            }
            Page::Ready => {
                body = body.push(Box::new(IconWidget::new(Icon::Check).accent()));
            }
        }
        body
    }

    pub(super) fn restart_view(&self) -> Box<dyn Widget> {
        let mut body = page::stack()
            .push(Box::new(
                Text::new(&t(if self.restart.state() == "stale" {
                    "installer-restart-changed-heading"
                } else {
                    "installer-restart-heading"
                }))
                .with_role(TextRole::LargeTitle)
                .with_heading_level(1),
            ))
            .push(Box::new(Text::new(&t(match self.restart.state() {
                "checking" => "installer-restart-checking",
                "pending" => "installer-restart-incomplete",
                "unavailable" => "installer-restart-unavailable",
                "stale" => "installer-restart-changed",
                "resuming" => "installer-restart-resuming",
                "retiring" => "installer-restart-retiring",
                _ => "installer-restart-intro",
            }))));
        if !self.restart.busy() {
            if matches!(self.restart.state(), "unavailable" | "stale") {
                body = body.push(Box::new(
                    self.button(
                        "installer.restart.retry",
                        if self.restart.state() == "stale" {
                            "installer-restart-review-latest"
                        } else {
                            "installer-restart-retry"
                        },
                        InstallerPreviewEffect::RetryInstallerPreparation,
                    )
                    .role(ButtonRole::Primary),
                ));
            } else {
                body = body
                    .push(Box::new(self.button(
                        "installer.restart.resume",
                        "installer-restart-resume",
                        InstallerPreviewEffect::ResumeInstallerPreparation,
                    )))
                    .push(Box::new(
                        self.button(
                            "installer.restart.new",
                            "installer-restart-new",
                            InstallerPreviewEffect::StartNewInstallerPreparation,
                        )
                        .role(ButtonRole::Plain),
                    ));
            }
        }
        Box::new(
            Scrollable::new_vertical()
                .with_cursor(self.cursor)
                .with_focus_reveal()
                .with_child(Box::new(
                    Container::new()
                        .with_inset(Rhythm::Group)
                        .readable()
                        .centered()
                        .with_child(Box::new(body)),
                )),
        )
    }

    pub(super) fn composition(&self, theme: &Theme) -> Box<dyn Widget> {
        if self.windows.enabled && self.live {
            return if self.journey.destination == Some(Destination::Another) {
                self.windows_view()
            } else {
                self.windows_destination_view()
            };
        }
        if self.live && self.restart.blocks() {
            return self.restart_view();
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
            && self.journey.page == Page::Accounts
            && let Some((id, label, request)) = self.accounts.navigation_action()
        {
            let (step, selection) = request.parts();
            actions = actions.push(Box::new(
                self.button(
                    id,
                    label,
                    InstallerPreviewEffect::AccountStep {
                        step: step.into(),
                        selection,
                    },
                )
                .role(ButtonRole::Primary),
            ));
        }
        if self.media_preparation_admitted() {
            actions = actions.push(Box::new(self.button(
                "installer.cancel-preparation",
                "installer-media-cancel-preparation",
                InstallerPreviewEffect::CancelMediaPreparation,
            )));
        }
        let changed_source = self.live
            && self.journey.page == Page::Review
            && !self.journey.editing_destination()
            && self.media_preparation.needs_source_review();
        if changed_source {
            actions = actions.push(Box::new(
                self.button(
                    "installer.review-changed-files",
                    self.media_preparation.review_reason().map_or(
                        "installer-source-review",
                        prepared_media::ReturnReason::button_key,
                    ),
                    InstallerPreviewEffect::ReviewChangedFiles,
                )
                .role(ButtonRole::Primary),
            ));
        }
        if !changed_source
            && self.live
            && self.journey.page == Page::Review
            && !self.journey.editing_destination()
            && self.usb_backend_available()
            && matches!(
                self.journey.destination,
                Some(Destination::Computer | Destination::Another)
            )
        {
            actions = actions.push(Box::new(
                self.button(
                    "installer.prepare-usb",
                    "installer-preview-prepare-usb",
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
                .with_enabled(!self.media_preparation.busy() && !self.personal_busy()),
            ));
        }
        let (label, effect) = match self.journey.page {
            Page::Password if self.personal_busy() => (
                "installer-preview-identity-preparing",
                InstallerPreviewEffect::ContinuePreview,
            ),
            Page::Accounts => (
                self.accounts.continue_label(),
                InstallerPreviewEffect::ContinuePreview,
            ),
            _ if self.journey.saving_edit() => (
                "installer-preview-save",
                InstallerPreviewEffect::ContinuePreview,
            ),
            Page::Belongings | Page::Review if self.live => (
                "installer-preview-continue",
                InstallerPreviewEffect::ContinuePreview,
            ),
            Page::Ready => (
                "installer-preview-restart",
                InstallerPreviewEffect::RestartPreview,
            ),
            Page::Review => (
                "installer-preview-finish",
                InstallerPreviewEffect::ContinuePreview,
            ),
            Page::Belongings if self.journey.details => (
                "installer-preview-bring-selected",
                InstallerPreviewEffect::ContinuePreview,
            ),
            Page::Belongings => (
                "installer-preview-bring-things",
                InstallerPreviewEffect::ContinuePreview,
            ),
            _ => (
                "installer-preview-continue",
                InstallerPreviewEffect::ContinuePreview,
            ),
        };
        let chooses_path = matches!(
            self.journey.page,
            Page::Destination | Page::Migration | Page::Identity | Page::Connect
        ) && !(self.journey.page == Page::Destination
            && self.journey.editing_destination())
            && !(self.live
                && self.journey.page == Page::Connect
                && matches!(self.journey.transfer.as_deref(), Some("backup" | "current")));
        let chooses_path = chooses_path
            && !(self.live
                && self.journey.page == Page::Connect
                && self.journey.transfer.as_deref() == Some("device")
                && self.pairing.ready());
        let accounts_waiting = self.journey.page == Page::Accounts && !self.accounts.can_continue();
        let mounted_step = self.live
            && self.journey.page == Page::Belongings
            && self.journey.destination == Some(Destination::Another)
            && self.journey.categories[0]
            && self.portable_source.mounted_permission_active();
        if !chooses_path
            && !self.sessions_review
            && !mounted_step
            && !accounts_waiting
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
        // Keep navigation at one position while a page's body, errors and
        // accessible text scroll independently. Personal forms retain their
        // bounded measure inside the same readable-width page frame.
        if matches!(
            self.journey.page,
            Page::Profile | Page::Password | Page::Devices
        ) {
            content = page::stack().push(Box::new(
                Container::new()
                    .with_no_padding()
                    .with_centered_max_width(theme.tokens.metrics.content_min_width)
                    .with_child(Box::new(content)),
            ));
        }
        let frame = page::Frame::new(content).with_cursor(self.cursor);
        let frame = if self.journey.page == Page::Devices {
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
                    ui::widget::Banner::info(t("installer-demo-notice"))
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
