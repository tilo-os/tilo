//! An agent shows in a room's Chat under its own name, as a person's
//! window announces its person when it opens the chat: on opening the
//! room's chat record the agent introduces itself (Chat's `introduce`,
//! housekeeping any member may do), so every member's chat names its
//! messages by the label its person gave it rather than as someone the
//! room has not named.

use tilo_browser_peer::DocumentRoute;

use super::AgentParticipant;

/// The app whose record an agent introduces itself in.
const CHAT: &str = "so.tilo.chat";

/// Chat's action that names its author in the room's roster.
const INTRODUCE: &str = "introduce";

impl AgentParticipant {
    /// Introduce the agent in the chat record just opened at `route`; any
    /// other record is left as it is. A refusal is logged, not raised: the
    /// agent's messages still go out, under the name the room gives them.
    pub(super) fn introduce_in_chat(&mut self, route: &DocumentRoute) {
        if route.app_id != CHAT {
            return;
        }
        let Some(app) = self.apps.get(route) else {
            return;
        };
        let Some(address) = app
            .survey()
            .into_iter()
            .map(|descriptor| descriptor.address)
            .find(|address| address.action == INTRODUCE)
        else {
            return;
        };
        let name = self.identity.delegation.label.trim().to_owned();
        if name.is_empty() {
            return;
        }
        let initials: String = name
            .split_whitespace()
            .filter_map(|word| word.chars().next())
            .flat_map(char::to_uppercase)
            .take(2)
            .collect();
        let [red, green, blue] = self.identity.delegation.color;
        let arguments = serde_json::json!({
            "name": name,
            "initials": initials,
            "color": u32::from(red ^ green ^ blue) & 7,
        });
        let Some(arguments) = arguments.as_object() else {
            return;
        };
        if let Err(error) = app.invoke(&address, arguments, false) {
            tracing::debug!(%error, "agent: could not introduce itself in the chat");
            return;
        }
        match app.take_mutation() {
            Ok(Some(mutation)) => {
                self.generation = self.generation.wrapping_add(1);
                if let Err(error) = self.peer.publish(route.clone(), &mutation) {
                    tracing::warn!(%error, "agent: its introduction could not be committed");
                }
            }
            Ok(None) => {}
            Err(error) => tracing::warn!(%error, "agent: its introduction was not admitted"),
        }
    }
}
