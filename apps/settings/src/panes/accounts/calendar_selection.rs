//! Discovery is a review step, not an implicit subscription to every calendar.

#[cfg(test)]
mod queue_tests;
#[cfg(test)]
mod tests;
use super::{
    AccountKind, AccountsEffect, AccountsPane, Button, LiveRow, ProbeJob, Row, Settings,
    SettingsSection, Step, ViewCx, merge_config, note, row, rows, section, setting_row_described,
    t, value_row,
};
use ui::tilo_resource;

/// The calendars an account offers, as the pane publishes it: a row of the
/// `settings.calendar-choices` type, holding each calendar beneath it.
#[derive(ui::tilo_resource::Row)]
#[row(kind = "settings.calendar-choices", label = "Calendar choices",
    effects = AccountsEffect
)]
pub(super) struct CalendarChoices;

/// One calendar an account offers, as the pane publishes it: a row of the
/// `settings.calendar-choice` type, offering to subscribe or not.
#[derive(ui::tilo_resource::Row)]
#[row(kind = "settings.calendar-choice", label = "Calendar choice",
    effects = AccountsEffect
)]
pub(super) struct CalendarChoiceRow;

impl AccountsPane {
    pub fn connect_nextcloud_calendars(&mut self, id: &str) -> Result<(), String> {
        let account = self
            .account_root
            .store()
            .account(id)
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
        // An account whose sign-in is the workspace's vault's has none on this device: the
        // session releases it to Settings for the calendars to be found with, and the device's
        // vault keeps nothing of it, before or after.
        if let Some(item) = accounts::email::password_item(&account.config).map(str::to_owned) {
            use ui::collaboration::browser_service_sessions::{
                BrowserServiceSessionCommand as Command, BrowserServiceSessionReply as Reply,
            };
            let asked = self
                .session_secrets
                .as_ref()
                .ok_or_else(|| t("accounts-no-session"))
                .and_then(|client| {
                    client.request(Command::ReleaseAccountSecret { item: item.clone() }, None)
                });
            let pending = match asked {
                Ok(pending) => pending,
                Err(error) => {
                    self.form_saving = false;
                    self.form_error = Some(error);
                    return Ok(());
                }
            };
            self.run_in_queue(move |_| {
                Step::vault(
                    move |_| pending.wait(),
                    move |pane, reply| {
                        let secret = match reply {
                            Ok(Reply::AccountSecret {
                                item: released,
                                secret,
                            }) if released == item => {
                                Ok((None, accounts::Secret::password(secret.as_str())))
                            }
                            Ok(_) => Err("The session answered something else".to_owned()),
                            Err(error) => Err(error),
                        };
                        pane.discover_nextcloud_calendars(round, account, secret)
                    },
                )
            });
            self.changed = true;
            return Ok(());
        }
        let kept = account.clone();
        self.run_in_queue(move |_| {
            Step::vault(
                move |vault| {
                    Self::prepare_secret(&mut vault.open(), Some(&kept), &kept, "")
                        .map(|secret| (Some(secret.clone()), secret))
                },
                move |pane, secret| pane.discover_nextcloud_calendars(round, account, secret),
            )
        });
        self.changed = true;
        Ok(())
    }

    /// Find the calendars `account` offers, signing in with `secret`: the one this device's vault
    /// keeps for it (then also the baseline a save checks the vault against), or the one the
    /// workspace's vault released, which this device's vault never keeps.
    fn discover_nextcloud_calendars(
        &mut self,
        round: u64,
        account: accounts::Account,
        secret: Result<(Option<accounts::Secret>, accounts::Secret), String>,
    ) -> Step {
        if self.form_round != round {
            return Step::Done;
        }
        self.form_saving = false;
        match secret {
            Ok((kept, secret))
                if !secret.password.is_empty()
                    && self.account_root.store().account(account.id.as_str())
                        == Some(account.clone()) =>
            {
                self.calendar_baseline = Some((Some(account.clone()), kept));
                let tester = std::sync::Arc::clone(&self.account_tester);
                let (sender, receiver) = crate::channel::sync_channel(1);
                let _ = crate::device_read::thread(
                    "settings-calendar-discovery",
                    Box::new(move || {
                        let _ = sender.send(tester.discover_calendars(&account, &secret));
                    }),
                );
                self.form_probe = Some(ProbeJob::new(receiver));
            }
            Ok(_) => self.form_error = Some(t("accounts-calendar-details-changed")),
            Err(error) => self.form_error = Some(error),
        }
        self.changed = true;
        Step::Done
    }

    pub(super) fn calendar_choice_rows(&self) -> Vec<LiveRow> {
        if self.calendar_choices.is_empty() {
            return Vec::new();
        }
        let bindings = ui::tilo_resource::RowBindings::new()
            .offer(AccountsEffect::INSPECT_CALENDAR_CHOICES)
            .offer(AccountsEffect::SAVE_ACCOUNT_FORM)
            .offer(AccountsEffect::CANCEL_ACCOUNT_FORM);
        let mut root = LiveRow::typed_with::<CalendarChoices>(
            "settings.calendar-choices",
            t("accounts-calendars-title"),
            &bindings,
        );
        for (calendar, selected) in &self.calendar_choices {
            let bindings = ui::tilo_resource::RowBindings::new().bind(
                AccountsEffect::SELECT_CALENDAR,
                [
                    ("href", calendar.href.clone().into()),
                    ("selected", (!selected).into()),
                ],
            );
            root = root.row(LiveRow::typed_with::<CalendarChoiceRow>(
                format!("settings.calendar-choice:{}", calendar.href),
                calendar.name.clone(),
                &bindings,
            ));
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
            .and_then(|id| store.account(id))
            .and_then(|account| {
                account
                    .config
                    .get("calendars")
                    .and_then(serde_json::Value::as_array)
                    .cloned()
            });
        self.calendar_choices = calendars
            .into_iter()
            .map(|calendar| {
                // No selection yet means a new setup, not an explicit opt-out.
                // An existing list (even an empty one) is authoritative.
                let selected = previous.as_ref().is_none_or(|values| {
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

    pub(super) fn calendar_selection_form(&self, cx: &ViewCx<Settings>) -> SettingsSection {
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
            let button = Self::bound(
                cx,
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
                .push(Box::new(Self::bound(
                    cx,
                    Button::new("settings.accounts.form.save", &t("accounts-save"))
                        .with_enabled(self.calendar_choices.iter().any(|(_, selected)| *selected)),
                    AccountsEffect::SaveAccountForm,
                )))
                .push(Box::new(Self::bound(
                    cx,
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
        self.run_in_queue(move |pane| {
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

    fn commit_calendar_then(
        &mut self,
        account: accounts::Account,
        secret: Option<accounts::Secret>,
        round: u64,
    ) -> Step {
        // A calendar account's password is the workspace's vault's, as an email account's: it
        // goes there through the session, and this device's vault keeps nothing of it. An edit
        // that typed none keeps the entry the account already names.
        let secret = secret.filter(|secret| !secret.password.is_empty());
        self.commit_then(
            vec![(account, secret)],
            Box::new(move |pane, saved| pane.form_saved(round, saved)),
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
                .account(source.id.as_str())
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
        if self.account_root.store().account(id.as_str()).as_ref() != original.as_ref() {
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

    /// Read the credential only in the serialized vault queue. The metadata,
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
