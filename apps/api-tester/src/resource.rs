//! The workspace's verbs and the app.
//!
//! Renaming a collection, a saved request, an environment or a variable,
//! choosing the method, typing the URL or the body, editing a row and
//! choosing the active environment are the built-in field edits of their
//! paths (`Workspace::paths().request.url.splice(..)`). The methods below are
//! the operations that are more than setting a field: creating and deleting,
//! saving and opening requests, and recording what was sent. Sending itself
//! reaches the network from this device: an effect
//! ([`crate::actions::ApiTesterEffect`]) whose completion records the history
//! entry through [`Workspace::RECORD_SENT`].

use std::rc::Rc;

use ui::framework::{EffectAnswer, EffectCx, ResourceApp, ResourceSurface, SurfaceCx, ViewCx};
use ui::i18n::EmbeddedCatalog;
use ui::tilo_resource::items::item_resource_id;
use ui::tilo_resource::{self, ActionCx, Atom, Id, Invoke, Refusal, SearchEntry, resource};
use ui::widget::Element;

use crate::actions::ApiTesterEffect;
use crate::app::ApiTesterApp;
use crate::model::HttpMethod;
use crate::record::{Environment, HISTORY_LIMIT, RequestRecord, SavedCollection, Sent, Variable};

pub use crate::record::Workspace;

/// API Tester's Fluent catalogs, source locale first.
pub const LOCALES: &[(&str, &str)] = &[("en-US", include_str!("../locales/en-US.ftl"))];

#[resource(
    kind = "api-tester.workspace",
    label = "API workspace",
    initialize = Workspace::blank,
    search = Workspace::search
)]
impl Workspace {
    /// Start a new request in the builder.
    #[action]
    pub fn new_request(&mut self, cx: &mut ActionCx) {
        self.request = RequestRecord::named("New request");
        self.opened = Atom(cx.uuid().to_string());
    }

    /// Create a collection.
    #[action]
    pub fn new_collection(&mut self, cx: &mut ActionCx, #[param("Collection name")] name: String) {
        self.collections.push(
            Id::from_raw(cx.uuid().to_string()),
            SavedCollection {
                name: name.trim().to_owned(),
                ..SavedCollection::default()
            },
        );
    }

    /// Delete a collection and the requests it holds.
    #[action(destructive, item = collections)]
    pub fn delete_collection(&mut self, collection: &Id) -> Result<(), Refusal> {
        self.collections
            .remove(collection)
            .map(drop)
            .ok_or_else(|| Refusal::new("no such collection"))
    }

    /// Add a new request to a collection.
    #[action(item = collections)]
    pub fn add_request(
        &mut self,
        cx: &mut ActionCx,
        collection: &Id,
        #[param("Request name")] name: String,
    ) -> Result<(), Refusal> {
        let collection = self
            .collections
            .get_mut(collection)
            .ok_or("no such collection")?;
        collection.requests.push(
            Id::from_raw(cx.uuid().to_string()),
            RequestRecord::named(name.trim()),
        );
        Ok(())
    }

    /// Save the request in the builder to a collection.
    ///
    /// A request the builder opened from that collection is saved over;
    /// otherwise it is added, and the builder holds the saved request.
    #[action(item = collections)]
    pub fn save_request(
        &mut self,
        cx: &mut ActionCx,
        collection: &Id,
        #[param("Request name; the builder's when none")] name: Option<String>,
    ) -> Result<(), Refusal> {
        if let Some(name) = name.map(|name| name.trim().to_owned())
            && !name.is_empty()
        {
            self.request.name = name;
        }
        let opened = self.opened();
        let elsewhere = self
            .saved(&opened)
            .is_some_and(|(holder, _)| holder != collection);
        let target = self
            .collections
            .get_mut(collection)
            .ok_or("no such collection")?;
        if let Some(saved) = target.requests.get_mut(&opened) {
            saved.clone_from(&self.request);
            return Ok(());
        }
        let id = if elsewhere || opened.as_str().is_empty() {
            Id::from_raw(cx.uuid().to_string())
        } else {
            opened
        };
        target.requests.push(id.clone(), self.request.clone());
        self.opened = Atom(id.as_str().to_owned());
        Ok(())
    }

