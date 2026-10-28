//! Answers to queries that are not names: arithmetic, unit conversions and
//! an address.
//!
//! "2+2" and `https://example.com` are among the commonest things typed into
//! a launcher, and each used to end in "No results". The sum is worked out as
//! it is typed and Enter copies it; an address opens through
//! [`crate::shell::uri_open`], placed like any launch. Searching the web for
//! what was typed is the web provider's ([`super::web`]). What answers a
//! query is worked out in [`tilo_desktop_search::answers`], which the web
//! desktop's search offers too; this provider words and runs the rows.

use tilo_desktop_search::answers::{address, evaluate, format_number, units};

use crate::shell::chrome_state::Chrome;
use crate::shell::i18n::{t, t_args};
use crate::shell::search::action_api::ActionError;
use crate::shell::search::{ResourceKind, SearchContext, SearchProvider, SearchResult};

pub const PROVIDER_ID: &str = "quick-answers";

mod currency;

const SUM_ID: &str = "sum";
const UNIT_ID: &str = "unit";
const MONEY_ID: &str = "money";
/// Rates older than this say how old they are on the answer.
const RATES_STALE_SECS: u64 = 24 * 60 * 60;
const ADDRESS_ID: &str = "address";

pub struct QuickAnswersProvider {
    kinds: Vec<ResourceKind>,
}

impl Default for QuickAnswersProvider {
    fn default() -> Self {
        Self {
            kinds: vec![ResourceKind::Custom(t("search-kind-answers"))],
        }
    }
}

impl QuickAnswersProvider {
    pub fn new() -> Self {
        Self::default()
    }

    fn kind(&self) -> ResourceKind {
        self.kinds
            .first()
            .cloned()
            .unwrap_or_else(|| ResourceKind::Custom(t("search-kind-answers")))
    }
}

fn row(
    kind: ResourceKind,
    id: String,
    title: String,
    subtitle: Option<String>,
    score: i32,
    open: impl Fn(&Chrome) -> Result<(), ActionError> + 'static,
) -> SearchResult {
    let mut row = SearchResult::nullary_fallible(
        kind,
        PROVIDER_ID,
        id,
        title,
        subtitle,
        score,
        move |_doc, _root, chrome| open(chrome),
    );
    row.contract = Some(tilo_actions::ActionContract::view());
    row
}

/// Open `uri` in the workspace, its refusal said as a person reads it.
pub(crate) fn open_uri(uri: &str, chrome: &Chrome) -> Result<(), ActionError> {
    crate::shell::uri_open::open_in_workspace(uri, chrome)
        .map(drop)
        .map_err(uri_refusal)
}

/// Why an address did not open, as a person reads it.
pub(crate) fn uri_refusal(error: crate::shell::uri_open::UriOpenError) -> ActionError {
    ActionError::Explained(match error {
        crate::shell::uri_open::UriOpenError::NoHandler { .. } => t("search-answer-no-browser"),
        crate::shell::uri_open::UriOpenError::NoWorkspace => t("doc-open-no-workspace"),
        crate::shell::uri_open::UriOpenError::InvalidUri => t("search-answer-bad-address"),
        crate::shell::uri_open::UriOpenError::Launch(error) => error.user_message(),
    })
}

impl SearchProvider for QuickAnswersProvider {
    fn id(&self) -> &'static str {
        PROVIDER_ID
    }

    fn name(&self) -> &'static str {
        "Quick answers"
    }

    fn display_name(&self) -> String {
        t("search-provider-quick-answers")
    }

    fn resource_kinds(&self) -> &[ResourceKind] {
        &self.kinds
    }

    /// New rates change what money answers.
    fn search_revision(&self, _ctx: &SearchContext<'_>) -> String {
        format!("answers:{}", crate::shell::net_answers::revision())
    }

    /// An answer's address carries what it answers (`sum:2+2`,
    /// `address:example.com`), so it resolves without the query that found
    /// it.
    fn resolve(&self, id: &str, _ctx: SearchContext<'_>) -> Option<SearchResult> {
        let (_, typed) = id.split_once(':')?;
        self.answers(typed).into_iter().find(|row| row.id == id)
    }

    fn query(&self, ctx: SearchContext<'_>) -> Vec<SearchResult> {
        if !ctx.wanted.contains(&self.kind()) {
            return Vec::new();
        }
        self.answers(ctx.needle)
    }
}

