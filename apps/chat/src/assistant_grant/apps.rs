//! Ask for precisely the apps the person selects from this host's signed action catalogue.

use tilo_actions::remote::{CatalogueQuery, InvokeArgument};
use tilo_server_protocol::agents::{
    AgentGrantAction, AgentGrantApp, AgentGrantApps, AgentGrantStatus, GrantState,
};
use ui::Arrival;
use ui::prelude::*;
use ui::widget::arrange::{Grid, Length, Stack};

use super::{Access, Panel};
use crate::Chat;

impl Access {
    pub(super) fn with_app_choices(&self, cx: &ViewCx<Chat>, status: AgentGrantStatus) -> Element {
        if matches!(status.state, GrantState::Pending | GrantState::Approved) {
            return self.panel_with_choices(cx, status, Vec::new());
        }
        let target = AgentGrantAction::RequestApps.target(&self.agent);
        let query = CatalogueQuery {
            provider: Some(target.provider.clone()),
            target: Some(target.address.clone()),
            max_results: 1,
            ..Default::default()
        };
        let read = self.remote.catalogue_page(query);
        let access = self.clone();
        cx.switch(&read.state(), move |cx, arrival| {
            let choices = match arrival {
                Arrival::Ready(page) => page
                    .rows
                    .iter()
                    .filter(|row| row.provider == target.provider)
                    .filter_map(|row| row.action.as_ref())
                    .find(|action| action.address == target.address)
                    .map(AgentGrantApp::choices)
                    .unwrap_or_default(),
                _ => Vec::new(),
            };
            access.panel_with_choices(cx, status.clone(), choices)
        })
    }

    fn panel_with_choices(
        &self,
        cx: &ViewCx<Chat>,
        status: AgentGrantStatus,
        choices: Vec<AgentGrantApp>,
    ) -> Element {
        let access = self.clone();
        cx.switch(&self.panel, move |_, panel| {
            let offered = choices.len() > 1 && choices.iter().any(|app| app.id == ui::apps::CHAT);
            if *panel == Panel::ChooseApps && offered {
                access.app_picker(&choices)
            } else {
                access.status(&status, *panel, offered)
            }
        })
    }

    fn app_picker(&self, choices: &[AgentGrantApp]) -> Element {
        let mut options = Grid::auto(Length::Rem(11.0)).gap(ui::tokens::Rhythm::Tight);
        let mut selected = Vec::new();
        for app in choices {
            let required = app.id == ui::apps::CHAT;
            let id = format!("chat-assistant-grant-app-{}", app.id);
            let selected_here = LocalState::new(required);
            let label = if required {
                crate::i18n::t("chat-assistant-grant-chat-required")
            } else if choices
                .iter()
                .any(|other| other.id != app.id && other.title == app.title)
            {
                format!("{} ({})", app.title, app.id)
            } else {
                app.title.clone()
            };
            let mut checkbox = ui::widget::Checkbox::from_state(&selected_here)
                .with_id(&id)
                .with_label(label)
                .with_enabled(!required);
            if !required {
                let access = self.clone();
                let displayed = selected_here.clone();
                let mut chosen_by_person = false;
                checkbox = checkbox.on_view(&id, move |checked| {
                    if access.at_own_conversation() {
                        chosen_by_person = checked;
                    }
                    // A remote invocation cannot add an app to the person's next request.
                    displayed.set_if_changed(chosen_by_person);
                });
            }
            selected.push((app.id.clone(), selected_here));
            options = options.push(Box::new(checkbox));
        }
        let access = self.clone();
        let send = Button::new(
            "chat-assistant-grant-submit-apps",
            "chat-assistant-grant-submit-apps",
        )
        .on_effect(move || {
            let apps = selected
                .iter()
                .filter(|(_, checked)| checked.get())
                .map(|(id, _)| id.clone())
                .collect();
            if let Ok(serde_json::Value::Object(args)) =
                serde_json::to_value(AgentGrantApps { apps })
            {
                access.ask(AgentGrantAction::RequestApps, InvokeArgument::Json(args));
            }
        });
        let panel = self.panel.clone();
        let cancel = Button::new(
            "chat-assistant-grant-cancel-apps",
            "chat-assistant-grant-cancel-apps",
        )
        .on_view(move || panel.set(Panel::Conversation));
        Box::new(
            Stack::stack()
                .gap(ui::tokens::Rhythm::Tight)
                .push(Text::new("chat-assistant-grant-pick-apps").into_widget())
                .push(Box::new(
                    ui::widget::Scrollable::new_vertical()
                        .with_semantic_key("chat-assistant-grant-apps")
                        .with_max_height(5.0 * ui::tokens::Metrics::BASE.panel_row_height)
                        .with_child(Box::new(options)),
                ))
                .push(Box::new(
                    Stack::row()
                        .wrap()
                        .push(send.into_widget())
                        .push(cancel.into_widget()),
                )),
        )
    }
}
