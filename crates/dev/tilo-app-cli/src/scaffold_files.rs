//! The files a template writes beside its code: the headless drive that
//! proves it, its source catalog and its committed surface.

use crate::{AppSpec, Template, escape_fluent_string};

pub(crate) fn headless_actions(spec: &AppSpec) -> String {
    match spec.template {
        Template::Starter => String::from(
            "[\n  {\"perform\": {\"target\": \"name-input\", \"action\": \"set_value\", \"value\": \"Ada\"}},\n  {\"perform\": {\"target\": \"name-input\", \"action\": \"focus\"}},\n  {\"expect\": {\"target\": \"name-input\", \"role\": \"text_input\", \"value\": \"Ada\", \"focused\": true}},\n  {\"expect\": {\"target\": \"greeting\", \"role\": \"static_text\", \"name_contains\": \"Ada\"}}\n]\n",
        ),
        Template::Document => String::from(
            "[\n  {\"perform\": {\"target\": \"document-title-input\", \"action\": \"set_value\", \"value\": \"Project Atlas\"}},\n  {\"perform\": {\"target\": \"document-body-input\", \"action\": \"set_value\", \"value\": \"A durable first draft.\"}},\n  {\"expect\": {\"target\": \"document-title-preview\", \"role\": \"heading\", \"name\": \"Project Atlas\"}},\n  {\"expect\": {\"target\": \"document-body-preview\", \"name\": \"A durable first draft.\"}}\n]\n",
        ),
        Template::Collection => String::from(
            "[\n  {\"perform\": {\"target\": \"draft-input\", \"action\": \"set_value\", \"value\": \"Buy milk\"}},\n  {\"expect\": {\"target\": \"draft-input\", \"role\": \"text_input\", \"value\": \"Buy milk\"}},\n  {\"perform\": {\"target\": \"add\", \"action\": \"activate\"}},\n  {\"expect\": {\"target\": \"draft-input\", \"role\": \"text_input\", \"value\": \"\"}}\n]\n",
        ),
        Template::Graph => String::from(
            "[\n  {\"perform\": {\"target\": \"name-input\", \"action\": \"set_value\", \"value\": \"Atlas\"}},\n  {\"expect\": {\"target\": \"name-preview\", \"role\": \"static_text\", \"name\": \"Atlas\"}},\n  {\"perform\": {\"target\": \"create\", \"action\": \"activate\"}}\n]\n",
        ),
    }
}

/// The app's source catalog. A graph screen's static text is a message id
/// too, so the graph template's catalog also names the screen it generates.
pub(crate) fn app_catalog(spec: &AppSpec) -> String {
    // The templates key their history messages `generated-…`, as they name
    // their kinds `generated.…`; the history looks them up under the app.
    // The words of their actions, after `## Actions`, are keyed by kind, so
    // they move with the kind to the app's whole id.
    let history = format!("{}-", tilo_history::app_prefix(&spec.app_id));
    let actions = format!("{}-", spec.app_id.replace(['.', '_', ':'], "-"));
    let mut prefix = &history;
    let catalog: String = spec
        .template
        .catalog()
        .lines()
        .map(|line| {
            if line.starts_with("## Actions") {
                prefix = &actions;
            }
            line.strip_prefix("generated-")
                .map_or_else(|| format!("{line}\n"), |rest| format!("{prefix}{rest}\n"))
        })
        .collect();
    if spec.template != Template::Graph {
        return catalog;
    }
    format!(
        "graph-title = {{ \"{}\" }}\ngraph-introduction = This screen is a serialized graph under ui/. Edit it while the app runs with tilo-app ui edit ui/main.json.\n{catalog}",
        escape_fluent_string(&spec.title)
    )
}

/// The app's committed public surface: what `tilo-app surface` writes for
/// the template's code, under the app's own resource kinds.
pub(crate) fn app_surface(spec: &AppSpec) -> String {
    spec.template
        .surface()
        .replace("kind = \"generated.", &format!("kind = \"{}.", spec.app_id))
}
