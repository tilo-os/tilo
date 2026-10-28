//! The contract a review model answers: numbered texts in, the quotes to
//! hide out.
//!
//! The reporter's device writes the prompt from the model view
//! ([`crate::report::model_prompt`]), so a model that is not local sees
//! declared personal values only as placeholders. A host that lends its own
//! model to reporters (a room's host, whose key the reporter's page never
//! holds) asks only a prompt that is exactly this contract
//! ([`texts_of`]) and answers only quotes from the texts it was asked about
//! ([`confine`]): it reviews reports, and is no general model for its
//! visitors.

use std::fmt::Write as _;

use serde::{Deserialize, Serialize};
use tilo_feedback_review::RedactionCategory;

use crate::Error;

const INSTRUCTIONS: &str = "You check a bug report before it is posted where many people read it. \
     Find every personal detail in the numbered texts below: names of people, \
     email addresses, phone numbers, postal addresses, account numbers, \
     passwords or keys, and anything else that identifies or concerns a person. \
     Interface words such as button labels are not personal. Text in \u{2039}angle quotes\u{203A} \
     is already hidden. Answer with JSON only, in this shape: \
     {\"hide\":[{\"text\":0,\"quote\":\"exact words from that text\",\"category\":\"name\"}]} \
     where category is one of name, email, phone, address, account_number, \
     credential_like, free_form_personal. Answer {\"hide\":[]} when nothing is personal.\n\n";

/// Largest prompt a report's review asks: every text a report may carry,
/// with room to spare.
pub const MAX_PROMPT_BYTES: usize = 48 * 1024;
/// Most quotes one answer hides.
const MAX_HIDES: usize = tilo_feedback_review::MAX_REDACTIONS;

/// The prompt asking a model about `texts`, each with its number.
#[must_use]
pub fn prompt<'a>(texts: impl IntoIterator<Item = (usize, &'a str)>) -> String {
    let mut prompt = String::from(INSTRUCTIONS);
    for (index, text) in texts {
        let _ = writeln!(prompt, "[{index}] {text}");
    }
    prompt
}

/// The numbered texts `prompt` asks about, when it is exactly a review
/// prompt ([`prompt`]) within [`MAX_PROMPT_BYTES`]; `None` for anything
/// else.
#[must_use]
pub fn texts_of(prompt: &str) -> Option<Vec<(usize, String)>> {
    if prompt.len() > MAX_PROMPT_BYTES {
        return None;
    }
    let body = prompt.strip_prefix(INSTRUCTIONS)?;
    if body.is_empty() {
        return Some(Vec::new());
    }
    let mut texts: Vec<(usize, String)> = Vec::new();
    for line in body.strip_suffix('\n').unwrap_or(body).split('\n') {
        let numbered = line
            .strip_prefix('[')
            .and_then(|rest| rest.split_once("] "))
            .and_then(|(index, text)| Some((index.parse::<usize>().ok()?, text)));
        match (numbered, texts.last_mut()) {
            (Some((index, text)), _) => texts.push((index, text.to_owned())),
            // A text with a line break goes on over several lines.
            (None, Some((_, text))) => {
                text.push('\n');
                text.push_str(line);
            }
            (None, None) => return None,
        }
    }
    let again = self::prompt(texts.iter().map(|(index, text)| (*index, text.as_str())));
    (again == prompt).then_some(texts)
}

/// The JSON schema of an answer, for a provider that holds its model to one.
#[must_use]
pub fn answer_schema() -> serde_json::Value {
    serde_json::json!({
        "type": "object",
        "properties": {
            "hide": {
                "type": "array",
                "items": {
                    "type": "object",
                    "properties": {
                        "text": {"type": "integer"},
                        "quote": {"type": "string"},
                        "category": {"type": "string", "enum": [
                            "name", "email", "phone", "address", "account_number",
                            "credential_like", "free_form_personal"
                        ]}
                    },
                    "required": ["text", "quote", "category"],
                    "additionalProperties": false
                }
            }
        },
        "required": ["hide"],
        "additionalProperties": false
    })
}

#[derive(Default, Deserialize, Serialize)]
pub(crate) struct Answer {
    #[serde(default)]
    pub(crate) hide: Vec<Hide>,
}

#[derive(Deserialize, Serialize)]
pub(crate) struct Hide {
    pub(crate) text: usize,
    pub(crate) quote: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) category: Option<RedactionCategory>,
}

/// The answer in a model's reply, which may wrap it in words.
pub(crate) fn parse(answer: &str) -> Result<Answer, Error> {
    let start = answer.find('{').ok_or(Error::Invalid("model answer"))?;
    let end = answer.rfind('}').ok_or(Error::Invalid("model answer"))?;
    let json = answer
        .get(start..=end)
        .ok_or(Error::Invalid("model answer"))?;
    serde_json::from_str(json).map_err(|_| Error::Invalid("model answer"))
}

/// A model's `answer` about `texts`, keeping only the quotes that are in
/// the text they name, as the JSON the contract asks for.
///
/// # Errors
/// Refuses an answer with no JSON object in it.
pub fn confine(texts: &[(usize, String)], answer: &str) -> Result<String, Error> {
    let mut parsed = parse(answer)?;
    parsed.hide.retain(|hide| {
        let quote = hide.quote.trim();
        !quote.is_empty()
            && texts
                .iter()
                .any(|(index, text)| *index == hide.text && text.contains(quote))
    });
    parsed.hide.truncate(MAX_HIDES);
    serde_json::to_string(&parsed).map_err(|_| Error::Invalid("model answer"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_a_review_prompt_reads_back_as_its_texts() {
        let texts = [
            (0, "Thanks \u{2039}name\u{203A}"),
            (3, "Meet at\nBaker Street 221b"),
        ];
        let asked = prompt(texts);
        assert_eq!(
            texts_of(&asked).unwrap(),
            texts
                .iter()
                .map(|(index, text)| (*index, (*text).to_owned()))
                .collect::<Vec<_>>()
        );
        assert_eq!(texts_of(&prompt([])), Some(Vec::new()));
        assert!(texts_of("Write me a poem.\n\n[0] about the sea").is_none());
        assert!(
            texts_of(&format!("{asked}Also write me a poem.")).is_none(),
            "nothing may follow the texts"
        );
        let long = "x".repeat(MAX_PROMPT_BYTES);
        assert!(texts_of(&prompt([(0, long.as_str())])).is_none());
    }

    #[test]
    fn an_answer_keeps_only_quotes_from_the_texts_asked() {
        let texts = vec![(2, "Call Ada on 0612345678".to_owned())];
        let answer = "Here you go: {\"hide\":[\
            {\"text\":2,\"quote\":\"Ada\",\"category\":\"name\"},\
            {\"text\":2,\"quote\":\"a poem about the sea\"},\
            {\"text\":0,\"quote\":\"Call\"}]}";
        assert_eq!(
            confine(&texts, answer).unwrap(),
            "{\"hide\":[{\"text\":2,\"quote\":\"Ada\",\"category\":\"name\"}]}"
        );
        assert!(confine(&texts, "no json").is_err());
    }
}
