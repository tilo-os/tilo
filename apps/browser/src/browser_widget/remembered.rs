//! What the Browser remembers of the forms sites ask, for the person: read
//! as a list, shown beside what else the window shows of the site in front,
//! and forgotten.
//!
//! Memory is the document's ([`crate::site_memory`]). Everything here goes
//! through the document's own actions: `forget-form`, `forget-site-forms`
//! and `forget-all-forms` forget, and the page's and the settings' read
//! actions list. The site panel's "Connection and data" page is where the
//! window shows what a site keeps and what the Browser keeps of it, so the
//! forms remembered of the site stand there, under its stored data, each
//! with the button that forgets it.

use ui::i18n::ArgValue;
use ui::tilo_resource::Effect as _;
use ui::tokens::TextRole;
use ui::webview::SiteStatus;
use ui::widget::{Button, ListRow, Text, Widget};

use super::BrowserWidget;
use crate::effects::BrowserEffect;
use crate::resource::{Browser, CATALOG, browser};
use crate::site_memory::{self, RememberedForm};

fn t(key: &str) -> String {
    CATALOG.t(key)
}

/// One site's forms as a read lists them: each by its shape.
fn listed(site: &str, forms: &[&RememberedForm]) -> serde_json::Value {
    serde_json::json!({ "site": site, "forms": forms })
}

impl BrowserWidget {
    /// The page's action that lists what is remembered of its site.
    pub(super) fn remembered_forms_action(&self) -> ui::tilo_actions::ActionSpec {
        let read = BrowserEffect::ReadRememberedForms.bound();
        if self.browsing_mode.is_private() {
            read.unavailable("private_window", "A private window remembers no forms")
        } else if crate::site_controls::origin(&self.current_uri).is_none() {
            read.unavailable("no_web_page", "Open a web page first")
        } else {
            read
        }
    }

    /// The forms remembered of the site in front, or of every site when
    /// `all`: what the read actions answer.
    pub(super) fn remembered_forms(&self, all: bool) -> Result<ui::framework::Effected, String> {
        if self.browsing_mode.is_private() {
            return Err("A private window remembers no forms".into());
        }
        let memory = self.forms.memory();
        if all {
            let sites: Vec<serde_json::Value> = site_memory::sites(memory)
                .iter()
                .map(|(site, forms)| listed(site, forms))
                .collect();
            return Ok(ui::framework::Effected::value(
                serde_json::json!({ "sites": sites, "forms": memory.len() }),
            ));
        }
        let site = crate::site_controls::origin(&self.current_uri)
            .ok_or("This window shows no web page: open one first")?;
        let forms: Vec<&RememberedForm> = site_memory::of_site(memory, &site)
            .map(|(_, form)| form)
            .collect();
        Ok(ui::framework::Effected::value(listed(&site, &forms)))
    }

    /// Forgetting the forms of the site `origin`, bound to the page's row,
    /// while the site has any.
    pub(super) fn forget_site_declaration(
        &self,
        origin: &str,
    ) -> Option<ui::tilo_resource::ActionDecl<Browser>> {
        let remembered = site_memory::of_site(self.forms.memory(), origin)
            .next()
            .is_some();
        (remembered && !self.browsing_mode.is_private())
            .then(|| Browser::forget_site(origin))
            .flatten()
    }

    /// The person cleared a site's data in the site panel: what is
    /// remembered of its forms goes with it.
    pub(super) fn forget_cleared_sites(&mut self) {
        for origin in self.site.take_cleared() {
            let remembered = site_memory::of_site(self.forms.memory(), &origin)
                .next()
                .is_some();
            if remembered {
                self.submit(Browser::FORGET_SITE_FORMS.with(browser::ForgetSiteForms { origin }));
            }
        }
    }

    /// The person cleared every site's data: every remembered form goes
    /// with it.
    pub(super) fn forget_all_forms(&self) {
        if !self.forms.memory().is_empty() {
            self.submit(Browser::FORGET_ALL_FORMS.with(()));
        }
    }

    /// What the site panel shows of the forms remembered of the site in
    /// front, under the site's stored data: each form with the button that
    /// forgets it, and one that forgets them all. Nothing in a private
    /// window, which remembers none.
    pub(super) fn remembered_section(&self) -> Vec<Box<dyn Widget>> {
        if self.browsing_mode.is_private() {
            return Vec::new();
        }
        let Some(origin) = crate::site_controls::origin(&self.current_uri) else {
            return Vec::new();
        };
        // By name, as a person looks for one.
        let mut forms = site_memory::forms_of(self.forms.memory(), &origin);
        forms.sort_by_key(|form| form.label.to_lowercase());
        let mut section: Vec<Box<dyn Widget>> = vec![Box::new(
            Text::new(&t("browser-site-forms")).with_role(TextRole::Subheading),
        )];
        if forms.is_empty() {
            section.push(Box::new(
                Text::new(&t("browser-site-forms-none")).with_role(TextRole::Body),
            ));
            return section;
        }
        for form in &forms {
            let label = if form.label.trim().is_empty() {
                t("browser-site-form-untitled")
            } else {
                form.label.clone()
            };
            let forget = Button::new(
                &format!("browser.site.forms.forget.{}", form.signature),
                &t("browser-site-form-forget"),
            )
            .with_accessible_name(&CATALOG.t_args(
                "browser-site-form-forget-name",
                &[("form", ArgValue::Text(label.clone()))],
            ));
            let forget = self.document_button(
                forget,
                Some(Browser::FORGET_FORM.with(browser::ForgetForm {
                    origin: origin.clone(),
                    signature: form.signature.clone(),
                })),
            );
            let questions = CATALOG.t_args(
                "browser-site-form-questions",
                &[(
                    "count",
                    ArgValue::Number(ui::num::f64_of_usize(form.questions.len())),
                )],
            );
            section.push(Box::new(
                ListRow::new(format!("browser.site.forms.{}", form.signature), label)
                    .secondary(questions)
                    .trailing(Box::new(forget)),
            ));
        }
        section.push(Box::new(
            Text::new(&t("browser-site-forms-note"))
                .with_role(TextRole::Caption)
                .dimmed(),
        ));
        let forget_all = Button::new(
            "browser.site.forms.forget-all",
            &t("browser-site-forms-forget"),
        );
        section.push(Box::new(self.document_button(
            forget_all,
            Some(Browser::FORGET_SITE_FORMS.with(browser::ForgetSiteForms { origin })),
        )));
        section
    }

    /// Screenshot hook: the site panel on the connection and data of a
    /// site forms are remembered of, as the engine answered its status.
    pub(super) fn show_remembered_for_shot(&mut self) {
        let url = format!("{}{}", site_memory::SHOT_SITE, site_memory::SHOT_PAGE);
        self.current_uri.clone_from(&url);
        self.address.set(url);
        self.title = "Apply for a visitor visa".into();
        self.show_settings = false;
        self.show_library = false;
        self.site.stage_information(SiteStatus {
            request_id: 0,
            origin: site_memory::SHOT_SITE.into(),
            generation: 1,
            blocked: 0,
            global_enabled: true,
            excepted: false,
            effective: true,
            security: "secure".into(),
            certificate_status: 0,
            tls_version: 6,
            content_status: 0,
            subject: "visas.example.org".into(),
            issuer: "Example Trust Services CA 2".into(),
            expires: 13_437_613_041_000_000.0,
            permissions: ["camera", "microphone", "location", "notifications"]
                .into_iter()
                .map(|name| (name.into(), "ask".into()))
                .collect(),
            can_reset: true,
        });
    }
}
