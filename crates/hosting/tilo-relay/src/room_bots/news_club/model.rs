//! What writes the club's posts: one Messages call per post, asked as one
//! persona with the story and the thread so far, answered as structured
//! output the club can trust the shape of.

use std::time::Duration;

use serde::Deserialize;
use zeroize::Zeroizing;

use super::hn::Story;
use super::personas::Persona;

/// The purpose the host's secret store keeps the club's model key under.
pub const KEY_PURPOSE: &str = "model-api-key";
/// The item that key is kept as.
pub const KEY_ITEM: &str = "news-club";
const API_VERSION: &str = "2023-06-01";
const ENDPOINT: &str = "https://api.anthropic.com";
const MAX_OUTPUT_TOKENS: u32 = 600;
const TIMEOUT: Duration = Duration::from_secs(60);
/// The most of an answer read.
const MAX_ANSWER: usize = 64 * 1024;
/// The most of an article the model is shown.
pub const ARTICLE_CHARS: usize = 6000;
/// The most of one chat line the model is shown.
const LINE_CHARS: usize = 1200;

/// One line of a thread, as the model is shown it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Line {
    /// The name it shows.
    pub name: String,
    /// Whether the club wrote it.
    pub club: bool,
    /// What it says.
    pub text: String,
}

/// What the model is asked to write.
#[derive(Clone, Debug)]
pub struct Ask {
    /// Who writes.
    pub persona: Persona,
    /// What the thread is about.
    pub story: Story,
    /// The article's readable text, when it could be fetched.
    pub article: Option<String>,
    /// The thread so far, oldest first; empty for the opening post.
    pub thread: Vec<Line>,
    /// The line this post answers, if it answers one.
    pub answering: Option<Line>,
}

/// What the model wrote.
#[derive(Clone, Debug, PartialEq, Eq, Deserialize)]
pub struct Written {
    /// The story is about politics: the club leaves it.
    pub political: bool,
    /// The post, or `None` when the persona has nothing worth adding.
    pub text: Option<String>,
}

/// Writes a persona's post.
pub trait Writer: Send + Sync {
    /// The post `ask` asks for.
    fn write(&self, ask: &Ask) -> impl Future<Output = Result<Written, String>> + Send;
}

/// The instructions every post is written under.
pub const INSTRUCTIONS: &str = "You write one chat message for one persona of the News Club, a bot in the \
public chat of the Tilo community. The club's personas discuss the ideas in stories from the Hacker News \
front page, with each other and with the people in the room. Every message the club posts is shown with the \
persona's name and a BOT label, so readers know it was written by AI.\n\
\n\
Write as the persona described in the input:\n\
- Discuss the idea: what is interesting or new, what could go wrong, what it connects to, what you would \
want to know. Be concrete and add something the thread has not said yet.\n\
- Write like a person typing in a chat: usually one to three sentences and under 70 words. No greetings, \
sign-offs, hashtags, emoji, bullet lists or headings.\n\
- Do not always agree. Push back politely where the persona would.\n\
- When answering someone, answer what they said; you may use their name.\n\
- Only state what the article or thread says, or what is common knowledge. Say when you are guessing.\n\
- If anyone asks whether you are a bot, an AI or a person, say plainly that you are one of the News Club's \
AI personas. Never claim to be human or to have a body, a job or a life.\n\
- Stay away from party politics, elections, politicians, wars and culture-war topics. If the story itself \
is about politics, set `political` to true and leave `text` empty.\n\
- If the persona has nothing worth adding, or the thread has run its course, leave `text` empty.\n\
\n\
Answer with `political` and `text` only.";

/// The JSON schema of [`Written`].
fn schema() -> serde_json::Value {
    serde_json::json!({
        "type": "object",
        "properties": {
            "political": {"type": "boolean"},
            "text": {"type": "string"}
        },
        "required": ["political", "text"],
        "additionalProperties": false
    })
}

fn clipped(text: &str, chars: usize) -> String {
    match text.char_indices().nth(chars) {
        Some((end, _)) => format!("{}…", &text[..end]),
        None => text.to_owned(),
    }
}

fn shown(line: &Line) -> String {
    let label = if line.club { " (club bot)" } else { "" };
    format!("{}{label}: {}", line.name, clipped(&line.text, LINE_CHARS))
}

impl Ask {
    /// The user turn the model is given.
    #[must_use]
    pub fn input(&self) -> String {
        let mut input = format!(
            "Persona: {}\nVoice: {}\n\nStory: {}\nLink: {}\nPoints on Hacker News: {}\n",
            self.persona.name,
            self.persona.voice,
            self.story.title,
            self.story.link(),
            self.story.score
        );
        if let Some(text) = self
            .story
            .text
            .as_deref()
            .filter(|text| !text.trim().is_empty())
        {
            input.push_str(&format!("\nPost text:\n{}\n", clipped(text, ARTICLE_CHARS)));
        }
        if let Some(article) = self.article.as_deref() {
            input.push_str(&format!(
                "\nArticle:\n{}\n",
                clipped(article, ARTICLE_CHARS)
            ));
        }
        if self.thread.is_empty() {
            input.push_str(
                "\nThe thread is new. Write the opening message: the link is posted with it, so do \
                 not repeat the title; say what caught the persona's interest and ask the room \
                 something.\n",
            );
        } else {
            input.push_str("\nThe thread so far, oldest first:\n");
            for line in &self.thread {
                input.push_str(&shown(line));
                input.push('\n');
            }
        }
        if let Some(answering) = &self.answering {
            input.push_str(&format!("\nAnswer this message:\n{}\n", shown(answering)));
        }
        input
    }
}

