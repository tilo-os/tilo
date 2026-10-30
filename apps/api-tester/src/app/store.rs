//! The API tester as a bound view: the [`Store`] the window shares with the
//! view's effect handler, and [`window`], which takes the window into the
//! view.
//!
//! The effects answer here, from the workspace the document holds and what
//! this device keeps: the response, the request in flight and the secrets.
//! What they ask of the window itself (a sheet to open, a pane to show
//! again) waits until the window next looks.

use std::cell::RefCell;
use std::collections::HashMap;
use std::fmt::Write as _;
use std::rc::Rc;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use ui::framework::{EffectAnswer, EffectCx, Effected, ViewCx};
use ui::secrets::SecretStore;
use ui::tilo_resource::Model;
use ui::widget::Element;

use super::ApiTesterApp;
use crate::actions::ApiTesterEffect;
use crate::commands::{Cmd, FormMode, FormTarget, Sink};
use crate::http::{self, Outgoing};
use crate::model::{Auth, Environment, HttpMethod, Id, Request, ResponseData};
use crate::resource::{Workspace, workspace as verbs};
use crate::vars;

/// Where a request's credential is kept on this device, by request.
const CREDENTIAL: &str = "request-credential";
/// Where a secret variable's value is kept on this device, by variable.
pub(super) const SECRET_VARIABLE: &str = "variable-value";

/// What the effects changed that the window shows, taken when it next looks.
#[derive(Default)]
#[allow(
    clippy::struct_excessive_bools,
    reason = "each flag names an independent part of the view"
)]
pub(super) struct Shown {
    /// The response pane: what was sent, what came back, what failed.
    pub(super) response: bool,
    /// A response arrived, shown pretty from its first line.
    pub(super) arrived: bool,
    /// The bar's Send and Cancel.
    pub(super) url_bar: bool,
    /// Anything else to draw again.
    pub(super) redraw: bool,
}

/// What the window and the view's effect handler share.
pub(crate) struct Store {
    /// Where credentials and secret variables are kept on this device; `None`
    /// for a window outside the framework.
    secrets: Option<SecretStore>,
    /// Credentials of requests made before they named theirs, read once.
    credentials: HashMap<Id, String>,
    /// What the window's controls push, for an effect that opens a sheet.
    sink: Sink,
    pub(crate) response: Option<ResponseData>,
    /// A request is in flight.
    pub(crate) sending: bool,
    /// Set when the person cancels the request in flight: the send watches it
    /// and stops, rather than holding the app's only primary action forever.
    pub(super) send_cancel: Arc<AtomicBool>,
    /// A copy whose outcome the clipboard reports after it returned: a copy
    /// that fails later is said, not confirmed.
    pub(super) copy: ui::CopyWatch,
    /// What the last copy did, shown once in the response pane.
    pub(crate) notice: Option<String>,
    pub(crate) error: Option<String>,
    /// What changed since the window last looked.
    pub(super) shown: Shown,
    /// The scenario a capture staged, staged by the window when it next
    /// looks.
    pub(super) staged: Option<Staged>,
}

/// A capture's scenario (`None` for a plain capture).
pub(super) struct Staged(pub(super) Option<String>);

/// The [`Store`] as the window and the view share it.
pub(crate) type Shared = Rc<RefCell<Store>>;

impl Store {
    pub(super) fn new(sink: Sink, response: Option<ResponseData>) -> Self {
        Self {
            secrets: None,
            credentials: HashMap::new(),
            sink,
            response,
            sending: false,
            send_cancel: Arc::new(AtomicBool::new(false)),
            copy: ui::CopyWatch::default(),
            notice: None,
            error: None,
            shown: Shown::default(),
            staged: None,
        }
    }

    /// Keep secrets in `secrets` from now on.
    pub(super) fn keep_secrets_in(&mut self, secrets: SecretStore) {
        self.secrets = Some(secrets);
        self.credentials.clear();
    }

    pub(super) fn secret(&self, purpose: &'static str, item: &str) -> Option<String> {
        let secret = self
            .secrets
            .as_ref()?
            .item(purpose, item)
            .get_blocking()
            .ok()??;
        String::from_utf8(secret.expose().to_vec()).ok()
    }

    pub(super) fn keep_secret(
        &self,
        purpose: &'static str,
        item: &str,
        value: &str,
    ) -> Result<(), String> {
        let secrets = self
            .secrets
            .as_ref()
            .ok_or_else(|| "this window keeps no secrets".to_string())?;
        secrets
            .item(purpose, item)
            .put_blocking(value.as_bytes())
            .map_err(|error| error.to_string())
    }

