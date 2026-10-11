//! A planning model behind Anthropic's Messages API (`…/v1/messages`): the
//! same tools, first turn, reads and refusals as [`OpenAiModel`]'s Responses
//! path, spoken the way Claude takes them. The conversation is resent every
//! turn (the API keeps none); the instructions and tools, the apps'
//! reference and the latest turn each end at a cache breakpoint, so what
//! another agent or the next turn shares is read back at a tenth of the
//! price.
//!
//! [`OpenAiModel`]: crate::model::OpenAiModel

use tilo_agent::surface::Catalogue;
use tilo_agent::tools::Answer;

use crate::model::{
    ApiKey, Briefing, MOST_DISCOVERY_TURNS, ModelRequest, PLANNER_INSTRUCTIONS, PlanModel,
    READ_VIEW, ReadView, carried_gap, first_turn_parts, planning_tools, post_hedged,
    read_view_answer,
};

/// The Messages API version the requests are written for.
const ANTHROPIC_VERSION: &str = "2023-06-01";

/// The most a planning turn may write. A plan is a few thousand tokens at
/// most; the bound only keeps a runaway answer from running on.
const MOST_OUTPUT_TOKENS: u32 = 16_000;

/// The efforts Claude takes (`output_config.effort`).
const EFFORTS: &[&str] = &["low", "medium", "high", "xhigh", "max"];

/// A Claude model at a Messages API base URL.
pub struct AnthropicModel {
    endpoint: String,
    model: String,
    api_key: ApiKey,
    timeout: std::time::Duration,
    effort: Option<String>,
    service_tier: Option<String>,
    hedge_after: Option<std::time::Duration>,
    /// Whether a tool call is asked for outright (`tool_choice` `any`).
    /// Models that think before they answer refuse that; the first refusal
    /// leaves the choice to the model from then on.
    forced: bool,
    client: std::sync::Arc<std::sync::OnceLock<Box<dyn std::any::Any + Send + Sync>>>,
}

impl AnthropicModel {
    /// `model` at `base_url` (`…/v1`) with `api_key`, each plan bounded by
    /// `timeout`.
    #[must_use]
    pub fn new(base_url: &str, model: &str, api_key: ApiKey, timeout: std::time::Duration) -> Self {
        Self {
            endpoint: format!("{}/messages", base_url.trim_end_matches('/')),
            model: model.to_owned(),
            api_key,
            timeout,
            effort: None,
            service_tier: None,
            hedge_after: None,
            forced: true,
            client: std::sync::Arc::new(std::sync::OnceLock::new()),
        }
    }

    /// Ask for `effort` (`low` … `max`); anything else leaves the model's
    /// default.
    #[must_use]
    pub fn with_effort(mut self, effort: Option<&str>) -> Self {
        self.effort = effort
            .map(|effort| effort.trim().to_ascii_lowercase())
            .filter(|effort| EFFORTS.contains(&effort.as_str()));
        self
    }

    /// Ask for service tier `tier` (`auto` or `standard_only`); Anthropic
    /// has no Flex or fast tier, so anything else leaves the default.
    #[must_use]
    pub fn with_service_tier(mut self, tier: Option<&str>) -> Self {
        self.service_tier = tier
            .map(str::trim)
            .filter(|tier| ["auto", "standard_only"].contains(tier))
            .map(str::to_owned);
        self
    }

    /// Hedge a call that has not answered within `after`.
    #[must_use]
    pub fn with_hedge(mut self, after: Option<std::time::Duration>) -> Self {
        self.hedge_after = after;
        self
    }

    fn post(
        &self,
        body: &serde_json::Value,
        key: &str,
        deadline: std::time::Instant,
    ) -> Result<serde_json::Value, String> {
        let mut headers = vec![("anthropic-version".to_owned(), ANTHROPIC_VERSION.to_owned())];
        if let Some(key) = self.api_key.read()? {
            headers.push(("x-api-key".to_owned(), key));
        }
        let request = ModelRequest {
            client: std::sync::Arc::clone(&self.client),
            endpoint: self.endpoint.clone(),
            idempotency: false,
            headers,
            body: std::sync::Arc::new(body.to_string()),
        };
        post_hedged(&request, &self.model, key, deadline, self.hedge_after)
    }

    /// Post one turn: `messages` with `tools`, asking for a tool call
    /// outright unless the model refused that before. A request refused
    /// while it forced one is sent once more leaving the choice to the
    /// model, and the model is never forced again.
    fn turn(
        &mut self,
        tools: &serde_json::Value,
        messages: &[serde_json::Value],
        key: &str,
        deadline: std::time::Instant,
    ) -> Result<serde_json::Value, String> {
        match self.post(&self.body(tools, messages), key, deadline) {
            Err(error) if self.forced && error.contains("HTTP 400") => {
                self.forced = false;
                self.post(
                    &self.body(tools, messages),
                    &format!("{key}-auto"),
                    deadline,
                )
            }
            answer => answer,
        }
    }

