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
    ApiKey, Briefing, MOST_DISCOVERY_TURNS, ModelRequest, NoPlanReads, PLANNER_INSTRUCTIONS,
    PlanModel, ReadView, answer_call, first_turn_parts, planning_tools, post_hedged,
};
use crate::vision::{Eyes, Picture};

/// The Messages API version the requests are written for.
const ANTHROPIC_VERSION: &str = "2023-06-01";

/// The most a planning turn may write. A plan is a few thousand tokens at
/// most; the bound only keeps a runaway answer from running on.
const MOST_OUTPUT_TOKENS: u32 = 16_000;

/// The efforts Claude takes (`output_config.effort`).
const EFFORTS: &[&str] = &["low", "medium", "high", "xhigh", "max"];

/// Models known to refuse a tool call asked for outright (`tool_choice`
/// `any`): they think before they answer, so every agent's first call
/// would otherwise be refused and sent again.
const UNFORCED: &[&str] = &["claude-sonnet-5", "claude-opus-5", "claude-fable-"];

/// The models this process has seen refuse a forced tool call, so an agent
/// started after the first refusal asks the way the model takes.
static REFUSED_FORCING: std::sync::Mutex<Vec<String>> = std::sync::Mutex::new(Vec::new());

/// Whether `model` may be asked for a tool call outright.
fn forcible(model: &str) -> bool {
    !UNFORCED.iter().any(|prefix| model.starts_with(prefix))
        && !REFUSED_FORCING
            .lock()
            .is_ok_and(|refused| refused.iter().any(|seen| seen == model))
}

