//! What an app's resource command and a put into a workspace's folder arrive with: the
//! certified route and caller, and the call a put is made for.

/// A resource command's certified route and caller, supplied by the broker.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ResourceRequestContext {
    app_id: String,
    workspace_id: String,
    caller: tilo_actions::remote::Caller,
    /// The invocations the workspace service delivered to this route's app that it is still carrying out,
    /// each with who invoked it: a write that serves one is that invoker's, an agent acting for
    /// the caller included.
    serving: Vec<(tilo_actions::InvocationId, tilo_actions::Acting)>,
    /// The caller the device opened this route's view for, when it opened one for a caller of
    /// its dispatch: a write the app makes outside an invocation it serves is theirs, limited
    /// to what they may do, and a read answers only what they may read.
    opened_for: Option<tilo_actions::remote::Caller>,
    /// The record the route's channel opened, whose changes `RecordChanges` reads and
    /// `RecordChange` makes.
    record: Option<String>,
    /// Why the caller may not found the route's record in a workspace this device hosts.
    founding_refusal: Option<String>,
    /// The kind of the record the route's channel opened, as the app joined it.
    kind: Option<String>,
    /// Whether the route serves the app's own process, its connection to the device: what it
    /// writes outside an invocation it serves is the app's own. The device's own callers (the
    /// shell, a runtime of the device's) write for the device's person.
    app_process: bool,
}

impl ResourceRequestContext {
    pub fn authenticated(
        app_id: impl Into<String>,
        workspace_id: impl Into<String>,
        caller: tilo_actions::remote::Caller,
    ) -> Self {
        Self {
            app_id: app_id.into(),
            workspace_id: workspace_id.into(),
            caller,
            serving: Vec::new(),
            opened_for: None,
            record: None,
            founding_refusal: None,
            kind: None,
            app_process: false,
        }
    }

    /// The context of a route that serves the app's own process, its connection to the device.
    #[must_use]
    pub const fn of_app_process(mut self) -> Self {
        self.app_process = true;
        self
    }

    /// Whether the route serves the app's own process ([`Self::of_app_process`]).
    #[must_use]
    pub const fn serves_app_process(&self) -> bool {
        self.app_process
    }

    /// The context of a route whose channel opened a record of `kind`.
    #[must_use]
    pub fn of_kind(mut self, kind: Option<String>) -> Self {
        self.kind = kind;
        self
    }

    /// The kind of the record the route's channel opened, when the app named it.
    #[must_use]
    pub fn kind(&self) -> Option<&str> {
        self.kind.as_deref()
    }

    /// The context of a route whose caller may not found its record where this device hosts
    /// the workspace, and why.
    #[must_use]
    pub fn refusing_founding(mut self, refusal: Option<String>) -> Self {
        self.founding_refusal = refusal;
        self
    }

    /// Why the caller may not found the route's record where this device hosts the workspace.
    #[must_use]
    pub fn founding_refusal(&self) -> Option<&str> {
        self.founding_refusal.as_deref()
    }

    /// The context of a route whose channel opened the record `record`.
    #[must_use]
    pub fn on_record(mut self, record: impl Into<String>) -> Self {
        self.record = Some(record.into());
        self
    }

    /// The record the route's channel opened, when it opened one.
    #[must_use]
    pub fn record(&self) -> Option<&str> {
        self.record.as_deref()
    }

    /// The context of a view the device opened for `opened_for`, when it opened it for a caller
    /// of its dispatch rather than for the person at a window of its own.
    #[must_use]
    pub fn opened_for(mut self, opened_for: Option<tilo_actions::remote::Caller>) -> Self {
        self.opened_for = opened_for;
        self
    }

    /// The caller this route's view was opened for, when it was opened for one.
    #[must_use]
    pub const fn viewer(&self) -> Option<&tilo_actions::remote::Caller> {
        self.opened_for.as_ref()
    }

    /// The caller this route reads as: the one its view was opened for, else the caller it
    /// serves. Never the person whose device serves it, unless that is who called.
    #[must_use]
    pub fn reader(&self) -> &tilo_actions::remote::Caller {
        self.opened_for.as_ref().unwrap_or(&self.caller)
    }

    /// The context, with the invocations `serving` the workspace service delivered to its app and is
    /// waiting on, each with who invoked it.
    /// A route's context as the broker certifies it, for a test that stands in for the broker.
    #[cfg(any(test, feature = "test-support"))]
    #[must_use]
    pub fn for_tests(
        app_id: impl Into<String>,
        workspace_id: impl Into<String>,
        caller: tilo_actions::remote::Caller,
    ) -> Self {
        Self::authenticated(app_id, workspace_id, caller)
    }

    #[must_use]
    pub fn serving<A: Into<tilo_actions::Acting>>(
        mut self,
        serving: impl IntoIterator<Item = (tilo_actions::InvocationId, A)>,
    ) -> Self {
        self.serving = serving
            .into_iter()
            .map(|(invocation, acting)| (invocation, acting.into()))
            .collect();
        self
    }

    /// Who invoked `invocation`, and how they acted, when it is one the workspace service
    /// delivered to this route's app and the app is still carrying out.
    #[must_use]
    pub fn invoker_of(&self, invocation: &str) -> Option<&tilo_actions::Acting> {
        self.serving
            .iter()
            .find(|(id, _)| id.0.to_string() == invocation)
            .map(|(_, acting)| acting)
    }

    #[must_use]
    pub fn app_id(&self) -> &str {
        &self.app_id
    }

    #[must_use]
    pub fn workspace_id(&self) -> &str {
        &self.workspace_id
    }

    #[must_use]
    pub fn caller(&self) -> &tilo_actions::remote::Caller {
        &self.caller
    }
}

/// One entry of a workspace's folder, as its host lists it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FilesEntry {
    /// The entry's path in the workspace, never empty.
    pub path: String,
    /// Whether it is a folder rather than a file.
    pub folder: bool,
}

/// The call a put into a workspace's folder is made for: the context the tree's change is made
/// in (who asked, in which run and work), and the request they sent. The change keeps the request
/// as its caller signed it, so every replica checks who asked; a person's call nobody signed is
/// signed by the device carrying it out for its own person, and an agent's never is
/// ([`KEYLESS_AGENT`]).
pub struct FilesPutCall<'a> {
    pub context: tilo_actions::ActionContext,
    pub request: &'a tilo_actions::remote::InvokeRequest,
}

/// What an agent asking to put files under no key of its own is told: every device that takes
/// the files checks who asked against the call's signature, so an agent's put is the call it
/// signed with the key its person's root certified for it.
pub const KEYLESS_AGENT: &str = "This agent can't put files into the workspace's folder: it asks \
     under no key of its own. Each device that takes the files checks who asked, so an agent \
     puts them with the key its person approved for it. Connect the agent again so its person's \
     device gives it one.";
