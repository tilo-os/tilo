//! Discovery is a review step, not an implicit subscription to every calendar.

#[cfg(test)]
mod tests;
use super::{
    AccountKind, AccountsEffect, AccountsPane, Button, Effect, LiveRow, ProbeJob, Row,
    SettingsEffect, SettingsSection, card, merge_config, note, row, rows, setting_row_described, t,
    value_row,
};

impl AccountsPane {
    /// Start a calendar review using an existing Nextcloud account's canonical
    /// ID and credential. No account or secret is created or copied.
    ///
    /// # Errors
    /// If the account or its saved sign-in is unavailable.
    pub fn connect_nextcloud_calendars(&mut self, id: &str) -> Result<(), String> {
        let account = self
            .account_root
            .store()
            .get(&accounts::AccountId::from(id))
            .cloned()
            .filter(|account| account.type_id.as_str() == "nextcloud" && account.enabled)
            .ok_or("This Nextcloud account is unavailable or disabled")?;
        let secret = self
            .account_root
            .vault()
            .get(&account.id)
            .filter(|secret| !secret.password.is_empty())
            .ok_or("Sign in to this Nextcloud account before connecting calendars")?;
        self.cancel_form();
        self.form_kind = Some(AccountKind::CalDav);
        self.form_account_id = Some(id.to_owned());
        self.calendar_source = Some(account.clone());
        self.calendar_baseline = Some((Some(account.clone()), Some(secret.clone())));
        self.form_error = Some(t("accounts-checking"));
        let tester = std::sync::Arc::clone(&self.account_tester);
        let (sender, receiver) = std::sync::mpsc::sync_channel(1);
        std::thread::spawn(move || {
            let _ = sender.send(tester.discover_calendars(&account, &secret));
        });
        self.form_probe = Some(ProbeJob::new(receiver));
        self.changed = true;
        Ok(())
    }

    pub(super) fn calendar_choice_rows(&self) -> Vec<LiveRow> {
        if self.calendar_choices.is_empty() {
            return Vec::new();
        }
        let mut root = LiveRow::new(
            "settings.calendar-choices",
            "settings.calendar-choices",
            t("accounts-calendars-title"),
        )
        .operation(SettingsEffect::from(AccountsEffect::InspectCalendarChoices).bound())
        .operation(SettingsEffect::from(AccountsEffect::SaveAccountForm).bound())
        .operation(SettingsEffect::from(AccountsEffect::CancelAccountForm).bound());
        for (calendar, selected) in &self.calendar_choices {
            root = root.row(
                LiveRow::new(
                    format!("settings.calendar-choice:{}", calendar.href),
                    "settings.calendar-choice",
                    calendar.name.clone(),
                )
                .operation(
                    SettingsEffect::from(AccountsEffect::SelectCalendar {
                        href: calendar.href.clone(),
                        selected: !selected,
                    })
                    .bound(),
                ),
            );
        }
        vec![root]
    }

    pub(super) fn calendar_choice_summary(&self) -> serde_json::Value {
        let calendars: Vec<_> = self
            .calendar_choices
            .iter()
            .map(|(calendar, selected)| {
                serde_json::json!({
                    "href": calendar.href,
                    "name": calendar.name,
                    "read_only": calendar.read_only,
                    "selected": selected,
                })
            })
            .collect();
        serde_json::json!({
            "account": self.form_account_id,
            "checking": self.form_probe.is_some(),
            "error": if self.form_probe.is_none() { self.form_error.as_ref() } else { None },
            "calendars": calendars,
        })
    }

    pub(super) fn stage_calendar_choices(
        &mut self,
        calendars: Vec<tilo_dav::discovery::CalendarCollection>,
    ) -> Result<(), String> {
        self.validate_calendar_review()?;
        let store = self.account_root.store();
        let previous = self
            .form_account_id
            .as_ref()
            .and_then(|id| store.get(&accounts::AccountId::from(id.clone())))
            .and_then(|account| account.config.get("calendars"))
            .and_then(serde_json::Value::as_array);
        self.calendar_choices = calendars
            .into_iter()
            .map(|calendar| {
                // No selection yet means a new setup, not an explicit opt-out.
                // An existing list (even an empty one) is authoritative.
                let selected = previous.is_none_or(|values| {
                    values.iter().any(|value| {
                        value.get("id").and_then(serde_json::Value::as_str)
                            == Some(calendar.href.as_str())
                    })
                });
                (calendar, selected)
            })
            .collect();
        Ok(())
    }

    /// Choose one collection from the current discovery result, as the
    /// published Settings action and the shared controls do.
    ///
    /// # Errors
    /// If the collection is not in this form's current discovery result.
    pub fn select_calendar(&mut self, href: &str, selected: bool) -> Result<(), String> {
        let choice = self
            .calendar_choices
            .iter_mut()
            .find(|(calendar, _)| calendar.href == href)
            .ok_or("This calendar is not in the current discovery result")?;
        choice.1 = selected;
        self.changed = true;
        Ok(())
    }

