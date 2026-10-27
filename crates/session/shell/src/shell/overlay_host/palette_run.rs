//! Palette search filters, running results, parameter steps and secondary actions.

#[allow(
    clippy::wildcard_imports,
    reason = "private implementation module shares its enclosing host types"
)]
use super::*;

impl ShellOverlayHost {
    /// Where the filter band sits among the palette's semantic children:
    /// appended after whatever the palette itself publishes (its query
    /// field and result list), and only while the palette is open —
    /// a closed palette publishes no children and then neither does the
    /// band. Computed from the palette's own tree rather than pinned to a
    /// literal, so the two can never disagree if the palette grows a child.
    pub(super) fn search_filters_child_index(&self) -> Option<usize> {
        if !self.palette.filter_band_shown() || self.palette.shows_field_only() {
            return None;
        }
        let palette = self.palette.accessibility()?;
        (!palette.children.is_empty()).then_some(palette.children.len())
    }

    /// The band's inputs as this host shows them now: the registered kinds
    /// with their labels, the chosen kinds, and the scopes with theirs.
    pub(super) fn search_filter_inputs(&self) -> SearchFilterInputs {
        // The kinds the results hold, most hits first, and any kind chosen
        // already: never a kind with nothing to show. The scope is not a
        // control here; it is the token at the start of the field.
        let mut present = self.present_kinds.clone();
        for kind in &self.active_kind_filter {
            if !present.contains(kind) {
                present.push(kind.clone());
            }
        }
        let kinds = present
            .into_iter()
            .map(|kind| {
                let label = kind.localized_label();
                (kind, label)
            })
            .collect();
        let scopes = Vec::new();
        let active_scope = self
            .chrome
            .as_ref()
            .map(|c| c.borrow().search_scope)
            .unwrap_or_default();
        SearchFilterInputs {
            kinds,
            selected: self.active_kind_filter.clone(),
            scopes,
            active_scope,
        }
    }

    /// Hand `inputs` to `filters` with this host's accessible names.
    pub(super) fn configure_search_filters(
        filters: &mut SearchFilters,
        inputs: &SearchFilterInputs,
    ) {
        let kinds = inputs
            .kinds
            .iter()
            .map(|(kind, label)| KindChip {
                kind: kind.clone(),
                label: label.clone(),
                name: t_args("host-a11y-filter-kind", &[("kind", label.as_str().into())]),
            })
            .collect();
        let scopes = inputs
            .scopes
            .iter()
            .map(|(scope, label)| ScopeSegment {
                scope: *scope,
                label: label.clone(),
                name: t_args("host-a11y-scope", &[("scope", label.as_str().into())]),
            })
            .collect();
        filters.configure(
            kinds,
            inputs.selected.clone(),
            scopes,
            inputs.active_scope,
            FilterText {
                all_label: t("search-filter-all"),
                all_name: t("host-a11y-filter-all"),
                more_label: t("search-filter-more"),
                more_name: t("host-a11y-filter-more"),
                filters_name: t("host-a11y-search-filters"),
                kinds_name: t("host-a11y-result-kinds"),
                scopes_name: t("host-a11y-search-scope"),
            },
        );
    }

    /// Reconfigure the band only when what it shows has changed.
    pub(super) fn sync_search_filters(&mut self) {
        let inputs = self.search_filter_inputs();
        if self.search_filter_inputs.as_ref() != Some(&inputs) {
            Self::configure_search_filters(&mut self.search_filters, &inputs);
            self.search_filter_inputs = Some(inputs);
        }
    }

    /// The filter band as semantics, reflecting the current selection even
    /// before the next paint re-lays it out.
    pub(super) fn search_filters_accessibility(&self) -> ui::a11y::AccessNode {
        let inputs = self.search_filter_inputs();
        if self.search_filter_inputs.as_ref() == Some(&inputs) {
            return self.search_filters.accessibility();
        }
        let mut filters = self.search_filters.clone();
        Self::configure_search_filters(&mut filters, &inputs);
        filters.accessibility()
    }

    /// [`Self::activate_selected_result_with`] with no modifier held.
    #[cfg(test)]
    pub(super) fn activate_selected_result(
        &mut self,
        doc: &mut ui::widget::Edits<'_>,
        root: &ObjId,
    ) {
        self.activate_selected_result_with(doc, root, None);
    }