/// Remember that `model` refused a forced tool call.
fn refused_forcing(model: &str) {
    if let Ok(mut refused) = REFUSED_FORCING.lock()
        && !refused.iter().any(|seen| seen == model)
    {
        refused.push(model.to_owned());
    }
}

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
    /// Models that think before they answer refuse that; those known to are
    /// never forced, and the first refusal leaves the choice to the model
    /// from then on, for every agent of this process.
    forced: bool,
    /// Whether it is shown pictures, set outright
    /// ([`crate::vision::VISION_ENV`]); `None` leaves its default, shown.
    vision: Option<bool>,
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
            forced: forcible(model),
            vision: None,
            client: std::sync::Arc::new(std::sync::OnceLock::new()),
        }
    }

    /// Show it pictures (`Some(true)`) or not (`Some(false)`); `None`
    /// leaves the default, which is to.
    #[must_use]
    pub fn with_vision(mut self, vision: Option<bool>) -> Self {
        self.vision = vision;
        self
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
            pictures: crate::vision::pictures_in(body),
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
                refused_forcing(&self.model);
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

/// Drop the cache breakpoints earlier turns' results carried; the first
/// message (the reference) keeps its own.
fn forget_turn_breakpoints(messages: &mut [serde_json::Value]) {
    for message in messages.iter_mut().skip(1) {
        if let Some(blocks) = message
            .get_mut("content")
            .and_then(serde_json::Value::as_array_mut)
        {
            for block in blocks {
                if let Some(block) = block.as_object_mut() {
                    block.remove("cache_control");
                }
            }
        }
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

/// The first turn: the reference (a cache breakpoint), then the rest, then
/// `pictures`, after what is cached.
fn first_message((stable, rest): (&str, &str), pictures: &[Picture]) -> serde_json::Value {
    let mut content = Vec::new();
    if !stable.is_empty() {
        content.push(serde_json::json!({
            "type": "text",
            "text": stable,
            "cache_control": { "type": "ephemeral" },
        }));
    }
    content.push(serde_json::json!({ "type": "text", "text": rest }));
    content.extend(pictures.iter().map(Picture::anthropic_block));
    serde_json::json!({ "role": "user", "content": content })
}

/// A call's result block: its text alone, or the text then its pictures.
fn tool_result(id: &str, text: String, pictures: &[Picture]) -> serde_json::Value {
    let content = if pictures.is_empty() {
        serde_json::Value::String(text)
    } else {
        let mut blocks = vec![serde_json::json!({ "type": "text", "text": text })];
        blocks.extend(pictures.iter().map(Picture::anthropic_block));
        serde_json::Value::Array(blocks)
    };
    serde_json::json!({
        "type": "tool_result",
        "tool_use_id": id,
        "content": content,
    })
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
    fn sees(&self) -> bool {
        self.vision.unwrap_or(true)
    }

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
            &mut NoPlanReads,
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
        let mut eyes = Eyes::new(self.sees());
        eyes.show(&stable);
        eyes.show(&rest);
        let (note, first_pictures) = eyes.attach_first(&briefing.pictures);
        let tools = claude_tools(&planning_tools(
            most_steps,
            briefing.describes(),
            briefing.searches(),
            eyes.sees(),
        ));
        let mut messages = vec![first_message((&stable, &(rest + &note)), &first_pictures)];
        let mut reminded = false;
        let mut repeats = crate::model::Repeats::default();
        for turn in 0..MOST_DISCOVERY_TURNS {
            eyes.next_turn();
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
            let (mut seen, mut answered) = (String::new(), String::new());
            for call in &calls {
                let result = match answer_call(
                    (&call.name, &call.input),
                    (catalogue, briefing, most_steps),
                    &mut revealed,
                    (&eyes, read),
                    false,
                ) {
                    std::ops::ControlFlow::Break(answer) => return Ok(answer),
                    std::ops::ControlFlow::Continue(seen) => seen,
                };
                let (result, pictures) = eyes.attach(result);
                crate::model::Repeats::call(&mut seen, &call.name, &call.input, &result);
                answered.clone_from(&result);
                results.push(tool_result(&call.id, result, &pictures));
            }
            repeats.note(seen, &answered)?;
            // The answer as given (its thinking included), then the results,
            // the latest turn ending a cache breakpoint for the next. A
            // request takes four breakpoints: the tools and the reference
            // keep theirs, and only the latest turn has one.
            forget_turn_breakpoints(&mut messages);
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
        let body = model.body(&tools, &[first_message(("reference", "the task"), &[])]);
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
        let body = thinking.body(&tools, &[first_message(("reference", "the task"), &[])]);
        assert_eq!(body["tool_choice"]["type"], "auto");
    }

    #[test]
    fn a_model_known_to_think_first_is_never_forced() {
        let model = |name: &str| {
            AnthropicModel::new(
                "https://api.anthropic.com/v1",
                name,
                ApiKey::None,
                std::time::Duration::from_secs(60),
            )
        };
        for name in ["claude-sonnet-5-5", "claude-opus-5-5", "claude-fable-5-1"] {
            assert!(!model(name).forced, "{name}");
        }
        assert!(model("claude-haiku-5-5").forced);
        refused_forcing("claude-refuses-later-1");
        assert!(!model("claude-refuses-later-1").forced);
    }

    #[test]
    fn only_the_latest_turn_keeps_a_cache_breakpoint() {
        let result = |id: &str| {
            serde_json::json!({"role": "user", "content": [
                {"type": "tool_result", "tool_use_id": id, "content": "ok",
                 "cache_control": {"type": "ephemeral"}}
            ]})
        };
        let mut messages = vec![
            first_message(("reference", "the task"), &[]),
            result("a"),
            result("b"),
        ];
        forget_turn_breakpoints(&mut messages);
        assert_eq!(
            messages[0]["content"][0]["cache_control"]["type"],
            "ephemeral"
        );
        assert!(messages[1]["content"][0].get("cache_control").is_none());
        assert!(messages[2]["content"][0].get("cache_control").is_none());
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

    /// A Messages answer calling `name` with `input`.
    fn claude_call(id: &str, name: &str, input: &serde_json::Value) -> String {
        crate::model::tests::http(
            "200 OK",
            &serde_json::json!({"content": [
                {"type": "tool_use", "id": id, "name": name, "input": input}]})
            .to_string(),
        )
    }

    /// A re-plan's picture follows the first turn's text, a look's picture
    /// rides in its tool result after its text, the cached reference stays
    /// byte-identical across turns, and only the last result block carries
    /// the latest turn's cache breakpoint.
    #[test]
    fn pictures_follow_the_cached_prefix_and_ride_in_tool_results() {
        use crate::Planner as _;
        use crate::model::tests::{Looks, endpoint, look_context};
        let (url, sent) = endpoint(vec![
            claude_call(
                "t1",
                crate::READ_VIEW,
                &serde_json::json!({"operation": "canvas.artwork#look", "arguments": {}}),
            ),
            claude_call(
                "t2",
                tilo_agent::tools::PLAN,
                &serde_json::json!({"steps": [{"operation": "canvas.artwork#draw",
                    "arguments": {"marks": []}}], "then": null}),
            ),
        ]);
        let mut reads = Looks::new('a');
        let earlier = reads.keep('f', crate::vision::tests::png(5, 5));
        let mut context = look_context();
        context.request = Some("draw the band, then look again".into());
        context.pictures = vec![earlier];
        let model = AnthropicModel::new(
            &url,
            "claude-haiku-5-5",
            ApiKey::None,
            std::time::Duration::from_secs(30),
        );
        assert!(model.sees());
        crate::ModelPlanner::new(Box::new(model))
            .plan_reading("look again", &context, &mut reads)
            .unwrap();
        let requests: Vec<serde_json::Value> = sent
            .try_iter()
            .map(|(_, body)| serde_json::from_str(&body).unwrap())
            .collect();
        let [first, second] = requests.as_slice() else {
            panic!("{requests:?}");
        };
        let tools = serde_json::to_string(&first["tools"]).unwrap();
        assert!(tools.contains(crate::vision::LOOK_CONTENT), "{tools}");
        let opening = first["messages"][0]["content"].as_array().unwrap();
        assert_eq!(opening[0]["cache_control"]["type"], "ephemeral");
        assert_eq!(opening[2]["type"], "image", "the picture after the text");
        assert!(
            opening[1]["text"]
                .as_str()
                .unwrap()
                .contains("picture shown below, 5×5 PNG")
        );
        assert_eq!(
            first["messages"][0]["content"][0].to_string(),
            second["messages"][0]["content"][0].to_string(),
            "the cached reference is the same bytes every turn"
        );
        assert_eq!(first["system"], second["system"]);
        assert_eq!(first["tools"], second["tools"]);
        let results = second["messages"][2]["content"].as_array().unwrap();
        let result = results.last().unwrap();
        assert_eq!(result["type"], "tool_result");
        assert_eq!(result["cache_control"]["type"], "ephemeral");
        let blocks = result["content"].as_array().unwrap();
        assert_eq!(blocks[0]["type"], "text");
        assert!(
            blocks[0]["text"]
                .as_str()
                .unwrap()
                .contains("picture shown below, 8×6 PNG")
        );
        assert_eq!(blocks[1]["type"], "image");
        assert_eq!(blocks[1]["source"]["type"], "base64");
        assert_eq!(blocks[1]["source"]["media_type"], "image/png");
        assert!(
            blocks
                .iter()
                .all(|block| block.get("cache_control").is_none()),
            "only the last result block carries the breakpoint"
        );
        assert!(
            opening[1..]
                .iter()
                .all(|block| block.get("cache_control").is_none()),
            "the first turn's breakpoint stays on its reference alone"
        );
    }
}
