//! Presentation of the backend-owned private Apple sign-in operation.
use super::*;
use imager_backend::account_transfer::Connector;
use zeroize::Zeroizing;

pub(super) struct ICloud {
    flow: imager_backend::icloud_setup::ICloud,
    fields: [ui::LocalState<String>; 3],
}
impl ICloud {
    pub fn new(connector: Connector, lease: Arc<tempfile::TempDir>) -> Self {
        Self {
            flow: imager_backend::icloud_setup::ICloud::new(connector, lease),
            fields: std::array::from_fn(|_| ui::LocalState::new(String::new())),
        }
    }
    pub fn active(&self) -> bool {
        self.flow.active()
    }
    pub fn state(&self) -> &'static str {
        self.flow.state()
    }
    pub fn edit(&mut self, field: &str, value: Zeroizing<String>) -> Result<(), &'static str> {
        self.flow.edit(field, value)?;
        self.refresh_fields();
        Ok(())
    }
    fn refresh_fields(&self) {
        for (index, field) in self.fields.iter().enumerate() {
            if field.get() != self.flow.value(index) {
                field.set(self.flow.value(index).to_owned());
            }
        }
    }
    pub fn confirm(&mut self) -> Result<(), &'static str> {
        let result = self.flow.confirm();
        self.refresh_fields();
        result
    }
    pub fn back(&mut self) -> bool {
        let result = self.flow.back();
        self.refresh_fields();
        result
    }
    pub fn cancel(&mut self) {
        self.flow.cancel();
        self.refresh_fields();
    }
    pub fn poll(&mut self) -> Option<Result<Option<String>, ()>> {
        let result = self.flow.poll().map(|result| result.map_err(|_| ()));
        self.refresh_fields();
        result
    }
    pub fn heading(&self) -> String {
        t(if self.active() {
            "installer-accounts-icloud-wait-heading"
        } else {
            [
                "installer-accounts-icloud-login-heading",
                "installer-accounts-icloud-password-heading",
                "installer-accounts-icloud-code-heading",
            ][self.flow.field()]
        })
    }
    pub fn intro(&self) -> String {
        t(if self.flow.abandoning() {
            "installer-accounts-cancelling-intro"
        } else if self.active() {
            "installer-accounts-icloud-wait-intro"
        } else {
            [
                "installer-accounts-icloud-login-intro",
                "installer-accounts-icloud-password-intro",
                "installer-accounts-icloud-code-intro",
            ][self.flow.field()]
        })
    }
    pub fn view(
        &self,
        dispatch: Rc<dyn Fn(AccountRequest)>,
        edit: Rc<dyn Fn(&str, String)>,
    ) -> Box<dyn ui::widget::Widget> {
        let mut body = page::related();
        if let Some(error) = self.flow.error() {
            body = body.push(Box::new(Text::new(&t(error))));
        }
        if !self.active() {
            let submit = dispatch.clone();
            let key = ["icloud-login", "icloud-password", "icloud-code"][self.flow.field()];
            let label = [
                "installer-accounts-icloud-login-label",
                "installer-accounts-icloud-password-label",
                "installer-accounts-icloud-code-label",
            ][self.flow.field()];
            body = body
                .push(Box::new(
                    Field::new(
                        t(label),
                        TextInput::from_state(&self.fields[self.flow.field()])
                            .with_id(&format!("installer.account.{key}"))
                            .with_kind(if self.flow.field() == 0 {
                                TextInputKind::Username
                            } else {
                                TextInputKind::Password
                            })
                            .with_secret(self.flow.field() != 0)
                            .on_change(move |value| edit(key, value.to_string()))
                            .on_submit(move |_| submit(AccountRequest::Confirm)),
                    )
                    .with_label_placement(Some(LabelPlacement::Above)),
                ))
                .push(Box::new(
                    action_button(
                        "installer.account.icloud.confirm",
                        if self.flow.field() == 0 {
                            "installer-accounts-continue"
                        } else {
                            "installer-accounts-connect"
                        },
                        AccountRequest::Confirm,
                        dispatch.clone(),
                    )
                    .role(ButtonRole::Primary),
                ));
        }
        Box::new(body.push(Box::new(action_button(
            "installer.account.icloud.cancel",
            "installer-accounts-cancel",
            AccountRequest::Cancel,
            dispatch,
        ))))
    }
}