    pub(super) fn credential(&mut self, id: &Id, reference: &str) -> Result<String, String> {
        if !reference.is_empty() {
            let store = self
                .secrets
                .as_ref()
                .ok_or("this window keeps no secrets")?;
            let auth = crate::credentials::resolve(
                &Auth::SecretRef {
                    id: reference.into(),
                    scheme: String::new(),
                },
                store,
            )
            .map_err(str::to_owned)?;
            return Ok(auth.secret().unwrap_or_default().to_owned());
        }
        // Compatibility with credentials created before immutable references.
        if let Some(known) = self.credentials.get(id) {
            return Ok(known.clone());
        }
        let found = self.secret(CREDENTIAL, id.as_str()).unwrap_or_default();
        self.credentials.insert(id.clone(), found.clone());
        Ok(found)
    }

    /// The active environment's variables, secret ones read from this device.
    pub(super) fn env_vars(
        &self,
        environments: &[Environment],
        active: Option<&Id>,
    ) -> HashMap<String, String> {
        match active.and_then(|id| environments.iter().find(|e| &e.id == id)) {
            Some(env) => {
                let variables: Vec<_> = env
                    .variables
                    .iter()
                    .map(|variable| {
                        let mut variable = variable.clone();
                        if variable.secret {
                            variable.value = self
                                .secret(SECRET_VARIABLE, &variable.id)
                                .unwrap_or_default();
                        }
                        variable
                    })
                    .collect();
                vars::var_map(&variables)
            }
            None => HashMap::new(),
        }
    }

    /// The request the workspace holds, with its credential from this device.
    fn request_with_credential(&mut self, workspace: &Workspace) -> Result<Request, String> {
        let mut request = workspace.request_shown();
        let credential = self.credential(&request.id, &workspace.request.auth.reference)?;
        request.auth = request.auth.with_secret(&credential);
        Ok(request)
    }

    /// Answer one of the API tester's effects, invoked by a control, the
    /// palette, an agent or a script.
    fn answer(
        &mut self,
        effect: ApiTesterEffect,
        me: &Shared,
        cx: &mut EffectCx<Workspace>,
    ) -> EffectAnswer {
        match effect {
            ApiTesterEffect::ImportLegacy { snapshot } => self.import_legacy(snapshot, cx),
            ApiTesterEffect::PickEnvironment { choice } => {
                self.sink.borrow_mut().push(Cmd::PickEnvironment(choice));
                Ok(Effected::done())
            }
            ApiTesterEffect::Send => {
                let workspace = cx.live().get();
                let request = self.request_with_credential(&workspace)?;
                let environments = workspace.environments_shown();
                let vars = self.env_vars(&environments, workspace.active_environment.as_ref());
                let out = http::build_outgoing(&request, &vars);
                self.start_send(request.method, out, me, cx)
            }
            ApiTesterEffect::SendSaved { request } => {
                let workspace = cx.live().get();
                let request = Id::from_raw(request);
                let Some((collection, saved)) = workspace.saved(&request) else {
                    return Err("saved request no longer exists".to_string());
                };
                let mut outgoing = saved.shown(&request);
                let reference = saved.auth.reference.clone();
                cx.submit(Workspace::OPEN_REQUEST.with(verbs::OpenRequest {
                    collection: collection.clone(),
                    request,
                }));
                let credential = self.credential(&outgoing.id, &reference)?;
                outgoing.auth = outgoing.auth.with_secret(&credential);
                let environments = workspace.environments_shown();
                let vars = self.env_vars(&environments, workspace.active_environment.as_ref());
                let out = http::build_outgoing(&outgoing, &vars);
                self.start_send(outgoing.method, out, me, cx)
            }
            ApiTesterEffect::CancelSend => {
                self.cancel_send();
                Ok(Effected::done())
            }
            ApiTesterEffect::CopyResponse { part } => {
                let copied = match part.as_str() {
                    "body" => self.copy_body(),
                    "headers" => self.copy_headers(),
                    "request" => self.copy_request(&cx.live().get().request_shown()),
                    other => Err(format!("there is nothing called {other} to copy")),
                };
                copied.map(|()| Effected::done())
            }
            ApiTesterEffect::SetCredential { secret } => self.set_credential(&secret, cx),
            ApiTesterEffect::SetSecretValue { variable, value } => {
                let secret = cx
                    .live()
                    .get()
                    .environments_shown()
                    .iter()
                    .flat_map(|environment| &environment.variables)
                    .find(|row| row.id == variable)
                    .map(|row| row.secret);
                match secret {
                    Some(true) => {
                        self.keep_secret(SECRET_VARIABLE, &variable, &value)?;
                        self.shown.redraw = true;
                        Ok(Effected::done())
                    }
                    Some(false) => {
                        Err("this variable's value is shared; set it as a document edit".into())
                    }
                    None => Err("no such variable".into()),
                }
            }
            ApiTesterEffect::RenameCollection { collection } => {
                self.rename_collection(&Id::from_raw(collection), cx)
            }
            ApiTesterEffect::RequestMenu { action } => {
                let cmd = match action.as_str() {
                    "Save request…" => {
                        Cmd::OpenForm(FormMode::SaveRequest, FormTarget::default())
                    }
                    "Rename request…" => {
                        Cmd::OpenForm(FormMode::RenameRequest, FormTarget::default())
                    }
                    "Delete request…" => Cmd::DeleteRequest(FormTarget::default()),
                    _ => return Err(format!("unknown request action `{action}`")),
                };
                self.sink.borrow_mut().push(cmd);
                Ok(Effected::done())
            }
            ApiTesterEffect::EnvironmentMenu { action } => {
                let mode = match action.as_str() {
                    "Rename environment…" => FormMode::RenameEnvironment,
                    "Add variable…" => FormMode::NewVariable,
                    "Delete variable…" => FormMode::DeleteVariable,
                    _ => return Err(format!("unknown environment action `{action}`")),
                };
                self.sink
                    .borrow_mut()
                    .push(Cmd::OpenForm(mode, FormTarget::default()));
                Ok(Effected::done())
            }
        }
    }