    /// Dispatch the selected palette row: a value of the argument step on
    /// screen, one of a file's other actions, a "Show all" row, or a search
    /// result through its provider callback. With `modifier` held a row that
    /// opens a window opens as a tab here (Shift), beside the focused window
    /// (Ctrl), or lists its other actions (Alt); Shift on a window opens a
    /// new window of its app, as a tab here.
    pub(super) fn activate_selected_result_with(
        &mut self,
        doc: &mut ui::widget::Edits<'_>,
        root: &ObjId,
        modifier: Option<ui::widget::command_palette::ActivationModifier>,
    ) {
        let Some(id) = self
            .palette
            .selected_id()
            .map(std::string::ToString::to_string)
        else {
            return;
        };
        if let Some(index) = id
            .strip_prefix(palette_flow::CHOICE_PREFIX)
            .and_then(|index| index.parse::<usize>().ok())
        {
            self.choose_param_value(index, doc, root);
            return;
        }
        if let Some(slug) = id.strip_prefix(SECONDARY_PREFIX) {
            self.run_secondary_action(slug, doc, root);
            return;
        }
        if id == palette_flow::ALL_APPS_ID {
            self.show_all_apps(doc, root);
            return;
        }
        if let Some(more) = id
            .strip_prefix(palette_flow::MORE_PREFIX)
            .and_then(|more| more.parse::<usize>().ok())
        {
            self.show_whole_group(more, doc, root);
            return;
        }
        let Some(result) = id
            .strip_prefix("result:")
            .and_then(|idx| idx.parse::<usize>().ok())
            .and_then(|idx| self.search_results.get(idx).cloned())
        else {
            return;
        };
        // Choosing an app for an app: the row answers it, never opens.
        if self.choosing_app() {
            let _ = self.choose_app_row(&result);
            return;
        }
        if modifier == Some(ui::widget::command_palette::ActivationModifier::Shift)
            && let Some(app) = self.new_window_of(&result)
        {
            let target = self
                .focused_group()
                .map(crate::shell::chrome_state::OpenTarget::Here);
            self.run_result(app, doc, root, target);
            return;
        }
        let activation = modifier
            .filter(|_| result.params.is_empty() && palette_flow::opens_a_window(&result.kind))
            .map(palette_flow::Activation::of);
        match activation {
            Some(palette_flow::Activation::More)
                if result.kind == crate::shell::search::ResourceKind::File
                    || !self.search_registry.secondary_actions(&result).is_empty() =>
            {
                self.show_secondary_actions(result);
            }
            Some(palette_flow::Activation::TabHere) => {
                let target = self
                    .focused_group()
                    .map(crate::shell::chrome_state::OpenTarget::Here);
                self.run_result(result, doc, root, target);
            }
            Some(palette_flow::Activation::Beside) => {
                let target = self
                    .focused_group()
                    .map(crate::shell::chrome_state::OpenTarget::RightOf);
                self.run_result(result, doc, root, target);
            }
            _ => self.run_result(result, doc, root, None),
        }
    }

    /// For a window row, the row that starts its app: what a new window of
    /// it runs. `None` for any other row, or a window of no app search has.
    pub(super) fn new_window_of(
        &self,
        result: &crate::shell::search::SearchResult,
    ) -> Option<crate::shell::search::SearchResult> {
        if result.provider_id != tilo_desktop::search::WINDOWS_PROVIDER_ID
            || result.kind != crate::shell::search::ResourceKind::Tab
        {
            return None;
        }
        let app = {
            let state = self.chrome.as_ref()?.borrow();
            let tree = state.tree();
            tilo_desktop::search::window_row_app_id(&tree.desktop, &result.id)?
        };
        self.search_registry.app_row(&app)
    }

    /// The group of the focused window: where "here" and "beside" are.
    pub(super) fn focused_group(&self) -> Option<GroupId> {
        let chrome = self.chrome.as_ref()?;
        let state = chrome.borrow();
        let strip = state.active_strip_id()?;
        state.tree().desktop.active_group_for(strip)
    }

