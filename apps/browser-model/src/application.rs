//! The browser's document: the bookmarks, the preferences that follow the person to
//! every window, and the sites tracker protection is off for.

use tilo_resource::{
    self, ActionCx, ActionDecl, Collection, Derived, Id, Model, Refusal, ScopeDecl, Set, resource,
};

use crate::library::{Bookmark, MAX_BOOKMARKS};
use crate::settings::Settings;
use crate::urls::{MAX_SITE_EXCEPTIONS, normalized_bookmark_url};

#[derive(Model, Clone, Debug, Default, PartialEq)]
pub struct Browser {
    /// Bookmarks
    pub bookmarks: Collection<Bookmark>,
    /// Preferences
    pub preferences: Settings,
    /// Sites without tracker protection, by exact origin
    #[model(readonly)]
    pub site_exceptions: Set<String>,
    /// Forms remembered of the sites that asked them, by site and
    /// signature: their shape, never what a field held. Reading or
    /// answering a page's forms remembers them, and forgetting a form, a
    /// site's forms or all of them forgets them; nothing else changes them.
    #[model(readonly)]
    pub site_forms: std::collections::BTreeMap<String, crate::site_memory::RememberedForm>,
}

impl Browser {
    /// What the resource holds: the bookmarks, the preferences that follow the person to every
    /// window, and the sites tracker protection is off for.
    pub fn read_document(
        &self,
        item: Option<&tilo_resource::Id>,
    ) -> Result<serde_json::Value, String> {
        tilo_resource::read::fields(self, item)
    }
}

#[resource(
    kind = "browser.application",
    initialize = Browser::default,
    scopes = Browser::scopes,
    children = Browser::sites,
    facts = Browser::document_facts,
    read_content = Browser::read_document
)]
impl Browser {
    /// Bookmark a page.
    #[action(keywords = "bookmark, favourite, favorite, save page")]
    pub fn add_bookmark(
        &mut self,
        cx: &mut ActionCx,
        #[param("The page's absolute http(s) address")] url: String,
        #[param("Its title; the address when none")] title: Option<String>,
    ) -> Result<(), Refusal> {
        let url = normalized_bookmark_url(&url)
            .ok_or_else(|| Refusal::new("a bookmark is an absolute http(s) URL"))?;
        if self.bookmarks.values().any(|bookmark| bookmark.url == url) {
            return Ok(());
        }
        if self.bookmarks.len() >= MAX_BOOKMARKS {
            return Err(Refusal::new("too many bookmarks; remove some first"));
        }
        let title = title
            .map(|title| title.trim().to_owned())
            .filter(|title| !title.is_empty())
            .unwrap_or_else(|| url.clone());
        self.bookmarks.push(cx.id(), Bookmark { url, title });
        Ok(())
    }

    /// Remove a bookmark.
    #[action(item = bookmarks)]
    pub fn remove_bookmark(&mut self, bookmark: &Id) -> Result<(), Refusal> {
        let url = self
            .bookmarks
            .get(bookmark)
            .ok_or_else(|| Refusal::new("no such bookmark"))?
            .url
            .clone();
        // Two people bookmarking one page at once made two items of it.
        self.bookmarks.retain(|_, bookmark| bookmark.url != url);
        Ok(())
    }

    /// Turn tracker protection on or off for a site.
    #[action(keywords = "ad blocker, tracking protection, block trackers, privacy")]
    pub fn set_site_protection(
        &mut self,
        #[param("The site's address or exact origin, such as https://example.org")] origin: String,
        #[param("Whether tracker protection is on for the site")] enabled: bool,
    ) -> Result<(), Refusal> {
        let origin = crate::urls::origin(origin.trim())
            .ok_or_else(|| Refusal::new("a site is named by one http(s) origin"))?;
        if enabled {
            self.site_exceptions.remove(&origin);
        } else if !self.site_exceptions.contains(&origin) {
            if self.site_exceptions.len() >= MAX_SITE_EXCEPTIONS {
                return Err(Refusal::new(
                    "too many site exceptions; turn protection back on for some",
                ));
            }
            self.site_exceptions.insert(origin);
        }
        Ok(())
    }

    /// Remember the forms a page asks.
    #[action(housekeeping)]
    pub fn remember_forms(
        &mut self,
        #[param("The site's exact origin, such as https://example.org")] origin: String,
        #[param("The page's path on the site")] page: String,
        #[param("The forms read on the page, by their shape")] forms: Vec<
            crate::site_memory::RememberedForm,
        >,
        #[param("Whether they are every form the page asks now")] whole: bool,
        #[param("When they were read, in Unix seconds")] at: u64,
    ) -> Result<(), Refusal> {
        crate::site_memory::remember(&mut self.site_forms, &origin, &page, forms, whole, at)
    }

