//! The stories the club talks about: Hacker News's front page, read through
//! its public API, with what is plainly politics left out before the model
//! is ever asked.

use std::time::Duration;

use serde::{Deserialize, Serialize};

/// Where the front page is read.
pub const API: &str = "https://hacker-news.firebaseio.com/v0";
/// How many of the top stories are looked at for the next one to open.
const LOOKED_AT: usize = 30;
/// The fewest points a story needs to be worth a thread.
const MIN_SCORE: u64 = 40;
const TIMEOUT: Duration = Duration::from_secs(15);
/// The most of an answer read.
const MAX_ANSWER: usize = 256 * 1024;

/// One story from the front page.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Story {
    /// Hacker News's item id.
    pub id: u64,
    /// The headline.
    pub title: String,
    /// The linked article, if it is not a text post.
    #[serde(default)]
    pub url: Option<String>,
    /// The post's own text, for an Ask or Show HN.
    #[serde(default)]
    pub text: Option<String>,
    /// Its points on the front page.
    #[serde(default)]
    pub score: u64,
}

impl Story {
    /// The story's own page on Hacker News.
    #[must_use]
    pub fn discussion(&self) -> String {
        format!("https://news.ycombinator.com/item?id={}", self.id)
    }

    /// The link the club's opening post carries: the article, or the
    /// discussion for a text post.
    #[must_use]
    pub fn link(&self) -> String {
        self.url.clone().unwrap_or_else(|| self.discussion())
    }
}

/// An item as the API answers it.
#[derive(Deserialize)]
struct Item {
    id: u64,
    #[serde(default, rename = "type")]
    kind: String,
    #[serde(default)]
    title: Option<String>,
    #[serde(default)]
    url: Option<String>,
    #[serde(default)]
    text: Option<String>,
    #[serde(default)]
    score: u64,
    #[serde(default)]
    dead: bool,
    #[serde(default)]
    deleted: bool,
}

/// Words that mark a title as politics. Matched as whole words, case
/// folded; the model is asked again about what passes.
const POLITICAL: &[&str] = &[
    "trump",
    "biden",
    "harris",
    "vance",
    "obama",
    "putin",
    "zelensky",
    "netanyahu",
    "xi",
    "election",
    "elections",
    "electoral",
    "campaign",
    "senate",
    "senator",
    "congress",
    "congressional",
    "parliament",
    "democrat",
    "democrats",
    "republican",
    "republicans",
    "gop",
    "maga",
    "president",
    "presidential",
    "prime",
    "minister",
    "white",
    "impeachment",
    "partisan",
    "immigration",
    "abortion",
    "gaza",
    "israel",
    "ukraine",
    "russia",
    "war",
    "tariff",
    "tariffs",
    "doge",
    "ice",
    "protest",
    "protests",
    "supreme",
];

/// Whether `title` is plainly about politics.
#[must_use]
pub fn plainly_political(title: &str) -> bool {
    let words: Vec<String> = title
        .split(|c: char| !c.is_alphanumeric())
        .filter(|word| !word.is_empty())
        .map(str::to_lowercase)
        .collect();
    let has = |word: &str| words.iter().any(|w| w == word);
    // "white" and "prime" only with what makes them politics.
    POLITICAL.iter().any(|word| match *word {
        "white" => has("white") && has("house"),
        "prime" => has("prime") && has("minister"),
        "supreme" => has("supreme") && has("court"),
        "ice" => title.contains("ICE"),
        other => has(other),
    })
}

/// Whether `title` is a front-page item that is not a story to discuss:
/// a hiring thread or a job post.
fn housekeeping(title: &str) -> bool {
    let lower = title.to_lowercase();
    lower.starts_with("ask hn: who is hiring")
        || lower.starts_with("ask hn: who wants to be hired")
        || lower.starts_with("ask hn: freelancer")
        || lower.contains("is hiring")
}

/// Where the club's stories come from.
pub trait Stories: Send + Sync {
    /// The next story worth a thread that `taken` does not hold, and the
    /// ids passed over on the way, so they are not looked at again.
    fn next(
        &self,
        taken: &(dyn Fn(u64) -> bool + Sync),
    ) -> impl Future<Output = Result<(Option<Story>, Vec<u64>), String>> + Send;
}

/// Reads the front page.
pub struct FrontPage {
    client: tilo_net::Client,
    api: String,
}