    /// One turn's request: the fixed instructions and tools first, then the
    /// conversation so far.
    fn body(&self, tools: &serde_json::Value, messages: &[serde_json::Value]) -> serde_json::Value {
        let mut body = serde_json::json!({
            "model": self.model,
            "max_tokens": MOST_OUTPUT_TOKENS,
            "system": [{ "type": "text", "text": PLANNER_INSTRUCTIONS }],
            "tools": tools,
            "tool_choice": { "type": if self.forced { "any" } else { "auto" } },
            "messages": messages,
        });
        if let Some(object) = body.as_object_mut() {
            if let Some(effort) = &self.effort {
                object.insert(
                    "output_config".into(),
                    serde_json::json!({ "effort": effort }),
                );
            }
            if let Some(tier) = &self.service_tier {
                object.insert("service_tier".into(), tier.clone().into());
            }
        }
        body
    }
}

/// The planning tools as Claude takes them, the last ending a cache
/// breakpoint so the instructions and tools are cached together.
fn claude_tools(definitions: &serde_json::Value) -> serde_json::Value {
    let mut tools: Vec<serde_json::Value> = definitions
        .as_array()
        .into_iter()
        .flatten()
        .map(|tool| {
            serde_json::json!({
                "name": tool.get("name").cloned().unwrap_or(serde_json::Value::Null),
                "description": tool.get("description").cloned().unwrap_or(serde_json::Value::Null),
                "input_schema": tool
                    .get("input_schema")
                    .cloned()
                    .unwrap_or_else(|| serde_json::json!({ "type": "object" })),
            })
        })
        .collect();
    if let Some(last) = tools.last_mut().and_then(serde_json::Value::as_object_mut) {
        last.insert(
            "cache_control".into(),
            serde_json::json!({ "type": "ephemeral" }),
        );
    }
    serde_json::Value::Array(tools)
}

/// The first turn: the reference (a cache breakpoint), then the rest.
fn first_message((stable, rest): (&str, &str)) -> serde_json::Value {
    let mut content = Vec::new();
    if !stable.is_empty() {
        content.push(serde_json::json!({
            "type": "text",
            "text": stable,
            "cache_control": { "type": "ephemeral" },
        }));
    }
    content.push(serde_json::json!({ "type": "text", "text": rest }));
    serde_json::json!({ "role": "user", "content": content })
}

/// A tool call of a Messages answer: its id, name and input.
struct Call {
    id: String,
    name: String,
    input: serde_json::Value,
}

/// The tool calls of `answer` and its text, if any.
fn read_answer(answer: &serde_json::Value) -> (Vec<Call>, String) {
    let mut calls = Vec::new();
    let mut text = String::new();
    for block in answer
        .get("content")
        .and_then(serde_json::Value::as_array)
        .into_iter()
        .flatten()
    {
        match block.get("type").and_then(serde_json::Value::as_str) {
            Some("tool_use") => calls.push(Call {
                id: block
                    .get("id")
                    .and_then(serde_json::Value::as_str)
                    .unwrap_or_default()
                    .to_owned(),
                name: block
                    .get("name")
                    .and_then(serde_json::Value::as_str)
                    .unwrap_or_default()
                    .to_owned(),
                input: block.get("input").cloned().unwrap_or_default(),
            }),
            Some("text") => {
                text.push_str(
                    block
                        .get("text")
                        .and_then(serde_json::Value::as_str)
                        .unwrap_or_default(),
                );
            }
            _ => {}
        }
    }
    (calls, text)
}

impl PlanModel for AnthropicModel {
    fn choose(
        &mut self,
        task: &str,
        catalogue: &Catalogue,
        most_steps: Option<usize>,
    ) -> Result<Answer, String> {
        self.choose_briefed(
            task,
            &Briefing::default(),
            catalogue,
            most_steps,
            &mut |_, _| Err("no document can be read here".into()),
        )
    }