/// Anthropic's Messages API, its key from the host's secret store.
pub struct Anthropic {
    endpoint: String,
    model: String,
    key: Zeroizing<String>,
    client: tilo_net::Client,
}

impl std::fmt::Debug for Anthropic {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Anthropic")
            .field("endpoint", &self.endpoint)
            .field("model", &self.model)
            .finish_non_exhaustive()
    }
}

impl Anthropic {
    /// The writer asking `model`, its key kept from `key_file` in `store`
    /// first, as every provider key on a host is.
    ///
    /// # Errors
    /// The file is unreadable or empty, or the store refused or lost the
    /// key. The key itself never appears in an error.
    pub fn import(
        store: &tilo_secrets::SecretStore,
        key_file: &std::path::Path,
        model: Option<&str>,
    ) -> Result<Self, String> {
        let key = Zeroizing::new(
            std::fs::read(key_file)
                .map_err(|error| format!("read {}: {}", key_file.display(), error.kind()))?,
        );
        let trimmed = key.trim_ascii();
        if trimmed.is_empty() {
            return Err(format!("{} is empty", key_file.display()));
        }
        let item = store.item(KEY_PURPOSE, KEY_ITEM);
        item.put_blocking(trimmed)
            .map_err(|error| format!("keep the news club's model key: {error}"))?;
        let kept = item
            .get_blocking()
            .map_err(|error| format!("read the news club's model key: {error}"))?
            .ok_or("the news club's model key was not kept")?;
        let key = Zeroizing::new(
            String::from_utf8(kept.expose().to_vec())
                .map_err(|_| "the news club's model key is not text".to_owned())?,
        );
        Ok(Self {
            endpoint: ENDPOINT.to_owned(),
            model: model.unwrap_or(tilo_moderation::DEFAULT_MODEL).to_owned(),
            key,
            client: tilo_net::Client::new(),
        })
    }

    /// The model it asks.
    #[must_use]
    pub fn model(&self) -> &str {
        &self.model
    }

    /// The request body for `ask`.
    #[must_use]
    pub fn body(&self, ask: &Ask) -> serde_json::Value {
        serde_json::json!({
            "model": self.model,
            "max_tokens": MAX_OUTPUT_TOKENS,
            "system": INSTRUCTIONS,
            "output_config": {
                "effort": "low",
                "format": {"type": "json_schema", "schema": schema()}
            },
            "messages": [{"role": "user", "content": ask.input()}]
        })
    }

    async fn ask(&self, ask: &Ask) -> Result<Written, String> {
        let response = self
            .client
            .post(format!("{}/v1/messages", self.endpoint))
            .header("x-api-key", self.key.as_str())
            .header("anthropic-version", API_VERSION)
            .json(&self.body(ask))
            .timeout(TIMEOUT)
            .send()
            .await
            .map_err(|error| format!("ask the model: {error}"))?;
        let status = response.status().as_u16();
        let bytes = response
            .bytes_limited(MAX_ANSWER)
            .await
            .map_err(|error| format!("read the model's answer: {error}"))?;
        let body: serde_json::Value = serde_json::from_slice(&bytes)
            .map_err(|_| format!("an unreadable answer ({status})"))?;
        if !(200..300).contains(&status) {
            let message = body
                .pointer("/error/message")
                .and_then(serde_json::Value::as_str)
                .unwrap_or("no message");
            return Err(format!("{status}: {message}"));
        }
        let text =
            tilo_moderation::anthropic::answer_text(&body).map_err(|error| error.to_string())?;
        parse(&text)
    }
}

impl Writer for Anthropic {
    fn write(&self, ask: &Ask) -> impl Future<Output = Result<Written, String>> + Send {
        self.ask(ask)
    }
}

/// The model's answer, a post with nothing in it read as no post.
///
/// # Errors
/// The answer is not a [`Written`].
pub fn parse(text: &str) -> Result<Written, String> {
    let mut written: Written = serde_json::from_str(text.trim())
        .map_err(|error| format!("the model's answer: {error}"))?;
    written.text = written
        .text
        .map(|text| text.trim().to_owned())
        .filter(|text| !text.is_empty() && !written.political);
    Ok(written)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ask() -> Ask {
        Ask {
            persona: super::super::personas::PERSONAS[0],
            story: Story {
                id: 1,
                title: "A tiny CRDT".into(),
                url: Some("https://example.com/crdt".into()),
                text: None,
                score: 120,
            },
            article: Some("It merges.".into()),
            thread: vec![Line {
                name: "Sam".into(),
                club: false,
                text: "Does it scale?".into(),
            }],
            answering: Some(Line {
                name: "Sam".into(),
                club: false,
                text: "Does it scale?".into(),
            }),
        }
    }

    #[test]
    fn the_model_sees_the_persona_story_thread_and_line_to_answer() {
        let input = ask().input();
        assert!(input.contains("Persona: Ada"));
        assert!(input.contains("Story: A tiny CRDT"));
        assert!(input.contains("Article:\nIt merges."));
        assert!(input.contains("Answer this message:\nSam: Does it scale?"));
    }

    #[test]
    fn an_empty_or_political_answer_is_no_post() {
        assert_eq!(
            parse(r#"{"political": false, "text": "  "}"#).unwrap().text,
            None
        );
        assert_eq!(
            parse(r#"{"political": true, "text": "about the vote"}"#)
                .unwrap()
                .text,
            None
        );
        assert_eq!(
            parse(r#"{"political": false, "text": " Neat. "}"#)
                .unwrap()
                .text
                .as_deref(),
            Some("Neat.")
        );
    }
}