    fn rename_collection(&self, collection: &Id, cx: &EffectCx<Workspace>) -> EffectAnswer {
        if !cx.live().get().collections.contains(collection) {
            return Err("no such collection".into());
        }
        self.sink.borrow_mut().push(Cmd::OpenForm(
            FormMode::RenameCollection,
            FormTarget {
                collection: Some(collection.clone()),
                sent: None,
                request: None,
            },
        ));
        Ok(Effected::done())
    }

    fn import_legacy(&self, snapshot: String, cx: &mut EffectCx<Workspace>) -> EffectAnswer {
        use base64::Engine;
        let before = cx.live().get();
        if *before != Workspace::blank() {
            return Err("Import requires a new, empty workspace".into());
        }
        let bytes = base64::engine::general_purpose::STANDARD
            .decode(snapshot)
            .map_err(|_| "Invalid base64 snapshot")?;
        let secrets = self.secrets.clone().ok_or("this window keeps no secrets")?;
        let imported = ui::rt::app().spawn_pinned(move || async move {
            crate::migration::snapshot(&bytes, &secrets).await
        });
        Ok(cx.spawn(
            move |_| async move {
                imported
                    .await
                    .map_err(|_| "Import task failed".to_string())?
            },
            move |(): &mut (), imported, cx| {
                let imported = imported?;
                if cx.live().get() != before {
                    return Err("Workspace changed during import; use a new workspace".into());
                }
                cx.submit(Workspace::IMPORT_WORKSPACE.with(imported.to_json()));
                Ok(Effected::done())
            },
        ))
    }

    /// Seal `secret` as the open request's credential, keeping only its
    /// reference in the document, which the window follows to show it.
    fn set_credential(&self, secret: &str, cx: &mut EffectCx<Workspace>) -> EffectAnswer {
        let workspace = cx.live().get();
        let request = workspace.request_shown();
        let id = request.id.clone();
        let before = workspace.request.auth.clone();
        let auth = request.auth.with_secret(secret);
        let secrets = self.secrets.clone().ok_or("this window keeps no secrets")?;
        // Build storage futures on their owning runtime thread: browser
        // storage handles must never move between threads.
        let stored = ui::rt::app().spawn_pinned(move || async move {
            crate::credentials::seal_async(&auth, &secrets)
                .await
                .map_err(str::to_owned)
        });
        Ok(cx.spawn(
            move |_| async move {
                stored
                    .await
                    .map_err(|_| "credential storage task failed".to_string())?
            },
            move |(): &mut (), sealed, cx| {
                let Auth::SecretRef { id: reference, .. } = sealed? else {
                    return Err("choose an authentication scheme first".into());
                };
                let now = cx.live().get();
                if now.request_shown().id != id || now.request.auth != before {
                    return Err("request changed while saving authentication; try again".into());
                }
                cx.submit(Workspace::paths().request.auth.reference.replace(reference));
                Ok(Effected::done())
            },
        ))
    }

    /// Stop the request in flight. The send wakes on the flag within a
    /// moment and reports that it was cancelled, so nothing is left looking
    /// like it is still on its way.
    pub(super) fn cancel_send(&mut self) {
        if !self.sending {
            return;
        }
        self.send_cancel.store(true, Ordering::Release);
        self.sending = false;
        self.error = Some(http::CANCELLED.to_owned());
        // Show what the cancel did, the moment it is pressed: the response
        // pane and the bar's Send/Cancel control.
        self.shown.response = true;
        self.shown.url_bar = true;
    }

