//! Discovery is a review step, not an implicit subscription to every calendar.

#[cfg(test)]
mod lane_tests;
#[cfg(test)]
mod tests;
use super::{
    AccountKind, AccountsEffect, AccountsPane, Button, Effect, LiveRow, ProbeJob, Row,
    SettingsEffect, SettingsSection, Step, merge_config, note, row, rows, section,
    setting_row_described, t, value_row,
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
        self.cancel_form();
        self.next_form_round();
        self.form_kind = Some(AccountKind::CalDav);
        self.form_account_id = Some(id.to_owned());
        self.calendar_source = Some(account.clone());
        self.form_error = Some(t("accounts-checking"));
        self.form_saving = true;
        let round = self.form_round;
        let id = account.id.clone();
        self.run_in_lane(move |_| {
            Step::vault(
                move |vault| Self::prepare_secret(&vault.open(), &id, ""),
                move |pane, secret| {
                    if pane.form_round != round {
                        return Step::Done;
                    }
                    pane.form_saving = false;
                    match secret {
                        Ok(secret)
                            if !secret.password.is_empty()
                                && pane.account_root.store().get(&account.id) == Some(&account) =>
                        {
                            pane.calendar_baseline =
                                Some((Some(account.clone()), Some(secret.clone())));
                            let tester = std::sync::Arc::clone(&pane.account_tester);
                            let (sender, receiver) = crate::channel::sync_channel(1);
                            let _ = crate::device_read::thread(
                                "settings-calendar-discovery",
                                Box::new(move || {
                                    let _ =
                                        sender.send(tester.discover_calendars(&account, &secret));
                                }),
                            );
                            pane.form_probe = Some(ProbeJob::new(receiver));
                        }
                        Ok(_) => pane.form_error = Some(t("accounts-calendar-details-changed")),
                        Err(error) => pane.form_error = Some(error),
                    }
                    pane.changed = true;
                    Step::Done
                },
            )
        });
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
            "checking": self.form_probe.is_some() || self.form_saving,
            "error": if self.form_probe.is_none() && !self.form_saving { self.form_error.as_ref() } else { None },
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
        section(&t("accounts-calendars-title"), rows(body))
    }

    pub(super) fn save_calendar_selection(&mut self) {
        if self.form_saving {
            return;
        }
        if let Err(error) = self.validate_calendar_review() {
            self.form_error = Some(error);
            self.changed = true;
            return;
        }
        self.form_saving = true;
        let round = self.form_round;
        self.run_in_lane(move |pane| {
            pane.check_calendar_credential_then(Box::new(move |pane, checked| {
                if pane.form_round != round {
                    return Step::Done;
                }
                let prepared = checked.and_then(|()| pane.prepared_calendar_selection());
                match prepared {
                    Ok((account, secret)) => pane.commit_calendar_then(account, secret, round),
                    Err(error) => pane.form_saved(round, Err(error)),
                }
            }))
        });
        self.changed = true;
    }

    /// A typed credential takes another worker turn to seal. Recheck the
    /// review before publishing its metadata, and roll the seal back if the
    /// person canceled or changed the account while that worker ran.
    fn commit_calendar_then(
        &mut self,
        account: accounts::Account,
        secret: Option<accounts::Secret>,
        round: u64,
    ) -> Step {
        let Some(secret) = secret else {
            return self.commit_then(
                vec![(account, None)],
                Box::new(move |pane, saved| pane.form_saved(round, saved)),
            );
        };
        let root = &self.account_root;
        if let Err(error) = root.begin_mutation(&root.store(), &root.statuses()) {
            return self.form_saved(round, Err(error));
        }
        let expected = self
            .calendar_baseline
            .as_ref()
            .and_then(|(_, secret)| secret.clone());
        let id = account.id.clone();
        Step::vault(
            move |opener| {
                let mut vault = opener.open();
                if vault.recovery_error().is_some() || vault.get(&id) != expected {
                    return Err(t("accounts-calendar-details-changed"));
                }
                vault.put(&id, &secret);
                vault
                    .try_save()
                    .map_err(|error| format!("could not save credential: {error}"))?;
                Ok((id, expected))
            },
            move |pane, sealed| {
                let (id, previous) = match sealed {
                    Ok(previous) => previous,
                    Err(error) => {
                        let _ = pane.account_root.finish_mutation();
                        return pane.form_saved(round, Err(error));
                    }
                };
                if round == pane.form_round && pane.validate_calendar_review().is_ok() {
                    return pane.commit_metadata(
                        vec![account],
                        vec![(id, previous)],
                        Box::new(move |pane, saved| pane.form_saved(round, saved)),
                    );
                }
                Step::vault(
                    move |vault| super::restore_secret(vault.open(), &id, previous.as_ref()),
                    move |pane, restored| {
                        if restored.is_ok() {
                            let _ = pane.account_root.finish_mutation();
                        }
                        pane.form_saved(round, Err(t("accounts-calendar-details-changed")))
                    },
                )
            },
        )
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
            return Ok((account, None));
        }
        let draft = self
            .form_candidate
            .as_ref()
            .ok_or("Calendar setup has no pending draft")?;
        let account = Self::prepare_metadata(&self.account_root, draft)?;
        let mut secret = self
            .calendar_baseline
            .as_ref()
            .and_then(|(_, secret)| secret.clone())
            .unwrap_or_default();
        if !draft.password.trim().is_empty() {
            draft.password.trim().clone_into(&mut secret.password);
        }
        Ok((
            account,
            (!draft.password.trim().is_empty()).then_some(secret),
        ))
    }

    pub(super) fn validate_calendar_review(&self) -> Result<(), String> {
        let (original, _) = self
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
        if self.account_root.store().get(id) != original.as_ref() {
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

    /// Read the credential only in the serialized vault lane. The metadata,
    /// form round and draft are checked again when that read lands.
    pub(super) fn check_calendar_credential_then(&mut self, settle: super::Settle) -> Step {
        if let Err(error) = self.validate_calendar_review() {
            return settle(self, Err(error));
        }
        let Some((original, credential)) = self.calendar_baseline.clone() else {
            return settle(self, Err(t("accounts-calendar-details-changed")));
        };
        let Some(id) = original
            .as_ref()
            .map(|account| account.id.clone())
            .or_else(|| {
                self.form_candidate
                    .as_ref()
                    .and_then(|draft| draft.assigned_id.clone())
            })
        else {
            return settle(self, Err(t("accounts-calendar-details-changed")));
        };
        let round = self.form_round;
        Step::vault(
            move |vault| {
                let vault = vault.open();
                vault.recovery_error().is_none() && vault.get(&id) == credential
            },
            move |pane, unchanged| {
                let checked = if unchanged && pane.form_round == round {
                    pane.validate_calendar_review()
                } else {
                    Err(t("accounts-calendar-details-changed"))
                };
                settle(pane, checked)
            },
        )
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