    /// Run one search result. An action that takes arguments first asks for
    /// them, one step each, unless the query already named them. `target`
    /// overrides where a window it opens lands, for this one run.
    #[allow(
        clippy::too_many_lines,
        reason = "invocation, its failure report and the three ways a run ends (resource scope, a switch kept open, close) stay in their visible order"
    )]
    pub(super) fn run_result(
        &mut self,
        result: crate::shell::search::SearchResult,
        doc: &mut ui::widget::Edits<'_>,
        root: &ObjId,
        target: Option<crate::shell::chrome_state::OpenTarget>,
    ) {
        let Some(chrome) = self.chrome.clone() else {
            return;
        };
        if !result.params.is_empty() {
            self.begin_param_flow(result, doc, root);
            return;
        }
        let previous_target = target.map(|target| {
            let mut state = chrome.borrow_mut();
            std::mem::replace(&mut state.open_target, target)
        });
        let args = crate::shell::search::action_api::ActionArgs::none();
        let previous_notice = chrome
            .borrow()
            .notifications()
            .last()
            .map(|notice| notice.id);
        let outcome = if let Some(begin) = &result.on_begin_invoke {
            begin(doc, root, &chrome, &args).and_then(|disposition| match disposition {
                crate::shell::search::InvocationDisposition::Accepted(_) => Ok(()),
                crate::shell::search::InvocationDisposition::Pending(ticket) => {
                    self.settle_or_await(&result.title, ticket)
                }
            })
        } else if let Some(invoke) = &result.on_invoke {
            invoke(doc, root, &chrome, &args).map(|_| ())
        } else {
            (result.on_open)(doc, root, &chrome, &args)
        };
        if let Some(previous) = previous_target {
            chrome.borrow_mut().open_target = previous;
        }
        let entered_resource_scope =
            outcome.is_ok() && result.id.starts_with("directory-resource\u{1f}");
        if let Err(err) = outcome {
            let reported_detail = chrome
                .borrow()
                .notifications()
                .last()
                .filter(|notice| {
                    Some(notice.id) != previous_notice
                        && matches!(
                            notice.severity,
                            tilo_desktop::notifications::NotificationSeverity::Warning
                                | tilo_desktop::notifications::NotificationSeverity::Error
                        )
                })
                .map(|notice| notice.body.clone());
            // The failure is told once, on the row that failed. The error's
            // own text is for agents and logs; people get its catalogue
            // sentence (or the notice the action already posted).
            tracing::info!(
                provider = %result.provider_id,
                action = %result.id,
                code = err.code(),
                detail = %err,
                "palette action did not run"
            );
            let detail = reported_detail.unwrap_or_else(|| err.user_message());
            self.palette
                .set_selected_status(t("host-action-failed-status"), detail);
            self.palette_layout_dirty = true;
            return;
        }
        if entered_resource_scope {
            let label = chrome.borrow().search_resource_scope.as_ref().map_or_else(
                || t("host-search-in-resource-fallback"),
                |scope| scope.label.clone(),
            );

            self.palette.set_query("");
            self.palette.set_query_placeholder(t_args(
                "host-search-in",
                &[("scope", label.as_str().into())],
            ));

            self.last_search_query = "\0resource-scope-enter\0".into();
            self.palette_visible = true;
            return;
        }
        // What was typed for it, too: the same text leads with it next time.
        crate::shell::search::usage::record_choice(
            self.palette.query(),
            &result.provider_id,
            &result.id,
        );
        // A switch flips where the person can see it: keep the palette open
        // and rebuild its rows now, so the row shows the state it was just
        // switched to. One whose state lands later is rebuilt when the
        // chrome revision it reads moves (`Self::refresh_open_palette`).
        if self.search_registry.toggles_in_place(&result) {
            self.last_search_revision.clear();
            self.refresh_search_results(doc, root);
            return;
        }
        self.palette_visible = false;
        self.palette.close();
    }

    /// A window operation this host runs itself is run now, so its outcome
    /// is told on the row that asked for it; any other ticket waits among
    /// the admissions, which tell how it went once it is answered.
    fn settle_or_await(
        &mut self,
        title: &str,
        ticket: crate::shell::search::PendingInvocation,
    ) -> Result<(), crate::shell::search::action_api::ActionError> {
        self.run_requested_shortcut_actions();
        if ticket()?.is_none() {
            self.pending_action_admissions
                .push((title.to_owned(), ticket));
        }
        Ok(())
    }

    /// Start collecting `result`'s arguments. Words after its title in the
    /// query fill the first one ("scale 125"): a declared value or a number
    /// runs the action at once; free text waits in its field for Enter.
    pub(super) fn begin_param_flow(
        &mut self,
        result: crate::shell::search::SearchResult,
        doc: &mut ui::widget::Edits<'_>,
        root: &ObjId,
    ) {
        let query = self.palette.query().to_string();
        let mut values = serde_json::Map::new();
        let mut prefill = None;
        if let Some(inline) = palette_flow::inline_argument(&query, &result.title, &result.params) {
            if inline.certain {
                values.insert(inline.name, inline.value);
            } else if let serde_json::Value::String(text) = inline.value {
                prefill = Some((inline.name, text));
            }
        }
        // The row an app prepared, picked from the search it opened: its
        // arguments, never the person's confirmation, which they give here.
        #[allow(
            clippy::suspicious_operation_groupings,
            reason = "an action row's id is the action an offer names; an offer has no id of its own"
        )]
        if let Some((offer, label)) = self.palette_offer.take()
            && result.provider_id == crate::shell::search::providers::actions::PROVIDER_ID
            && result.id == offer.action
            && query == label
        {
            for (name, value) in offer.arguments {
                if name != "confirm"
                    && result
                        .params
                        .iter()
                        .any(|param| param.name.as_ref() == name)
                {
                    values.insert(name, serde_json::Value::String(value));
                }
            }
        }
        let pending = palette_flow::missing_params(&result.params, &values);
        self.param_flow = Some(palette_flow::ParamFlow {
            result,
            values,
            pending,
            asked: Vec::new(),
            choices: Vec::new(),
            query,
            prefill,
        });
        self.advance_param_flow(doc, root);
    }

    /// Show the next step, or run the action once nothing is missing.
    pub(super) fn advance_param_flow(&mut self, doc: &mut ui::widget::Edits<'_>, root: &ObjId) {
        if self
            .param_flow
            .as_ref()
            .is_some_and(|flow| flow.pending.is_empty())
        {
            self.finish_param_flow(doc, root);
        } else {
            self.show_param_step();
        }
    }

    /// Put the step for the parameter on screen: its values as rows to pick
    /// from, or a field of its kind.
    pub(super) fn show_param_step(&mut self) {
        let Some((param, title)) = self
            .param_flow
            .as_ref()
            .and_then(|flow| Some((flow.param().cloned()?, flow.result.title.clone())))
        else {
            return;
        };
        let step = palette_flow::step_for(&param, &title, &|pick| self.param_picks(pick));
        let Some(flow) = self.param_flow.as_mut() else {
            return;
        };
        match step {
            palette_flow::Step::Choices(rows) => {
                flow.choices = rows.iter().map(|choice| choice.value.clone()).collect();
                let entries = rows
                    .into_iter()
                    .enumerate()
                    .map(|(i, choice)| {
                        let mut entry = CommandEntry::new(
                            format!("{}{i}", palette_flow::CHOICE_PREFIX),
                            choice.label.clone(),
                        )
                        .group(title.clone());
                        if let Some(detail) = choice.detail {
                            entry = entry.description(detail);
                        }
                        match choice.person {
                            Some(key) => entry.avatar(choice.label, key),
                            None => entry,
                        }
                    })
                    .collect();
                self.palette.cancel_argument();
                // Typing narrows the values, as it narrows any list.
                self.palette.set_bypass_fuzzy(false);
                self.palette.set_query("");
                self.palette.set_entries(entries);
                self.palette
                    .set_query_placeholder(param.description.to_string());
            }
            palette_flow::Step::Field(mut spec) => {
                flow.choices.clear();
                if let Some((_, text)) = flow
                    .prefill
                    .take_if(|(name, _)| name.as_str() == param.name.as_ref())
                {
                    spec.prefill = Some(text);
                }
                self.palette.set_entries(Vec::new());
                self.palette
                    .open_argument_step(format!("param:{}", param.name), spec);
            }
        }
        self.palette.set_filter_band_shown(false);
        self.palette_layout_dirty = true;
    }

    /// What a person may pick for `pick` in the active workspace.
    fn param_picks(&self, pick: tilo_actions::Pick) -> Vec<palette_flow::Choice> {
        let Some(chrome) = self.chrome.as_ref() else {
            return Vec::new();
        };
        let state = chrome.borrow();
        let user = state.active_local_user();
        let picked = state.tree().desktop.active_workspace().map(|workspace| {
            let directory = super::ws_settings::SettingsDirectory {
                room: workspace.room().to_string(),
            };
            tilo_desktop::workspace_settings::picks(workspace, &user, &directory, pick)
        });
        picked
            .into_iter()
            .flatten()
            .map(|picked| palette_flow::Choice {
                value: picked.value,
                label: picked.label,
                detail: picked.detail,
                person: picked.person,
            })
            .collect()
    }

    /// The step on screen was answered with the value of its row `index`.
    pub(super) fn choose_param_value(
        &mut self,
        index: usize,
        doc: &mut ui::widget::Edits<'_>,
        root: &ObjId,
    ) {
        let Some(flow) = self.param_flow.as_mut() else {
            return;
        };
        let (Some(value), Some(param)) = (flow.choices.get(index).cloned(), flow.param()) else {
            return;
        };
        let name = param.name.to_string();
        flow.values.insert(name, value);
        let asked = flow.pending.remove(0);
        flow.asked.push(asked);
        self.advance_param_flow(doc, root);
    }

    /// The field on screen was confirmed with `text`. A value the
    /// parameter can't take asks again, saying why.
    pub(super) fn submit_param_field(
        &mut self,
        text: String,
        doc: &mut ui::widget::Edits<'_>,
        root: &ObjId,
    ) {
        let Some(flow) = self.param_flow.as_mut() else {
            return;
        };
        let Some(param) = flow.param().cloned() else {
            return;
        };
        match palette_flow::value_of(param.ty, &text) {
            Ok(value) => {
                flow.values.insert(param.name.to_string(), value);
                let asked = flow.pending.remove(0);
                flow.asked.push(asked);
                self.advance_param_flow(doc, root);
            }
            Err(reason) => {
                let palette_flow::Step::Field(mut spec) =
                    palette_flow::step_for(&param, &flow.result.title, &|_| Vec::new())
                else {
                    return;
                };
                spec.prompt = reason;
                // A refused secret is not shown again; anything else stays
                // to be corrected.
                spec.prefill = (param.ty != crate::shell::search::action_api::ParamType::Secret)
                    .then_some(text);
                self.palette
                    .open_argument_step(format!("param:{}", param.name), spec);
            }
        }
    }

    /// Run the action with the values collected. A refusal is reported and
    /// the steps start over, so the values can be corrected.
    pub(super) fn finish_param_flow(&mut self, doc: &mut ui::widget::Edits<'_>, root: &ObjId) {
        let Some(mut flow) = self.param_flow.take() else {
            return;
        };
        let Some(chrome) = self.chrome.clone() else {
            return;
        };
        let result = flow.result.clone();
        let outcome =
            crate::shell::search::action_api::ActionArgs::bind_json(&result.params, &flow.values)
                .map_err(crate::shell::search::action_api::ActionError::Arguments)
                .and_then(|bound| {
                    if let Some(begin) = &result.on_begin_invoke {
                        match begin(doc, root, &chrome, &bound)? {
                            crate::shell::search::InvocationDisposition::Accepted(_) => Ok(()),
                            crate::shell::search::InvocationDisposition::Pending(ticket) => {
                                self.settle_or_await(&result.title, ticket)
                            }
                        }
                    } else if let Some(invoke) = &result.on_invoke {
                        invoke(doc, root, &chrome, &bound).map(|_| ())
                    } else {
                        (result.on_open)(doc, root, &chrome, &bound)
                    }
                });
        // A change the values chosen make less safe asks one more step: the
        // person's confirmation, with what they chose kept.
        if matches!(
            outcome,
            Err(crate::shell::search::action_api::ActionError::ConfirmationRequired)
        ) && let Some(confirm) = result
            .params
            .iter()
            .position(|param| param.name == "confirm")
            .filter(|_| !flow.values.contains_key("confirm"))
        {
            flow.pending = vec![confirm];
            self.param_flow = Some(flow);
            self.show_param_step();
            return;
        }
        if let Err(error) = outcome {
            // Told once, where the person is: on the step that starts over.
            tracing::info!(
                provider = %result.provider_id,
                action = %result.id,
                code = error.code(),
                detail = %error,
                "palette action did not run"
            );
            flow.values.clear();
            flow.asked.clear();
            flow.pending = palette_flow::missing_params(&result.params, &flow.values);
            self.param_flow = Some(flow);
            self.show_param_step();
            self.show_step_error(error.user_message());
            return;
        }
        // What was typed for it, too: the same text leads with it next time.
        crate::shell::search::usage::record_choice(&flow.query, &result.provider_id, &result.id);
        self.end_palette_steps();
        // A switch stays in view, showing what it was switched to, as it
        // does when it asks nothing first.
        if self.search_registry.toggles_in_place(&result) {
            self.last_search_revision.clear();
            self.refresh_search_results(doc, root);
            return;
        }
        self.palette_visible = false;
        self.palette.close();
    }

    /// Say why a run was refused on the step on screen: as a field's
    /// heading, or on the selected value.
    pub(super) fn show_step_error(&mut self, message: String) {
        let Some(param) = self.param_flow.as_ref().and_then(|flow| {
            flow.param()
                .cloned()
                .map(|param| (param, flow.result.title.clone()))
        }) else {
            return;
        };
        let (param, title) = param;
        match palette_flow::step_for(&param, &title, &|pick| self.param_picks(pick)) {
            palette_flow::Step::Field(mut spec) => {
                spec.prompt = message;
                self.palette
                    .open_argument_step(format!("param:{}", param.name), spec);
            }
            palette_flow::Step::Choices(_) => {
                self.palette
                    .set_selected_status(t("host-action-failed-status"), message);
            }
        }
    }

    /// Escape inside the steps: back to the previous parameter, or from the
    /// first one back to the results, with what was typed before.
    pub(super) fn palette_step_back(&mut self) {
        if let Some(flow) = self.param_flow.as_mut() {
            if let Some(previous) = flow.asked.pop() {
                if let Some(param) = flow.result.params.get(previous) {
                    flow.values.remove(param.name.as_ref());
                }
                flow.pending.insert(0, previous);
                self.show_param_step();
                return;
            }
            let query = flow.query.clone();
            self.end_palette_steps();
            self.restore_results(&query);
            return;
        }
        if let Some((_, query)) = self.secondary_flow.take() {
            self.end_palette_steps();
            self.restore_results(&query);
        }
    }

    /// Back on the results for `query`; the next frame lists them.
    pub(super) fn restore_results(&mut self, query: &str) {
        self.palette.set_query(query);
        self.palette
            .set_query_placeholder(t("host-search-placeholder"));
        // Never equal to a real query: the next frame fans out again.
        self.last_search_query = "\0steps-ended\0".into();
        self.palette_layout_dirty = true;
    }

    /// Leave any argument step or list of other actions.
    pub(super) fn end_palette_steps(&mut self) {
        self.param_flow = None;
        self.secondary_flow = None;
        self.palette.cancel_argument();
        self.palette.set_bypass_fuzzy(true);
    }

    /// Every app, as choosing the Apps chip lists them: the launcher's
    /// "All apps" row. Clearing the chip brings the launcher back.
    pub(super) fn show_all_apps(&mut self, doc: &automerge::AutoCommit, root: &ObjId) {
        self.active_kind_filter = vec![crate::shell::search::ResourceKind::Application];
        self.last_search_query.clear(); // force a refresh
        self.refresh_search_results(doc, root);
        self.palette.set_cursor(0);
    }

    /// Show every row of the group behind "Show all" row `more`, in place:
    /// the selection stays where the row was, now on the group's next hit.
    pub(super) fn show_whole_group(
        &mut self,
        more: usize,
        doc: &automerge::AutoCommit,
        root: &ObjId,
    ) {
        let Some(kind) = self.more_rows.get(more).cloned() else {
            return;
        };
        let cursor = self.palette.cursor();
        if !self.expanded_kinds.contains(&kind) {
            self.expanded_kinds.push(kind);
        }
        self.last_search_revision.clear();
        // The same query keeps the expansion and the chosen row.
        let query = self.palette.query().to_string();
        self.last_search_query.clone_from(&query);
        self.refresh_search_results(doc, root);
        self.palette.set_cursor(cursor);
    }

    /// List a row's other actions (Alt+Enter) in place of the results: a
    /// file's, or the ones its provider gives it
    /// ([`crate::shell::search::SearchProvider::secondary_actions`]).
    pub(super) fn show_secondary_actions(&mut self, result: crate::shell::search::SearchResult) {
        let query = self.palette.query().to_string();
        let entries = if result.kind == crate::shell::search::ResourceKind::File {
            palette_flow::SecondaryAction::FOR_FILES
                .into_iter()
                .map(|action| {
                    CommandEntry::new(
                        format!("{SECONDARY_PREFIX}{}", action.slug()),
                        action.label(),
                    )
                    .group(result.title.clone())
                })
                .collect()
        } else {
            self.search_registry
                .secondary_actions(&result)
                .into_iter()
                .enumerate()
                .map(|(at, row)| {
                    let mut entry = CommandEntry::new(
                        format!("{SECONDARY_PREFIX}{PROVIDED_SLUG}{at}"),
                        row.title,
                    )
                    .group(result.title.clone());
                    if let Some(subtitle) = row.subtitle {
                        entry = entry.description(subtitle);
                    }
                    entry
                })
                .collect()
        };
        self.palette.set_bypass_fuzzy(false);
        self.palette.set_query("");
        self.palette.set_entries(entries);
        self.palette.set_query_placeholder(result.title.clone());
        self.palette.set_filter_band_shown(false);
        self.secondary_flow = Some((result, query));
        self.palette_layout_dirty = true;
    }

    /// Run one of the listed row's other actions.
    pub(super) fn run_secondary_action(
        &mut self,
        slug: &str,
        doc: &mut ui::widget::Edits<'_>,
        root: &ObjId,
    ) {
        if let Some(at) = slug
            .strip_prefix(PROVIDED_SLUG)
            .and_then(|at| at.parse::<usize>().ok())
        {
            let Some((result, _)) = self.secondary_flow.clone() else {
                return;
            };
            self.end_palette_steps();
            if let Some(row) = self
                .search_registry
                .secondary_actions(&result)
                .into_iter()
                .nth(at)
            {
                self.run_result(row, doc, root, None);
            }
            return;
        }
        let Some(action) = palette_flow::SecondaryAction::from_slug(slug) else {
            return;
        };
        let Some((result, _)) = self.secondary_flow.clone() else {
            return;
        };
        self.end_palette_steps();
        match action {
            palette_flow::SecondaryAction::TabHere => {
                let target = self
                    .focused_group()
                    .map(crate::shell::chrome_state::OpenTarget::Here);
                self.run_result(result, doc, root, target);
            }
            palette_flow::SecondaryAction::Beside => {
                let target = self
                    .focused_group()
                    .map(crate::shell::chrome_state::OpenTarget::RightOf);
                self.run_result(result, doc, root, target);
            }
            palette_flow::SecondaryAction::ContainingFolder => {
                let folder = std::path::Path::new(&result.id)
                    .parent()
                    .and_then(|parent| self.resolve_folder_row(parent, doc, root));
                if let Some(folder) = folder {
                    self.run_result(folder, doc, root, None);
                } else {
                    self.palette_visible = false;
                    self.palette.close();
                }
            }
            palette_flow::SecondaryAction::CopyPath => {
                ui::text_selection::clipboard_copy(&result.id);
                self.palette_visible = false;
                self.palette.close();
            }
        }
    }

    /// The folder row the files provider gives `path`, opened the way any
    /// folder found by search opens.
    pub(super) fn resolve_folder_row(
        &self,
        path: &std::path::Path,
        doc: &ui::widget::Edits<'_>,
        root: &ObjId,
    ) -> Option<crate::shell::search::SearchResult> {
        let chrome = self.chrome.as_ref()?;
        let state = chrome.borrow();
        let folder = [crate::shell::search::ResourceKind::Folder];
        let args = crate::shell::search::QueryArgs {
            needle: "",
            scope: crate::shell::chrome_state::SearchScope::All,
            active_provider_id: None,
            wanted_kinds: &folder,
            chrome: &state,
            doc,
            doc_root: root,
        };
        self.search_registry
            .resolve("files", &path.display().to_string(), &args)
    }
}