    pub(super) fn calendar_selection_form(&self) -> SettingsSection {
        let mut body = vec![note(&t(if self.calendar_source.is_some() {
            "accounts-nextcloud-calendar-caption"
        } else {
            "accounts-calendars-caption"
        }))];
        if let Some(draft) = &self.form_candidate {
            body.push(value_row(&t("accounts-display-name"), &draft.name));
            body.push(value_row(&t("accounts-server"), &draft.server));
        }
        if let Some(account) = &self.calendar_source {
            body.push(value_row(
                &t("accounts-display-name"),
                &account.display_name,
            ));
            if let Some(server) = account
                .config
                .get("server_url")
                .and_then(serde_json::Value::as_str)
            {
                body.push(value_row(&t("accounts-server"), server));
            }
        }
        for (calendar, selected) in &self.calendar_choices {
            let label = t(if *selected {
                "accounts-calendar-deselect"
            } else {
                "accounts-calendar-select"
            });
            let button = self.bound(
                Button::new(
                    &format!("settings.accounts.calendar:{}", calendar.href),
                    &label,
                ),
                AccountsEffect::SelectCalendar {
                    href: calendar.href.clone(),
                    selected: !selected,
                },
            );
            let description = if calendar.read_only {
                t("accounts-calendar-read-only")
            } else {
                calendar.href.clone()
            };
            body.push(setting_row_described(
                &calendar.name,
                &description,
                Box::new(button),
            ));
        }
        if let Some(error) = &self.form_error {
            body.push(note(error));
        }
        body.push(row(Box::new(
            Row::new()
                .push(Box::new(
                    self.bound(
                        Button::new("settings.accounts.form.save", &t("accounts-save"))
                            .with_enabled(
                                self.calendar_choices.iter().any(|(_, selected)| *selected),
                            ),
                        AccountsEffect::SaveAccountForm,
                    ),
                ))
                .push(Box::new(self.bound(
                    Button::new("settings.accounts.form.cancel", &t("accounts-cancel")),
                    AccountsEffect::CancelAccountForm,
                ))),
        )));
        card(&t("accounts-calendars-title"), rows(body))
    }

    pub(super) fn save_calendar_selection(&mut self) {
        let result = self
            .prepared_calendar_selection()
            .and_then(|(account, secret)| {
                Self::commit_account(&self.account_root, account, secret.as_ref())
            });
        match result {
            Ok(()) => {
                self.cancel_form();
                self.reload();
            }
            Err(error) => self.form_error = Some(error),
        }
        self.changed = true;
    }

    fn calendar_base_account(
        &self,
    ) -> Result<(accounts::Account, Option<accounts::Secret>), String> {
        self.validate_calendar_review()?;
        if let Some(source) = &self.calendar_source {
            let account = self
                .account_root
                .store()
                .get(&source.id)
                .cloned()
                .ok_or("The Nextcloud account was removed")?;
            if account.type_id != source.type_id
                || !account.enabled
                || account.config.get("server_url") != source.config.get("server_url")
                || account.config.get("username") != source.config.get("username")
            {
                return Err(t("accounts-calendar-details-changed"));
            }
            if self
                .account_root
                .vault()
                .get(&source.id)
                .is_none_or(|secret| secret.password.is_empty())
            {
                return Err("Sign in to this Nextcloud account before connecting calendars".into());
            }
            return Ok((account, None));
        }
        let draft = self
            .form_candidate
            .as_ref()
            .ok_or("Calendar setup has no pending draft")?;
        let (account, secret) = Self::prepare_account(&self.account_root, draft)?;
        Ok((
            account,
            (!draft.password.trim().is_empty()).then_some(secret),
        ))
    }

    pub(super) fn validate_calendar_review(&self) -> Result<(), String> {
        let (original, credential) = self
            .calendar_baseline
            .as_ref()
            .ok_or_else(|| t("accounts-calendar-details-changed"))?;
        let id = self
            .calendar_source
            .as_ref()
            .map(|source| &source.id)
            .or_else(|| {
                self.form_candidate
                    .as_ref()
                    .and_then(|draft| draft.assigned_id.as_ref())
            })
            .ok_or_else(|| t("accounts-calendar-details-changed"))?;
        if self.account_root.store().get(id) != original.as_ref()
            || self.account_root.vault().get(id).as_ref() != credential.as_ref()
        {
            return Err(t("accounts-calendar-details-changed"));
        }
        if let Some(draft) = &self.form_candidate
            && (self.form_account_id != draft.existing_id
                || self.form_kind != Some(draft.kind)
                || self.form_name.with(|value| value != &draft.name)
                || self.form_user.with(|value| value != &draft.username)
                || self.form_server.with(|value| value != &draft.server)
                || self.form_password.with(|value| value != &draft.password))
        {
            return Err(t("accounts-calendar-details-changed"));
        }
        Ok(())
    }

    fn prepared_calendar_selection(
        &self,
    ) -> Result<(accounts::Account, Option<accounts::Secret>), String> {
        let (mut account, secret) = self.calendar_base_account()?;
        let old = account
            .config
            .get("calendars")
            .and_then(serde_json::Value::as_array);
        let calendars: Vec<_> = self
            .calendar_choices
            .iter()
            .filter(|(_, selected)| *selected)
            .enumerate()
            .map(|(index, (calendar, _))| {
                let mut value = old
                    .and_then(|values| {
                        values.iter().find(|value| {
                            value.get("id").and_then(serde_json::Value::as_str)
                                == Some(calendar.href.as_str())
                        })
                    })
                    .and_then(serde_json::Value::as_object)
                    .cloned()
                    .unwrap_or_default();
                value.entry("color").or_insert_with(|| (index % 8).into());
                value.entry("visible").or_insert(true.into());
                value.insert("id".into(), calendar.href.clone().into());
                value.insert("name".into(), calendar.name.clone().into());
                value.insert("read_only".into(), calendar.read_only.into());
                serde_json::Value::Object(value)
            })
            .collect();
        if calendars.is_empty() {
            return Err(t("accounts-calendars-empty"));
        }
        merge_config(
            &mut account.config,
            serde_json::json!({"calendars": calendars}),
        );
        if self.calendar_source.is_some() {
            merge_config(
                &mut account.config,
                serde_json::json!({"use_calendar": true}),
            );
        }
        Ok((account, secret))
    }
}