    /// Open a saved request in the builder.
    #[action(item = collections)]
    pub fn open_request(
        &mut self,
        collection: &Id,
        #[param("Id of the saved request")] request: Id,
    ) -> Result<(), Refusal> {
        let saved = self
            .collections
            .get(collection)
            .and_then(|collection| collection.requests.get(&request))
            .ok_or("no such request")?;
        self.request = saved.clone();
        self.opened = Atom(request.as_str().to_owned());
        Ok(())
    }

    /// Delete a saved request.
    #[action(destructive, item = collections)]
    pub fn delete_request(
        &mut self,
        collection: &Id,
        #[param("Id of the saved request")] request: Id,
    ) -> Result<(), Refusal> {
        self.collections
            .get_mut(collection)
            .and_then(|collection| collection.requests.remove(&request))
            .map(drop)
            .ok_or_else(|| Refusal::new("no such request"))
    }

    /// Create an environment.
    #[action]
    pub fn new_environment(
        &mut self,
        cx: &mut ActionCx,
        #[param("Environment name")] name: String,
    ) {
        self.environments.push(
            Id::from_raw(cx.uuid().to_string()),
            Environment {
                name: name.trim().to_owned(),
                ..Environment::default()
            },
        );
    }

    /// Delete an environment and its variables.
    #[action(destructive, item = environments)]
    pub fn delete_environment(&mut self, environment: &Id) -> Result<(), Refusal> {
        self.environments
            .remove(environment)
            .ok_or_else(|| Refusal::new("no such environment"))?;
        if self.active_environment.as_ref() == Some(environment) {
            self.active_environment = None;
        }
        Ok(())
    }

    /// Use an environment's variables in what is sent.
    #[action(item = environments)]
    pub fn use_environment(&mut self, environment: &Id) -> Result<(), Refusal> {
        if !self.environments.contains(environment) {
            return Err(Refusal::new("no such environment"));
        }
        self.active_environment = Some(environment.clone());
        Ok(())
    }

    /// Add a variable to an environment.
    ///
    /// A secret variable's value is set on each device, never here.
    #[action(item = environments)]
    pub fn add_variable(
        &mut self,
        cx: &mut ActionCx,
        environment: &Id,
        #[param("Variable key")] key: String,
        #[param("Its value; none for a secret")] value: Option<String>,
        #[param("Whether the value is a credential kept on each device")] secret: Option<bool>,
    ) -> Result<(), Refusal> {
        let secret = secret.unwrap_or(false);
        let environment = self
            .environments
            .get_mut(environment)
            .ok_or("no such environment")?;
        environment.variables.push(
            Id::from_raw(cx.uuid().to_string()),
            Variable {
                key: key.trim().to_owned(),
                value: if secret {
                    String::new()
                } else {
                    value.unwrap_or_default()
                },
                enabled: true,
                secret,
            },
        );
        Ok(())
    }

    /// Open a sent request again in the builder, as a new request.
    #[action(item = history)]
    pub fn open_sent(&mut self, cx: &mut ActionCx, sent: &Id) -> Result<(), Refusal> {
        let sent = self.history.get(sent).ok_or("no such request")?;
        self.request = RequestRecord {
            method: sent.method,
            url: sent.url.0.clone(),
            ..RequestRecord::named("New request")
        };
        self.opened = Atom(cx.uuid().to_string());
        Ok(())
    }