    fn choose_briefed(
        &mut self,
        task: &str,
        briefing: &Briefing,
        catalogue: &Catalogue,
        most_steps: Option<usize>,
        read: &mut ReadView<'_>,
    ) -> Result<Answer, String> {
        let deadline = std::time::Instant::now()
            .checked_add(self.timeout)
            .unwrap_or_else(std::time::Instant::now);
        let mut run = [0_u8; 16];
        getrandom::fill(&mut run).map_err(|_| "no randomness for the model call".to_string())?;
        let mut revealed = tilo_agent::tools::Revealed::new();
        let (stable, rest) = first_turn_parts(task, briefing, catalogue, &mut revealed);
        let tools = claude_tools(&planning_tools(most_steps, briefing.describes()));
        let mut messages = vec![first_message((&stable, &rest))];
        let mut reminded = false;
        for turn in 0..MOST_DISCOVERY_TURNS {
            let key = format!("{}-{turn}", hex::encode(run));
            let answer = self.turn(&tools, &messages, &key, deadline)?;
            let (calls, text) = read_answer(&answer);
            if calls.is_empty() && !self.forced && !reminded {
                // Left the choice, the model wrote instead of calling a
                // tool: it is reminded once.
                reminded = true;
                messages.push(serde_json::json!({
                    "role": "assistant",
                    "content": answer.get("content").cloned().unwrap_or_default(),
                }));
                messages.push(serde_json::json!({
                    "role": "user",
                    "content": "Answer by calling one of the tools.",
                }));
                continue;
            }
            if calls.is_empty() {
                return Err(format!(
                    "the model neither discovered operations nor planned: {}",
                    text.chars().take(300).collect::<String>()
                ));
            }
            let mut results = Vec::with_capacity(calls.len());
            for call in &calls {
                let result = if call.name == tilo_agent::tools::PLAN {
                    match tilo_agent::tools::read_plan(&call.input, &revealed, most_steps) {
                        Ok(answer) => match carried_gap(&answer, catalogue) {
                            None => return Ok(answer),
                            Some(gap) => tilo_agent::tools::plan_refused(&gap),
                        },
                        Err(error) => tilo_agent::tools::plan_refused(&error),
                    }
                } else if call.name == READ_VIEW {
                    read_view_answer(&call.input, &revealed, read)
                } else {
                    tilo_agent::tools::answer(&call.name, &call.input, catalogue, &mut revealed)
                };
                results.push(serde_json::json!({
                    "type": "tool_result",
                    "tool_use_id": call.id,
                    "content": result,
                }));
            }
            // The answer as given (its thinking included), then the results,
            // the latest turn ending a cache breakpoint for the next.
            messages.push(serde_json::json!({
                "role": "assistant",
                "content": answer.get("content").cloned().unwrap_or_default(),
            }));
            if let Some(last) = results
                .last_mut()
                .and_then(serde_json::Value::as_object_mut)
            {
                last.insert(
                    "cache_control".into(),
                    serde_json::json!({ "type": "ephemeral" }),
                );
            }
            messages.push(serde_json::json!({ "role": "user", "content": results }));
        }
        Err(format!(
            "the model did not plan within {MOST_DISCOVERY_TURNS} turns"
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_request_caches_tools_and_reference_and_carries_effort() {
        let model = AnthropicModel::new(
            "https://api.anthropic.com/v1",
            "claude-haiku-5-5",
            ApiKey::None,
            std::time::Duration::from_secs(60),
        )
        .with_effort(Some("Low"))
        .with_service_tier(Some("flex"));
        let tools = claude_tools(&serde_json::json!([
            {"name": "read_view", "description": "Read.", "input_schema": {"type": "object"}},
            {"name": "plan", "description": "Plan.", "input_schema": {"type": "object"}},
        ]));
        let body = model.body(&tools, &[first_message(("reference", "the task"))]);
        assert_eq!(body["output_config"]["effort"], "low");
        assert!(
            body.get("service_tier").is_none(),
            "Anthropic has no flex tier"
        );
        assert_eq!(body["tool_choice"]["type"], "any");
        assert!(body["tools"][0].get("cache_control").is_none());
        assert_eq!(body["tools"][1]["cache_control"]["type"], "ephemeral");
        let content = &body["messages"][0]["content"];
        assert_eq!(content[0]["cache_control"]["type"], "ephemeral");
        assert!(content[1].get("cache_control").is_none());
        assert_eq!(model.endpoint, "https://api.anthropic.com/v1/messages");
        let mut thinking = model;
        thinking.forced = false;
        let body = thinking.body(&tools, &[first_message(("reference", "the task"))]);
        assert_eq!(body["tool_choice"]["type"], "auto");
    }

    #[test]
    fn an_answer_gives_its_tool_calls() {
        let (calls, text) = read_answer(&serde_json::json!({
            "content": [
                {"type": "thinking", "thinking": "…"},
                {"type": "text", "text": "Planning."},
                {"type": "tool_use", "id": "toolu_1", "name": "plan", "input": {"steps": []}},
            ]
        }));
        assert_eq!(text, "Planning.");
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].id, "toolu_1");
        assert_eq!(calls[0].name, "plan");
    }
}