    /// Forget a remembered form.
    #[action(keywords = "remembered form, site memory")]
    pub fn forget_form(
        &mut self,
        #[param("The site's address or exact origin, such as https://example.org")] origin: String,
        #[param("The form's signature, as read-remembered-forms gives it")] signature: String,
    ) -> Result<(), Refusal> {
        let origin = site_named(&origin)?;
        if crate::site_memory::forget_form(&mut self.site_forms, &origin, &signature) == 0 {
            return Err(Refusal::new("no such form is remembered of this site"));
        }
        Ok(())
    }

    /// Forget a site's remembered forms.
    #[action(keywords = "remembered forms, site memory, forget site")]
    pub fn forget_site_forms(
        &mut self,
        #[param("The site's address or exact origin, such as https://example.org")] origin: String,
    ) -> Result<(), Refusal> {
        let origin = site_named(&origin)?;
        crate::site_memory::forget_site(&mut self.site_forms, &origin);
        Ok(())
    }

    /// Forget all remembered forms.
    #[action(keywords = "remembered forms, site memory, clear forms")]
    pub fn forget_all_forms(&mut self) -> Result<(), Refusal> {
        self.site_forms.clear();
        Ok(())
    }
}

/// The site `named` by its address or its exact origin.
fn site_named(named: &str) -> Result<String, Refusal> {
    crate::urls::origin(named.trim())
        .ok_or_else(|| Refusal::new("a site is named by one http(s) origin"))
}

pub fn bookmark_resource_id(id: &Id) -> String {
    tilo_resource::items::item_resource_id::<Browser>("bookmarks", id)
}

impl Browser {
    fn scopes(_: &Self) -> Vec<ScopeDecl> {
        vec![
            ScopeDecl::new(
                crate::urls::HISTORY_SCOPE,
                tilo_resource::word!("browser-library-section-history").read(),
            ),
            ScopeDecl::new(
                crate::urls::BOOKMARKS_SCOPE,
                tilo_resource::word!("browser-library-section-bookmarks").read(),
            ),
            ScopeDecl::new(
                crate::urls::DOWNLOADS_SCOPE,
                tilo_resource::word!("browser-library-section-downloads").read(),
            ),
        ]
    }

    /// What the document holds, as a person would state it: the bookmarks
    /// in order, the preferences, and the sites tracker protection is off
    /// for. It is the document's read, and so what the action directory
    /// discloses the window through: Show page and the window's other views
    /// change only what their caller sees, and a window disclosed through
    /// one of them is never listed to an agent, nor the page on it.
    #[allow(
        clippy::unnecessary_wraps,
        reason = "the signature is the one `#[resource(facts = path)]` takes"
    )]
    pub fn document_facts(&self) -> Result<serde_json::Value, String> {
        Ok(serde_json::json!({
            "bookmarks": self
                .bookmarks
                .values()
                .map(|bookmark| {
                    serde_json::json!({ "title": bookmark.title, "address": bookmark.url })
                })
                .collect::<Vec<_>>(),
            "homepage": self.preferences.homepage,
            "search_engine": self.preferences.search_engine,
            "blocks_ads_and_trackers": self.preferences.adblock_enabled,
            "clears_data_on_exit": self.preferences.clear_on_exit,
            "sites_without_tracker_protection": self
                .site_exceptions
                .iter()
                .cloned()
                .collect::<Vec<String>>(),
        }))
    }

    fn sites(&self) -> Vec<Derived<Self>> {
        self.site_exceptions
            .iter()
            .map(|origin| {
                let params = browser::SetSiteProtection::paths();
                Derived::new(origin.clone())
                    .of_kind("site")
                    .identified(format!("site:{origin}"))
                    .with(
                        Self::SET_SITE_PROTECTION
                            .bind(params.origin, origin.clone())
                            .and(params.enabled, true),
                    )
            })
            .collect()
    }

    /// Turning tracker protection for `origin` to the other state from `protected`, as
    /// the row of a page on that site offers it.
    pub fn site_switch(origin: &str, protected: bool) -> Option<ActionDecl<Self>> {
        let params = browser::SetSiteProtection::paths();
        Self::SET_SITE_PROTECTION
            .bind(params.origin, origin.to_owned())
            .and(params.enabled, !protected)
            .declaration(
                "set-site-protection",
                if protected {
                    tilo_resource::word!("browser-application-protection-off")
                } else {
                    tilo_resource::word!("browser-application-protection-on")
                },
            )
    }

    /// Forgetting the forms remembered of the site `origin`, as the row of a
    /// page on that site offers it.
    pub fn forget_site(origin: &str) -> Option<ActionDecl<Self>> {
        let params = browser::ForgetSiteForms::paths();
        Self::FORGET_SITE_FORMS
            .bind(params.origin, origin.to_owned())
            .declaration(
                "forget-this-site-forms",
                tilo_resource::word!("browser-application-forget-this-site-forms"),
            )
    }

    pub fn exceptions(&self) -> std::collections::BTreeSet<String> {
        self.site_exceptions.iter().cloned().collect()
    }
}
