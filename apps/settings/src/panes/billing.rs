//! Billing: the person's account with the host that runs their workspaces.

use std::cell::{OnceCell, RefCell};
use std::rc::Rc;
use std::time::Duration;

use ui::LocalState;
use ui::framework::{EffectAnswer, EffectCx, Effected, ViewCx};
use ui::tilo_actions::remote::InvokeArgument;
use ui::tilo_actions::remote::host_account::{self, HostAccount, InvoiceStatus, PaymentMethod};
use ui::tilo_resource;
use ui::widget::Element;

use crate::area::AreaContext;
use crate::effects::SettingsEffect;
use crate::i18n::{t, t_args};
use crate::resource::Settings;
use crate::settings_app::SettingsApp;
use provider_settings_ui::widgets::{
    SettingsSection, note, page, row, rows, section, section_action, thing_row, value_row,
};

crate::effects::effect_family!(BillingEffect => answer);

#[derive(Clone, Debug, PartialEq, Eq, tilo_resource::Effect)]
#[effect(operation = "settings.effect")]
pub enum BillingEffect {
    /// Open the provider's page for the account: its card, plan and invoices.
    #[effect(view, label = "settings-billing-manage")]
    ManageBilling,
    /// Open one invoice on the provider's page.
    #[effect(view)]
    OpenInvoice {
        #[param("The invoice's id, as the host's account answer lists it")]
        invoice: String,
    },
}

pub(crate) fn answer(
    app: &SettingsApp,
    effect: BillingEffect,
    _cx: &mut EffectCx<Settings>,
) -> EffectAnswer {
    let area = area(app).ok_or("this window has no billing")?;
    let model = area.model().borrow();
    let account = model
        .account
        .as_ref()
        .ok_or_else(|| t("settings-billing-reading"))?;
    ui::widget::open_external_url(&page_of(account, effect)?);
    Ok(Effected::done())
}

/// The provider's page `effect` opens, as the host answered it for `account`: only a web page
/// the provider serves over https, never anything else an answer might name.
fn page_of(account: &HostAccount, effect: BillingEffect) -> Result<String, String> {
    let url = match effect {
        BillingEffect::ManageBilling => account.manage_billing.clone(),
        BillingEffect::OpenInvoice { invoice } => account
            .standing
            .as_ref()
            .and_then(|standing| standing.invoices.iter().find(|i| i.id == invoice))
            .map(|invoice| invoice.url.clone()),
    };
    url.filter(|url| url.starts_with("https://"))
        .ok_or_else(|| t("settings-billing-no-page"))
}

crate::area::pane_area! {
    BillingArea;
    fn poll(&self) {
        BillingArea::poll(self);
    }
}

pub fn area(app: &SettingsApp) -> Option<&BillingArea> {
    app.areas().find()
}

pub struct BillingArea {
    remote: ui::RemoteActions,
    scene: Option<HostAccount>,
    model: OnceCell<Rc<RefCell<BillingModel>>>,
    shown: LocalState<u64>,
}

impl BillingArea {
    pub fn new(cx: &AreaContext, remote: ui::RemoteActions) -> Self {
        Self {
            remote,
            scene: cx.scene.as_ref().and_then(|scene| scene.billing.clone()),
            model: OnceCell::new(),
            shown: LocalState::new(0),
        }
    }

    pub fn model(&self) -> &Rc<RefCell<BillingModel>> {
        self.model.get_or_init(|| {
            Rc::new(RefCell::new(match &self.scene {
                Some(account) => BillingModel::shown(account.clone()),
                None => BillingModel::following(&self.remote),
            }))
        })
    }

    pub(crate) fn poll(&self) {
        if let Some(model) = self.model.get()
            && let Ok(mut model) = model.try_borrow_mut()
            && model.poll()
        {
            self.shown.set(self.shown.get().wrapping_add(1));
        }
    }
}

pub struct BillingModel {
    following: Option<ui::Followed>,
    seen: u64,
    account: Option<HostAccount>,
    error: Option<String>,
}

impl BillingModel {
    fn following(remote: &ui::RemoteActions) -> Self {
        Self {
            following: Some(remote.follow(
                host_account::target(host_account::ACCOUNT),
                InvokeArgument::None,
            )),
            seen: 0,
            account: None,
            error: None,
        }
    }

    fn shown(account: HostAccount) -> Self {
        Self {
            following: None,
            seen: 0,
            account: Some(account),
            error: None,
        }
    }

    fn poll(&mut self) -> bool {
        use ui::tilo_actions::InvocationUpdate;
        let Some(following) = &self.following else {
            return false;
        };
        let call = following.call();
        let revision = call.revision();
        if revision == self.seen {
            return false;
        }
        self.seen = revision;
        let value = match call.get() {
            ui::Arrival::Ready(InvocationUpdate::Item { value, .. }) => value,
            ui::Arrival::Ready(InvocationUpdate::Succeeded { output, .. }) => {
                output.value.unwrap_or(serde_json::Value::Null)
            }
            ui::Arrival::Ready(InvocationUpdate::Failed { error, .. }) => {
                self.error = Some(error.message);
                return true;
            }
            ui::Arrival::Failed(error) => {
                self.error = Some(error.to_string());
                return true;
            }
            ui::Arrival::Ready(_) | ui::Arrival::Pending { .. } => return false,
        };
        match serde_json::from_value::<HostAccount>(value) {
            Ok(account) => {
                self.account = Some(account);
                self.error = None;
            }
            Err(_) => self.error = Some(t("settings-billing-unreadable")),
        }
        true
    }

