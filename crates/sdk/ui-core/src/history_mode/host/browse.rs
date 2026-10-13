//! Entry lenses do not change the document captured by History's owner.
use super::{Button, Column, HistoryHost, I18nManager, Row, Text};
use crate::widget::{Dropdown, HistoryFilter};
use serde::{Deserialize, Serialize};
use tilo_actions::{ActionParam, ActionSpec, EncodedAction, OperationKind, ParamType};

pub(crate) const FILTER_OPERATION: &str = "tilo.host.history.filter";

#[derive(Clone, Default, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub(super) struct BrowseFilter {
    #[serde(default)]
    resource: String,
    #[serde(default)]
    actor: String,
    #[serde(default)]
    group: String,
}
impl BrowseFilter {
    fn entries(&self) -> HistoryFilter {
        let lens = |value: &String| {
            if value.is_empty() {
                vec![]
            } else {
                vec![value.clone()]
            }
        };
        HistoryFilter {
            resource_ids: lens(&self.resource),
            actor_ids: lens(&self.actor),
            work_group_ids: lens(&self.group),
            ..HistoryFilter::default()
        }
    }
}
impl HistoryHost {
    pub(super) fn browse_resource(
        &self,
        resource: tilo_actions::Resource,
    ) -> tilo_actions::Resource {
        if self.invalid_owner || self.controller.is_none() {
            return resource;
        }
        resource.with_action(ActionSpec::typed_operation(
            "filter",
            I18nManager::lookup_plain("history-browse-filter"),
            I18nManager::lookup_plain("history-browse-owner"),
            OperationKind::View,
            FILTER_OPERATION,
            ["resource", "actor", "group"]
                .into_iter()
                .map(|name| {
                    ActionParam::optional_owned(
                        name,
                        ParamType::Text,
                        I18nManager::lookup_plain(&format!("history-browse-{name}")),
                    )
                })
                .collect(),
            |args| EncodedAction {
                type_id: FILTER_OPERATION.into(),
                payload: serde_json::to_vec(&BrowseFilter {
                    resource: args.text("resource").unwrap_or_default().into(),
                    actor: args.text("actor").unwrap_or_default().into(),
                    group: args.text("group").unwrap_or_default().into(),
                })
                .unwrap_or_default(),
            },
        ))
    }
    pub(super) fn invoke_browse(&self, encoded: &EncodedAction) -> Option<Result<(), String>> {
        if encoded.type_id != FILTER_OPERATION {
            return None;
        }
        Some((|| {
            if self.invalid_owner || self.controller.is_none() {
                return Err("History owner unavailable".into());
            }
            let filter =
                serde_json::from_slice(&encoded.payload).map_err(|error| error.to_string())?;
            *self.browse_request.borrow_mut() = Some(filter);
            self.request_open();
            Ok(())
        })())
    }
    pub(super) fn browse_toolbar(&self, mut toolbar: Column) -> Column {
        if !self.open || !self.show_timeline() || self.controller.is_none() {
            return toolbar;
        }
        let all = I18nManager::lookup_plain("history-browse-all");
        let mut row = Row::new();
        for (id, title, selected) in [
            (
                "resource",
                "history-browse-resource",
                &self.browse_filter.resource,
            ),
            ("actor", "history-browse-actor", &self.browse_filter.actor),
            ("group", "history-browse-group", &self.browse_filter.group),
        ] {
            // Only the captured owner's admitted projection supplies choices.
            // Labels are presentation, never selection identities.
            let mut options: Vec<(String, String)> = self
                .timeline
                .entries()
                .iter()
                .filter_map(|entry| match id {
                    "actor" if !entry.is_genesis() => entry
                        .context
                        .as_ref()?
                        .invoker
                        .as_ref()
                        .map(|invoker| (invoker.actor.clone(), entry.author.name.clone())),
                    "resource" => entry
                        .context
                        .as_ref()?
                        .resource
                        .as_ref()
                        .map(|resource| (resource.id.clone(), resource.label.clone())),
                    "group" => entry
                        .context
                        .as_ref()?
                        .group
                        .as_ref()
                        .map(|group| (group.id.clone(), group.label.clone())),
                    _ => None,
                })
                .filter(|(value, _)| !value.is_empty())
                .collect();
            options.sort();
            options.dedup_by(|a, b| a.0 == b.0);
            options.sort_by(|a, b| a.1.cmp(&b.1).then(a.0.cmp(&b.0)));
            if !selected.is_empty() && !options.iter().any(|(value, _)| value == selected) {
                options.push((
                    selected.clone(),
                    I18nManager::lookup_plain("history-browse-filter"),
                ));
            }
            // Empty is the explicit no-filter value, independent of locale.
            options.insert(0, (String::new(), all.clone()));
            let state = crate::LocalState::new(selected.clone());
            let requests = self.browse_request.clone();
            let filter = self.browse_filter.clone();
            let mut picker = Dropdown::from_state(Vec::new(), &state)
                .with_id(&format!("history-filter-{id}"))
                .with_accessible_name(title)
                .caption(title)
                .on_change(move |value| {
                    let mut next = filter.clone();
                    match id {
                        "resource" => value.clone_into(&mut next.resource),
                        "actor" => value.clone_into(&mut next.actor),
                        _ => value.clone_into(&mut next.group),
                    }
                    *requests.borrow_mut() = Some(next);
                });
            picker.set_value_options(options);
            row = row.push(Box::new(picker));
        }
        let requests = self.browse_request.clone();
        toolbar = toolbar.push(Box::new(row));
        toolbar
            .push(Box::new(
                Button::new("history-filter-clear", "history-browse-clear").on_click(move || {
                    *requests.borrow_mut() = Some(BrowseFilter::default());
                }),
            ))
            .push(Box::new(Text::new("history-browse-owner")))
    }
    pub(super) fn apply_browse_request(&mut self) -> bool {
        let request = self.browse_request.borrow_mut().take();
        let Some(filter) = request else {
            return false;
        };
        self.browse_filter = filter;
        let entries = self.browse_filter.entries();
        if entries == HistoryFilter::default() {
            self.timeline.reset_filters();
        } else {
            self.timeline.set_filter(entries);
        }
        // A hidden selection must not remain a restore target or retain a
        // misleading preview. Filtering never changes the source registration.
        self.selected.clear();
        self.timeline.set_selection(None);
        self.before.preview = None;
        self.after.preview = None;
        self.assets.cancel();
        self.review = None;
        self.review_request = None;
        if let Some(controller) = &mut self.controller {
            controller.back_to_live();
        }
        true
    }
    pub(super) fn compare_description(&self) -> String {
        let summaries = self
            .controller
            .as_ref()
            .map(|controller| controller.selected_summaries().join(" · "))
            .unwrap_or_default();
        I18nManager::format("history-compare-pinned", &[("changes", summaries.into())])
    }
}