    /// Copy the response body exactly as the pane shows it — the text a bug
    /// report or a colleague needs.
    pub(super) fn copy_body(&mut self) -> Result<(), String> {
        let Some(response) = &self.response else {
            return self.no_response_to_copy();
        };
        let body = response.body.clone();
        self.copy_to_clipboard(&body, "api-copied-body")
    }

    /// Copy every response header, one `Name: value` per line, so a token in
    /// one of them can be taken without retyping it.
    pub(super) fn copy_headers(&mut self) -> Result<(), String> {
        let Some(response) = &self.response else {
            return self.no_response_to_copy();
        };
        let text = response
            .headers
            .iter()
            .map(|header| format!("{}: {}", header.key, header.value))
            .collect::<Vec<_>>()
            .join("\n");
        self.copy_to_clipboard(&text, "api-copied-headers")
    }

    fn no_response_to_copy(&mut self) -> Result<(), String> {
        let reason = "There is no response to copy yet.";
        self.error = Some(reason.to_owned());
        self.shown.response = true;
        Err(reason.to_owned())
    }

    /// Copy the request as it was authored (**with its `{{variables}}`, and
    /// never a credential, which stays on this device**), the shape a bug
    /// report needs.
    fn copy_request(&mut self, request: &Request) -> Result<(), String> {
        let mut text = format!("{} {}", request.method.as_str(), request.url);
        for row in request.headers.iter().filter(|row| row.enabled) {
            let _ = write!(text, "\n{}: {}", row.key, row.value);
        }
        text.push('\n');
        if !request.body_text.trim().is_empty() {
            text.push('\n');
            text.push_str(&request.body_text);
            text.push('\n');
        }
        self.copy_to_clipboard(&text, "api-copied-request")
    }

    /// Put `text` on the clipboard and say so, or say it did not take.
    fn copy_to_clipboard(&mut self, text: &str, done: &str) -> Result<(), String> {
        let copied = self.copy.watch(ui::try_clipboard_copy(text));
        if copied {
            self.error = None;
            self.notice = Some(ui::i18n::I18nManager::format(done, &[]));
        }
        self.shown.response = true;
        if copied {
            Ok(())
        } else {
            Err("Could not copy to the clipboard.".to_owned())
        }
    }

    fn start_send(
        &mut self,
        method: HttpMethod,
        out: Outgoing,
        me: &Shared,
        cx: &mut EffectCx<Workspace>,
    ) -> EffectAnswer {
        if self.sending {
            return Err("another API request is already running".into());
        }
        // With no address there is nothing to send to; a browser would
        // quietly fetch the page itself. Say what is missing where the
        // answer would show.
        if out
            .url
            .split(['?', '#'])
            .next()
            .unwrap_or_default()
            .trim()
            .is_empty()
        {
            let missing = String::from("Enter a URL to send the request to.");
            self.response = None;
            self.error = Some(missing.clone());
            self.shown.response = true;
            return Err(missing);
        }
        self.sending = true;
        self.error = None;
        self.notice = None;
        self.shown.redraw = true;
        // A fresh flag for this send: an earlier cancel must not stop it.
        let cancel = Arc::new(AtomicBool::new(false));
        self.send_cancel = Arc::clone(&cancel);
        let url = out.url.clone();
        let me = Rc::clone(me);
        Ok(cx.spawn(
            move |_| async move { http::send(&out, &cancel).await },
            move |(): &mut (), sent, cx| {
                let mut store = me.borrow_mut();
                store.sending = false;
                store.shown.redraw = true;
                match sent {
                    Ok(response) => {
                        cx.submit(Workspace::RECORD_SENT.with(verbs::RecordSent {
                            method: method.as_str().to_owned(),
                            url,
                            status: response.status,
                            duration_ms: response.duration_ms,
                        }));
                        let answer = Effected::value(http::answer(&response));
                        store.response = Some(response);
                        store.shown.arrived = true;
                        Ok(answer)
                    }
                    Err(error) => {
                        store.error = Some(error.clone());
                        store.shown.response = true;
                        Err(error)
                    }
                }
            },
        ))
    }
}

/// The window as the view lays it out: it shows the workspace the document
/// holds, answers the effects through the store it shares with them, and
/// stages a capture's scenario.
pub(crate) fn window(cx: &ViewCx<Workspace>) -> Element {
    let mut window = ApiTesterApp::new_with_actions(&cx.actions());
    window.attach(cx);
    let store = Rc::clone(&window.store);
    {
        let me = Rc::clone(&store);
        cx.effects(move |effect: ApiTesterEffect, cx| me.borrow_mut().answer(effect, &me, cx));
    }
    cx.on_stage(move |scenario| {
        store.borrow_mut().staged = Some(Staged(scenario.map(str::to_owned)));
    });
    Box::new(window)
}