impl QuickAnswersProvider {
    /// Money converted at the last rates the network-answers helper fetched.
    /// Rates are asked for only when money is typed, and never waited for:
    /// until the first ones arrive there is no row.
    fn money(&self, typed: &str) -> Option<SearchResult> {
        currency::request(typed)?;
        let kept = crate::shell::net_answers::rates()?;
        let (from, to) = currency::convert(typed, &kept.value)?;
        let age = tilo_clock::unix_now().saturating_sub(kept.fetched_at);
        let subtitle = if age > RATES_STALE_SECS {
            t_args(
                "search-answer-money-stale",
                &[
                    ("amount", from.into()),
                    (
                        "age",
                        ui::format::relative(i64::try_from(age).unwrap_or(i64::MAX)).into(),
                    ),
                ],
            )
        } else {
            t_args("search-answer-sum", &[("expression", from.into())])
        };
        let copied = to.clone();
        Some(row(
            self.kind(),
            format!("{MONEY_ID}:{typed}"),
            to,
            Some(subtitle),
            self.max_score(),
            move |chrome| {
                ui::text_selection::clipboard_copy(&copied);
                chrome.borrow_mut().acknowledge(t("search-answer-copied"));
                Ok(())
            },
        ))
    }

    /// What answers `typed`: its sum, a conversion, the address it is.
    fn answers(&self, typed: &str) -> Vec<SearchResult> {
        let typed = typed.trim();
        if typed.is_empty() {
            return Vec::new();
        }
        let mut out = Vec::new();
        if let Some(value) = evaluate(typed) {
            let answer = format_number(value);
            let copied = answer.clone();
            out.push(row(
                self.kind(),
                format!("{SUM_ID}:{typed}"),
                answer,
                Some(t_args("search-answer-sum", &[("expression", typed.into())])),
                self.max_score(),
                move |chrome| {
                    ui::text_selection::clipboard_copy(&copied);
                    chrome.borrow_mut().acknowledge(t("search-answer-copied"));
                    Ok(())
                },
            ));
        }
        if let Some(conversion) = units::convert(typed) {
            let copied = conversion.to.clone();
            out.push(row(
                self.kind(),
                format!("{UNIT_ID}:{typed}"),
                conversion.to,
                Some(t_args(
                    "search-answer-sum",
                    &[("expression", conversion.from.into())],
                )),
                self.max_score(),
                move |chrome| {
                    ui::text_selection::clipboard_copy(&copied);
                    chrome.borrow_mut().acknowledge(t("search-answer-copied"));
                    Ok(())
                },
            ));
        }
        if let Some(row) = self.money(typed) {
            out.push(row);
        }
        if let Some((uri, shown)) = address(typed) {
            out.push(row(
                self.kind(),
                format!("{ADDRESS_ID}:{typed}"),
                t_args("search-answer-open-address", &[("address", shown.into())]),
                None,
                self.max_score(),
                move |chrome| open_uri(&uri, chrome),
            ));
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn money_answers_from_the_kept_rates_and_says_when_they_are_old() {
        let rates = tilo_net_answers::Rates {
            base: "EUR".into(),
            date: "2026-10-16".into(),
            rates: [("USD".to_owned(), 1.25)].into_iter().collect(),
        };
        let provider = QuickAnswersProvider::new();
        crate::shell::net_answers::keep_rates_for_test(rates.clone(), tilo_clock::unix_now());
        let fresh = provider.money("10 usd in eur").expect("an answer");
        assert_eq!(fresh.title, "8.00 EUR");
        assert_eq!(
            fresh.subtitle.as_deref(),
            Some("10.00 USD ="),
            "fresh rates say nothing"
        );
        crate::shell::net_answers::keep_rates_for_test(
            rates,
            tilo_clock::unix_now().saturating_sub(3 * 24 * 60 * 60),
        );
        let stale = provider.money("10 usd in eur").expect("an answer offline");
        assert!(
            stale
                .subtitle
                .as_deref()
                .is_some_and(|line| line.contains("3 days ago")),
            "{:?}",
            stale.subtitle
        );
        assert!(provider.money("10 km in mi").is_none());
    }
}