impl FrontPage {
    /// The front page at `api` ([`API`] in production).
    #[must_use]
    pub fn new(api: &str) -> Self {
        Self {
            client: tilo_net::Client::new(),
            api: api.trim_end_matches('/').to_owned(),
        }
    }

    async fn get<T: serde::de::DeserializeOwned>(&self, path: &str) -> Result<T, String> {
        let response = self
            .client
            .get(format!("{}/{path}", self.api))
            .timeout(TIMEOUT)
            .send()
            .await
            .map_err(|error| format!("ask Hacker News: {error}"))?;
        if !response.status().is_success() {
            return Err(format!(
                "Hacker News answered {}",
                response.status().as_u16()
            ));
        }
        let bytes = response
            .bytes_limited(MAX_ANSWER)
            .await
            .map_err(|error| format!("read Hacker News: {error}"))?;
        serde_json::from_slice(&bytes).map_err(|error| format!("read Hacker News: {error}"))
    }

    /// The first top story worth a thread that `taken` does not hold:
    /// a live story, scored enough, not housekeeping and not plainly
    /// politics. The ids it passed over for politics are returned too.
    ///
    /// # Errors
    /// The front page could not be read.
    pub async fn top(
        &self,
        taken: &(dyn Fn(u64) -> bool + Sync),
    ) -> Result<(Option<Story>, Vec<u64>), String> {
        let top: Vec<u64> = self.get("topstories.json").await?;
        let mut passed = Vec::new();
        for id in top.into_iter().filter(|id| !taken(*id)).take(LOOKED_AT) {
            let item: Item = match self.get(&format!("item/{id}.json")).await {
                Ok(item) => item,
                Err(error) => {
                    tracing::debug!(id, %error, "news club skips a story it could not read");
                    continue;
                }
            };
            let Some(title) = item.title.filter(|title| !title.trim().is_empty()) else {
                continue;
            };
            if item.kind != "story" || item.dead || item.deleted || item.score < MIN_SCORE {
                continue;
            }
            if housekeeping(&title) || plainly_political(&title) {
                passed.push(item.id);
                continue;
            }
            return Ok((
                Some(Story {
                    id: item.id,
                    title,
                    url: item.url,
                    text: item.text.map(|text| plain(&text)),
                    score: item.score,
                }),
                passed,
            ));
        }
        Ok((None, passed))
    }
}

impl Stories for FrontPage {
    fn next(
        &self,
        taken: &(dyn Fn(u64) -> bool + Sync),
    ) -> impl Future<Output = Result<(Option<Story>, Vec<u64>), String>> + Send {
        self.top(taken)
    }
}

/// An item's HTML text as plain text: paragraphs kept, tags dropped, the
/// few entities Hacker News writes decoded.
fn plain(html: &str) -> String {
    let mut text = String::with_capacity(html.len());
    let mut in_tag = false;
    for c in html.replace("<p>", "\n\n").chars() {
        match c {
            '<' => in_tag = true,
            '>' => in_tag = false,
            c if !in_tag => text.push(c),
            _ => {}
        }
    }
    text.replace("&#x27;", "'")
        .replace("&#x2F;", "/")
        .replace("&quot;", "\"")
        .replace("&gt;", ">")
        .replace("&lt;", "<")
        .replace("&amp;", "&")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn titles_about_politics_are_left_out_and_others_kept() {
        assert!(plainly_political("Trump signs order on chip exports"));
        assert!(plainly_political("The White House's new AI memo"));
        assert!(plainly_political("Senate passes the budget"));
        assert!(!plainly_political("A white paper on CRDTs"));
        assert!(!plainly_political("Prime numbers in the wild"));
        assert!(!plainly_political("Show HN: An ice cream robot"));
        assert!(!plainly_political("Rewriting our database in Rust"));
    }

    #[test]
    fn hiring_threads_are_not_stories() {
        assert!(housekeeping("Ask HN: Who is hiring? (October 2026)"));
        assert!(housekeeping("Acme (YC S24) is hiring a founding engineer"));
        assert!(!housekeeping("Why hiring is broken"));
    }

    #[test]
    fn an_items_html_reads_as_plain_text() {
        assert_eq!(
            plain("I built this.<p>It&#x27;s <i>fast</i> &amp; small."),
            "I built this.\n\nIt's fast & small."
        );
    }
}
