//! Consented, device-local existing-identity recovery. No active persona or
//! source backup is modified; only authenticated canonical custody is adopted.
use super::super::{prepared_identity::PreparedIdentity, strings::t};
use std::{
    path::PathBuf,
    rc::Rc,
    sync::{Arc, mpsc},
};
use ui::widget::{Button, Field, LabelPlacement, Text, TextInput, TextInputKind, Widget};
use zeroize::Zeroizing;

pub(super) struct ExistingIdentity {
    source: Option<PathBuf>,
    password: ui::LocalState<String>,
    worker: Option<mpsc::Receiver<Result<Arc<PreparedIdentity>, ()>>>,
    picker: Option<ui::PortalChannel>,
    prepared: Option<Arc<PreparedIdentity>>,
    state: &'static str,
}
impl Default for ExistingIdentity {
    fn default() -> Self {
        Self {
            source: None,
            password: ui::LocalState::new(String::new()),
            worker: None,
            picker: None,
            prepared: None,
            state: "",
        }
    }
}

impl ExistingIdentity {
    pub(super) fn resume(prepared: Arc<PreparedIdentity>) -> Self {
        Self {
            prepared: Some(prepared),
            state: "ready",
            ..Self::default()
        }
    }
    pub fn busy(&self) -> bool {
        self.worker.is_some()
    }
    pub fn active(&self) -> bool {
        self.busy() || self.picker.is_some()
    }
    pub fn selected(&self) -> bool {
        self.source.is_some()
    }
    pub fn ready(&self) -> bool {
        self.prepared.is_some()
    }
    pub fn select(&mut self, path: String) -> Result<(), &'static str> {
        if self.busy() {
            return Err("installer-existing-busy");
        }
        let path = PathBuf::from(path);
        if !path.is_absolute() {
            return Err("installer-existing-file-invalid");
        }
        self.prepared = None;
        self.source = Some(path);
        self.password.set(String::new());
        self.state = "selected";
        Ok(())
    }
    pub fn back(&mut self) -> Result<(), &'static str> {
        if self.active() {
            return Err("installer-existing-busy");
        }
        self.source = None;
        self.password.set(String::new());
        self.state = "";
        Ok(())
    }
    pub fn choose(&mut self) -> Result<(), &'static str> {
        if self.active() {
            return Err("installer-existing-busy");
        }
        self.picker = Some(
            ui::PickRequest::open(
                &t("installer-existing-picker"),
                &t("installer-existing-choose"),
            )
            .send(),
        );
        Ok(())
    }
    pub fn set_password(&self, password: Zeroizing<String>) -> Result<(), &'static str> {
        if self.busy() || self.source.is_none() || password.len() > 4096 {
            return Err("installer-existing-password-required");
        }
        self.password.set(password.to_string());
        Ok(())
    }
    pub fn use_current(&mut self) -> Result<(), &'static str> {
        imager_backend::prepared_identity::require_supported()
            .map_err(|_| "installer-preview-identity-unavailable")?;
        if self.active() {
            return Err("installer-existing-busy");
        }
        let directory = super::personal::preparation_directory()
            .ok_or("installer-preview-identity-unavailable")?;
        let (sender, receiver) = mpsc::channel();
        self.worker = Some(receiver);
        self.state = "restoring";
        std::thread::spawn(move || {
            let result = (|| {
                let current = Arc::new(PreparedIdentity::review_current().map_err(|_| ())?);
                super::personal::ensure_private_tree(directory.parent().ok_or(())?)
                    .map_err(|_| ())?;
                PreparedIdentity::from_current(&directory, current)
                    .map(Arc::new)
                    .map_err(|_| ())
            })();
            let _ = sender.send(result);
        });
        Ok(())
    }
    pub fn current_view(&self) -> Box<dyn Widget> {
        let key = if self.busy() {
            "installer-existing-restoring"
        } else if self.state == "failed" {
            "installer-current-identity-unavailable"
        } else {
            "installer-preview-current-body"
        };
        if let Some(prepared) = &self.prepared
            && let Ok(metadata) = prepared.metadata()
        {
            return Box::new(
                crate::page::related()
                    .push(Box::new(Text::new(&metadata.display_name)))
                    .push(Box::new(Text::new(&t("installer-existing-connected")))),
            );
        }
        let notice = Text::new(&t(key));
        Box::new(if self.state == "failed" {
            notice
                .with_id("installer.existing.failure")
                .with_live(ui::a11y::Politeness::Assertive)
        } else {
            notice
        })
    }
    pub fn restore(&mut self) -> Result<(), &'static str> {
        imager_backend::prepared_identity::require_supported()
            .map_err(|_| "installer-preview-identity-unavailable")?;
        if self.busy() {
            return Err("installer-existing-busy");
        }
        let source = self
            .source
            .clone()
            .ok_or("installer-existing-file-required")?;
        let password = Zeroizing::new(self.password.get());
        self.password.set(String::new());
        if password.is_empty() {
            return Err("installer-existing-password-required");
        }
        let directory = super::personal::preparation_directory()
            .ok_or("installer-preview-identity-unavailable")?;
        let (sender, receiver) = mpsc::channel();
        self.worker = Some(receiver);
        self.state = "restoring";
        std::thread::spawn(move || {
            let result = (|| {
                let parent = directory.parent().ok_or(())?;
                super::personal::ensure_private_tree(parent).map_err(|_| ())?;
                PreparedIdentity::restore_backup(&directory, &source, password)
                    .map(Arc::new)
                    .map_err(|_| ())
            })();
            let _ = sender.send(result);
        });
        Ok(())
    }
    pub fn prepared(&self) -> Option<Arc<PreparedIdentity>> {
        self.prepared.clone()
    }
    pub fn poll(&mut self) -> bool {
        let mut changed = false;
        if let Some(picker) = &self.picker {
            match picker.try_recv() {
                Ok(ui::PortalOutcome::Opened(path)) => {
                    self.picker = None;
                    let _ = self.select(path.to_string_lossy().into_owned());
                    changed = true;
                }
                Ok(ui::PortalOutcome::Cancelled) => {
                    self.picker = None;
                    changed = true;
                }
                Ok(ui::PortalOutcome::Failed(_)) | Err(mpsc::TryRecvError::Disconnected) => {
                    self.picker = None;
                    self.state = "failed";
                    changed = true;
                }
                Err(mpsc::TryRecvError::Empty) => {}
            }
        }
        let Some(worker) = &self.worker else {
            return changed;
        };
        let result = match worker.try_recv() {
            Ok(result) => result,
            Err(mpsc::TryRecvError::Empty) => return changed,
            Err(mpsc::TryRecvError::Disconnected) => Err(()),
        };
        self.worker = None;
        match result {
            Ok(prepared) => {
                self.prepared = Some(prepared);
                self.state = "ready";
            }
            Err(()) => self.state = "failed",
        }
        true
    }
    pub fn projection(&self) -> serde_json::Value {
        serde_json::json!({"state": if self.state.is_empty() { "unconnected" } else { self.state }, "public_key": self.prepared.as_ref().map(|prepared| hex::encode(prepared.public_key()))})
    }
    pub fn view(
        &self,
        choose: Rc<dyn Fn()>,
        edit: Rc<dyn Fn(String)>,
        submit: Rc<dyn Fn()>,
    ) -> Box<dyn Widget> {
        let mut body = crate::page::stack();
        if self.busy() {
            return Box::new(body.push(Box::new(Text::new(&t("installer-existing-restoring")))));
        }
        if let Some(prepared) = &self.prepared {
            if let Ok(metadata) = prepared.metadata() {
                let avatar = ui::widget::Avatar::person(
                    &metadata.display_name,
                    hex::encode(metadata.group_public_key),
                );
                let avatar = match metadata.profile_photo.as_ref() {
                    Some(photo) => avatar.with_profile_photo(photo),
                    None => avatar,
                };
                return Box::new(
                    body.push(Box::new(avatar))
                        .push(Box::new(Text::new(&metadata.display_name)))
                        .push(Box::new(Text::new(&t("installer-existing-connected")))),
                );
            }
        }
        if self.source.is_some() {
            body = body.push(Box::new(
                Field::new(
                    t("installer-existing-password-label"),
                    TextInput::from_state(&self.password)
                        .with_id("installer.existing.password")
                        .with_kind(TextInputKind::Password)
                        .with_secret(true)
                        .on_change(move |value| edit(value.to_string()))
                        .on_submit(move |_| submit()),
                )
                .with_label_placement(Some(LabelPlacement::Above)),
            ));
        } else {
            body = body.push(Box::new(Text::new(&t("installer-existing-file-body"))));
        }
        if self.state == "failed" {
            body = body.push(Box::new(
                Text::new(&t("installer-existing-failed"))
                    .with_id("installer.existing.failure")
                    .with_live(ui::a11y::Politeness::Assertive),
            ));
        }
        Box::new(
            body.push(Box::new(
                Button::new("installer.existing.choose", &t("installer-existing-choose"))
                    .on_click(move || choose()),
            )),
        )
    }
}