    fn page(&self, cx: &ViewCx<Settings>) -> Element {
        let Some(account) = &self.account else {
            let line = self
                .error
                .clone()
                .unwrap_or_else(|| t("settings-billing-reading"));
            return page(vec![section(
                &t("settings-billing-title"),
                rows(vec![note(&line)]),
            )]);
        };
        if !account.sells() {
            return page(vec![section(
                &account.host,
                rows(vec![note(&t_args(
                    "settings-billing-sells-nothing",
                    &[("host", account.host.clone().into())],
                ))]),
            )]);
        }
        page(vec![
            account_section(cx, account),
            invoices_section(cx, account),
        ])
    }
}

fn vm_time(seconds: u64) -> String {
    const HOUR: u64 = 3600;
    if seconds < HOUR {
        return ui::format::duration(Duration::from_secs(seconds));
    }
    t_args(
        "settings-billing-hours",
        &[("hours", ui::format::count(seconds / HOUR).into())],
    )
}

fn facts(account: &HostAccount) -> Vec<(String, String)> {
    let standing = account.standing.as_ref();
    let plan = match account.plan() {
        Some(plan) => match &plan.price {
            Some(price) => t_args(
                "settings-billing-plan-priced",
                &[
                    ("plan", plan.name.clone().into()),
                    ("price", price.clone().into()),
                ],
            ),
            None => plan.name.clone(),
        },
        None => t("settings-billing-no-plan"),
    };
    let mut facts = vec![
        (t("settings-billing-plan"), plan),
        (t("settings-billing-card"), card_line(account)),
    ];
    if let Some(standing) = standing {
        let seconds = standing.usage.vm_seconds();
        let included = standing
            .limits
            .as_ref()
            .and_then(|limits| limits.included_vm_seconds);
        let usage = match included {
            Some(included) => t_args(
                "settings-billing-used-of",
                &[
                    ("used", vm_time(seconds).into()),
                    ("included", vm_time(included).into()),
                ],
            ),
            None if seconds == 0 => t("settings-billing-none-used"),
            None => vm_time(seconds),
        };
        facts.push((t("settings-billing-usage"), usage));
        if let Some(credit) = &standing.credit {
            facts.push((t("settings-billing-credit"), credit.clone()));
        }
    }
    facts
}

fn card(account: &HostAccount) -> Option<&PaymentMethod> {
    account.standing.as_ref()?.payment_method.as_ref()
}

fn has_card(account: &HostAccount) -> bool {
    card(account).is_some()
}

fn card_line(account: &HostAccount) -> String {
    let Some(card) = card(account) else {
        return t("settings-billing-card-none");
    };
    match (&card.brand, &card.last4) {
        (Some(brand), Some(last4)) => t_args(
            "settings-billing-card-brand-last4",
            &[
                ("brand", brand.clone().into()),
                ("last4", last4.clone().into()),
            ],
        ),
        (None, Some(last4)) => t_args(
            "settings-billing-card-last4",
            &[("last4", last4.clone().into())],
        ),
        (Some(brand), None) => brand.clone(),
        (None, None) => t("settings-billing-card-on-file"),
    }
}

fn account_section(cx: &ViewCx<Settings>, account: &HostAccount) -> SettingsSection {
    let card = has_card(account);
    let mut items: Vec<_> = facts(account)
        .iter()
        .map(|(label, value)| value_row(label, value))
        .collect();
    if !card {
        items.push(note(&t("settings-billing-card-when")));
    }
    if account.manage_billing.is_none() {
        return section(&account.host, rows(items));
    }
    let label = t(if card {
        "settings-billing-manage"
    } else {
        "settings-billing-add-card"
    });
    section_action(
        &account.host,
        cx.actions().bind_effect_invocation(
            ui::widget::Button::new("settings.billing.manage", &label),
            &SettingsEffect::from(BillingEffect::ManageBilling),
        ),
        rows(items),
    )
}

fn invoices_section(cx: &ViewCx<Settings>, account: &HostAccount) -> SettingsSection {
    let invoices = account
        .standing
        .as_ref()
        .map(|standing| standing.invoices.as_slice())
        .unwrap_or_default();
    let items = if invoices.is_empty() {
        vec![note(&t("settings-billing-no-invoices"))]
    } else {
        invoices
            .iter()
            .map(|invoice| {
                let status = t(match invoice.status {
                    InvoiceStatus::Paid => "settings-billing-invoice-paid",
                    InvoiceStatus::Open => "settings-billing-invoice-open",
                    InvoiceStatus::PastDue => "settings-billing-invoice-past-due",
                    InvoiceStatus::Void => "settings-billing-invoice-void",
                });
                let mut details = vec![status];
                if invoice.issued_ms > 0 {
                    details.push(ui::format::date_of(&ui::format::UnixSecs(
                        i64::try_from(invoice.issued_ms / 1000).unwrap_or_default(),
                    )));
                }
                let line = thing_row(
                    &format!("settings.billing.invoice.{}", invoice.id),
                    &invoice.amount,
                    &details,
                    None,
                );
                row(Box::new(cx.actions().bind_effect_invocation(
                    line,
                    &SettingsEffect::from(BillingEffect::OpenInvoice {
                        invoice: invoice.id.clone(),
                    }),
                )))
            })
            .collect()
    };
    section(&t("settings-billing-invoices"), rows(items))
}

pub(crate) fn view(cx: &ViewCx<Settings>, app: &Rc<SettingsApp>) -> Option<Element> {
    let held = area(app)?;
    let model = Rc::clone(held.model());
    model.borrow_mut().poll();
    Some(cx.switch(&held.shown, move |cx, _| model.borrow().page(cx)))
}

#[cfg(test)]
mod tests;