    /// Record a request that was sent.
    #[action]
    pub fn record_sent(
        &mut self,
        cx: &mut ActionCx,
        #[param("The HTTP method")] method: String,
        #[param("The URL as sent")] url: String,
        #[param("The response status")] status: u16,
        #[param("The round-trip time in milliseconds")] duration_ms: u64,
    ) {
        let sent = Sent::new(
            HttpMethod::from_str(&method),
            &url,
            status,
            duration_ms,
            cx.now_ms(),
        );
        self.history
            .insert(0, Id::from_raw(cx.uuid().to_string()), sent);
        while self.history.len() > HISTORY_LIMIT {
            let Some(oldest) = self.history.ids().next_back().cloned() else {
                break;
            };
            self.history.remove(&oldest);
        }
    }
}

impl Workspace {
    /// A new workspace opens on one blank request, the same on every device.
    fn blank() -> Self {
        Self {
            request: RequestRecord::named("New request"),
            opened: Atom(uuid::Uuid::nil().to_string()),
            ..Self::default()
        }
    }

    /// What system search finds: records by their names and routes, never a
    /// body, a credential or a variable's value. Each hit is anchored at the
    /// collection, environment or sent request it belongs to.
    fn search(&self) -> Vec<SearchEntry> {
        let anchored = |field: &str, id: &Id, title: String| {
            SearchEntry::new(title, String::new()).anchored(item_resource_id::<Self>(field, id))
        };
        let mut entries = Vec::new();
        for (id, collection) in self.collections.iter() {
            entries.push(anchored("collections", id, collection.name.clone()));
            for request in collection.requests.values() {
                entries.push(anchored(
                    "collections",
                    id,
                    format!("{} {}", request.method.as_str(), request.name),
                ));
            }
        }
        for (id, environment) in self.environments.iter() {
            entries.push(anchored("environments", id, environment.name.clone()));
            for variable in environment.variables.values() {
                entries.push(anchored(
                    "environments",
                    id,
                    format!("Variable {}", variable.key),
                ));
            }
        }
        for (id, sent) in self.history.iter() {
            entries.push(anchored("history", id, sent.route.0.clone()));
        }
        entries
    }
}

/// Framework app.
pub struct ApiTesterShell;

impl ApiTesterShell {
    /// The view state a screenshot scenario stages: a contextual form, the
    /// rail, the URL's focus, a failed send.
    pub const SCENARIOS: &'static [&'static str] = &[
        "empty",
        "rail-open",
        "error",
        "focus-url",
        "form-new-collection",
        "form-save-request",
        "form-rename-collection",
        "form-rename-request",
        "form-new-environment",
        "form-rename-environment",
        "form-new-variable",
        "form-delete-variable",
    ];
}

impl ResourceApp for ApiTesterShell {
    type Resource = Workspace;
    const ID: &'static str = "so.tilo.api-tester";
    const TITLE: &'static str = "API Tester";
    const ICON: Option<&'static str> = Some("api-tester");

    fn view(cx: &mut ViewCx<Workspace>) -> Element {
        cx.surface(ApiTesterApp::new_with_actions(&cx.actions()))
    }

    fn catalog() -> Option<&'static EmbeddedCatalog> {
        Some(&crate::locales::CATALOG)
    }

    fn scenarios() -> &'static [&'static str] {
        Self::SCENARIOS
    }

    /// Every scenario but the empty one shows the sample workspace.
    fn scenario(name: Option<&str>) -> Vec<Invoke<Workspace>> {
        if name == Some("empty") {
            return Vec::new();
        }
        crate::seed::workspace()
    }
}

impl ResourceSurface<Workspace, ApiTesterEffect> for ApiTesterApp {
    fn present(&mut self, state: &Rc<Workspace>) {
        self.present_workspace(state);
    }

    fn attach(&mut self, cx: SurfaceCx<Workspace>) {
        self.attach_surface(&cx);
    }

    fn read_only_changed(&mut self, read_only: bool) {
        self.set_read_only(read_only);
    }

    fn stage(&mut self, scenario: Option<&str>) {
        self.stage_scenario(scenario);
    }

    fn effect(
        &mut self,
        effect: ApiTesterEffect,
        cx: &mut EffectCx<Workspace, Self>,
    ) -> EffectAnswer {
        self.answer_effect(effect, cx)
    }
}

#[cfg(test)]
mod tests;
