//! Known model providers: the catalogue a new profile starts from.
//!
//! The catalogue is data: each entry is a row of [`Preset::ALL`] saying the
//! provider's name, what it is, its protocol, endpoint, the header its key
//! travels in, where it lists its models, how the person signs in and where
//! to read about it. Adding a provider is adding a row.
//!
//! A preset is a constructor, not stored state: it fills a new
//! [`ProviderProfile`], and everything it fills stays editable. Nothing
//! records which preset made a profile; the profile carries what the preset
//! said (its key header and models path among them), so listing its models
//! and calling them follow from the profile alone
//! ([`crate::transform::models_url`], [`crate::transform::profile_headers`]).
//! A person can add as many profiles of one preset as they like (two
//! `OpenRouter` keys, say): each has its own id and key.

use crate::{
    EndpointLocation, KeyHeader, OpenAiApi, ProviderProfile, ProviderProtocol, SecretReference,
    SignInCredential, SignInIssuer, TokenLimitParam,
};

/// One known provider: a row of the catalogue.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Preset {
    slug: &'static str,
    label: &'static str,
    description: &'static str,
    protocol: ProviderProtocol,
    endpoint: &'static str,
    endpoint_editable: bool,
    key_header: KeyHeader,
    models_path: Option<&'static str>,
    docs: &'static str,
    authentication: Authentication,
    location: EndpointLocation,
    token_limit_param: Option<TokenLimitParam>,
    openai_api: Option<OpenAiApi>,
}

/// How a preset's profile authenticates.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Authentication {
    /// No credential: a local server.
    None,
    /// An API key in this device's vault.
    ApiKey,
    /// A sign-in held by account custody.
    SignIn(SignInIssuer),
}

impl Authentication {
    /// The stable name an action and a catalogue listing use.
    pub const fn slug(self) -> &'static str {
        match self {
            Self::None => "none",
            Self::ApiKey => "api-key",
            Self::SignIn(_) => "sign-in",
        }
    }
}

/// A hosted OpenAI-compatible provider taking an API key as a bearer token
/// and listing its models at `/models`: the common row, which the
/// modifiers below adjust.
const fn hosted(
    slug: &'static str,
    label: &'static str,
    description: &'static str,
    endpoint: &'static str,
    docs: &'static str,
) -> Preset {
    Preset {
        slug,
        label,
        description,
        protocol: ProviderProtocol::OpenAiCompatible,
        endpoint,
        endpoint_editable: false,
        key_header: KeyHeader::Bearer,
        models_path: Some("models"),
        docs,
        authentication: Authentication::ApiKey,
        location: EndpointLocation::Hosted,
        token_limit_param: None,
        openai_api: None,
    }
}

impl Preset {
    /// Speaks Anthropic's Messages API, its key in `x-api-key`.
    const fn anthropic(mut self) -> Self {
        self.protocol = ProviderProtocol::Anthropic;
        self.key_header = KeyHeader::XApiKey;
        self.models_path = Some("v1/models");
        self
    }

    /// Takes its key in `header`.
    const fn key_in(mut self, header: KeyHeader) -> Self {
        self.key_header = header;
        self
    }

    /// Lists its models at `path` under the endpoint (from the endpoint's
    /// origin when it starts with `/`), or with `None` lists none: its
    /// models are typed.
    const fn models_at(mut self, path: Option<&'static str>) -> Self {
        self.models_path = path;
        self
    }

    /// The person types or completes the endpoint: a generic entry, one
    /// whose endpoint names their account, or a server they run.
    const fn editable(mut self) -> Self {
        self.endpoint_editable = true;
        self
    }

    /// Runs on this device and needs no key.
    const fn on_this_device(mut self) -> Self {
        self.location = EndpointLocation::ThisDevice;
        self.authentication = Authentication::None;
        self.endpoint_editable = true;
        self
    }

    /// A server the person runs at `location`, reached with a key they
    /// gave it.
    const fn self_run(mut self, location: EndpointLocation) -> Self {
        self.location = location;
        self.endpoint_editable = true;
        self
    }

    /// Signs in with `issuer` instead of taking a key.
    const fn signs_in(mut self, issuer: SignInIssuer) -> Self {
        self.authentication = Authentication::SignIn(issuer);
        self
    }

    /// Names its output-token limit `param`.
    const fn token_limit(mut self, param: TokenLimitParam) -> Self {
        self.token_limit_param = Some(param);
        self
    }

    /// Takes agent turns through `api`.
    const fn api(mut self, api: OpenAiApi) -> Self {
        self.openai_api = Some(api);
        self
    }
}

impl Preset {
    pub const OPENAI: Self = hosted(
        "openai",
        "OpenAI",
        "GPT models from OpenAI's own API.",
        "https://api.openai.com/v1",
        "https://developers.openai.com/api/reference",
    )
    .token_limit(TokenLimitParam::MaxCompletionTokens)
    .api(OpenAiApi::Responses);
    pub const ANTHROPIC: Self = hosted(
        "anthropic",
        "Anthropic",
        "Claude models from Anthropic's API, with a Console key.",
        "https://api.anthropic.com",
        "https://platform.claude.com/docs/en/api/overview",
    )
    .anthropic();
    /// Google's Gemini API (AI Studio), through its OpenAI-compatible
    /// surface; signs in with an API key or with Google.
    pub const GEMINI: Self = hosted(
        "gemini",
        "Google Gemini",
        "Gemini models from Google AI Studio.",
        GEMINI_ENDPOINT,
        "https://ai.google.dev/gemini-api/docs/openai",
    );
    pub const MISTRAL: Self = hosted(
        "mistral",
        "Mistral",
        "Mistral's models, from Mistral AI's platform.",
        "https://api.mistral.ai/v1",
        "https://docs.mistral.ai/api/",
    );
    pub const OPENROUTER: Self = hosted(
        "openrouter",
        "OpenRouter",
        "One key for hundreds of models from many providers.",
        OPENROUTER_ENDPOINT,
        "https://openrouter.ai/docs/quickstart",
    );
    pub const GROQ: Self = hosted(
        "groq",
        "Groq",
        "Open models at very high speed on Groq's LPUs.",
        "https://api.groq.com/openai/v1",
        "https://console.groq.com/docs/openai",
    );
    pub const XAI: Self = hosted(
        "xai",
        "xAI",
        "Grok models from xAI.",
        "https://api.x.ai/v1",
        "https://docs.x.ai/docs/overview",
    );
    pub const DEEPSEEK: Self = hosted(
        "deepseek",
        "DeepSeek",
        "DeepSeek's chat and reasoning models.",
        "https://api.deepseek.com",
        "https://api-docs.deepseek.com/",
    );
    pub const DEEPSEEK_ANTHROPIC: Self = hosted(
        "deepseek-anthropic",
        "DeepSeek, Anthropic API",
        "DeepSeek's models through its Anthropic-compatible endpoint.",
        "https://api.deepseek.com/anthropic",
        "https://api-docs.deepseek.com/guides/anthropic_api",
    )
    .anthropic()
    .key_in(KeyHeader::Bearer)
    .models_at(Some("/models"));
    pub const TOGETHER: Self = hosted(
        "together",
        "Together AI",
        "Open models served by Together AI.",
        "https://api.together.ai/v1",
        "https://docs.together.ai/docs/inference/openai-compatibility",
    );
    pub const FIREWORKS: Self = hosted(
        "fireworks",
        "Fireworks AI",
        "Open models served by Fireworks AI.",
        "https://api.fireworks.ai/inference/v1",
        "https://docs.fireworks.ai/tools-sdks/openai-compatibility",
    )
    .models_at(None);
    pub const HUGGING_FACE: Self = hosted(
        "huggingface",
        "Hugging Face",
        "Open models through Hugging Face's inference providers.",
        "https://router.huggingface.co/v1",
        "https://huggingface.co/docs/inference-providers",
    );
    pub const AZURE_OPENAI: Self = hosted(
        "azure-openai",
        "Azure OpenAI",
        "OpenAI models deployed in your own Azure resource.",
        "https://{resource}.openai.azure.com/openai/v1",
        "https://learn.microsoft.com/en-us/azure/foundry/openai/api-version-lifecycle",
    )
    .key_in(KeyHeader::ApiKey)
    .editable()
    .token_limit(TokenLimitParam::MaxCompletionTokens);
    pub const CEREBRAS: Self = hosted(
        "cerebras",
        "Cerebras",
        "Open models at very high speed on Cerebras wafer-scale chips.",
        "https://api.cerebras.ai/v1",
        "https://inference-docs.cerebras.ai/resources/openai",
    );
    pub const PERPLEXITY: Self = hosted(
        "perplexity",
        "Perplexity",
        "Models that search the web as they answer, through Perplexity's Orchestrator.",
        "https://api.perplexity.ai/orchestrator/v1",
        "https://docs.perplexity.ai",
    );
    pub const COHERE: Self = hosted(
        "cohere",
        "Cohere",
        "Command models through Cohere's OpenAI-compatible API.",
        "https://api.cohere.ai/compatibility/v1",
        "https://docs.cohere.com/docs/compatibility-api",
    )
    .models_at(None);
    pub const MOONSHOT: Self = hosted(
        "moonshot",
        "Moonshot AI (Kimi)",
        "Kimi models from Moonshot AI.",
        "https://api.moonshot.ai/v1",
        "https://platform.kimi.ai/docs/api/overview",
    );
    pub const MOONSHOT_ANTHROPIC: Self = hosted(
        "moonshot-anthropic",
        "Moonshot AI (Kimi), Anthropic API",
        "Kimi models through Moonshot's Anthropic-compatible endpoint.",
        "https://api.moonshot.ai/anthropic",
        "https://platform.kimi.ai/docs/api/messages",
    )
    .anthropic()
    .key_in(KeyHeader::Bearer)
    .models_at(Some("/v1/models"));
    pub const DASHSCOPE: Self = hosted(
        "dashscope",
        "Alibaba Cloud Model Studio (Qwen)",
        "Qwen models from Alibaba Cloud's DashScope, international region.",
        "https://dashscope-intl.aliyuncs.com/compatible-mode/v1",
        "https://www.alibabacloud.com/help/en/model-studio/compatibility-of-openai-with-dashscope",
    );
    pub const ZAI: Self = hosted(
        "zai",
        "Z.ai (GLM)",
        "GLM models from Z.ai, billed per token.",
        "https://api.z.ai/api/paas/v4",
        "https://docs.z.ai",
    );
    pub const ZAI_CODING: Self = hosted(
        "zai-coding",
        "Z.ai GLM Coding Plan",
        "GLM models on a Z.ai Coding Plan subscription.",
        "https://api.z.ai/api/coding/paas/v4",
        "https://docs.z.ai/devpack/quick-start",
    );
    pub const ZAI_ANTHROPIC: Self = hosted(
        "zai-anthropic",
        "Z.ai (GLM), Anthropic API",
        "GLM models through Z.ai's Anthropic-compatible endpoint.",
        "https://api.z.ai/api/anthropic",
        "https://docs.z.ai/scenario-example/develop-tools/claude",
    )
    .anthropic()
    .key_in(KeyHeader::Bearer)
    .models_at(None);
    pub const MINIMAX: Self = hosted(
        "minimax",
        "MiniMax",
        "MiniMax's models, international platform.",
        "https://api.minimax.io/v1",
        "https://platform.minimax.io/docs/api-reference/text-openai-api",
    );
    pub const MINIMAX_ANTHROPIC: Self = hosted(
        "minimax-anthropic",
        "MiniMax, Anthropic API",
        "MiniMax's models through its Anthropic-compatible endpoint.",
        "https://api.minimax.io/anthropic",
        "https://platform.minimax.io/docs/api-reference/text-anthropic-api",
    )
    .anthropic()
    .key_in(KeyHeader::Bearer)
    .models_at(Some("/v1/models"));
    pub const NEBIUS: Self = hosted(
        "nebius",
        "Nebius Token Factory",
        "Open models served by Nebius.",
        "https://api.tokenfactory.nebius.com/v1",
        "https://docs.tokenfactory.nebius.com/",
    );
    pub const SAMBANOVA: Self = hosted(
        "sambanova",
        "SambaNova Cloud",
        "Open models at high speed on SambaNova's chips.",
        "https://api.sambanova.ai/v1",
        "https://docs.sambanova.ai",
    );
    pub const NOVITA: Self = hosted(
        "novita",
        "Novita AI",
        "Open models served by Novita AI.",
        "https://api.novita.ai/openai",
        "https://docs.novita.ai/guides/llm-api",
    );
    pub const DEEPINFRA: Self = hosted(
        "deepinfra",
        "DeepInfra",
        "Open models served by DeepInfra.",
        "https://api.deepinfra.com/v1/openai",
        "https://docs.deepinfra.com/chat/overview",
    );
    pub const HYPERBOLIC: Self = hosted(
        "hyperbolic",
        "Hyperbolic",
        "Open models served by Hyperbolic.",
        "https://api.hyperbolic.ai/inference/v1",
        "https://hyperbolic.ai/docs/inference/text-apis",
    );
    pub const VENICE: Self = hosted(
        "venice",
        "Venice AI",
        "Open models from Venice, which keeps no prompts.",
        "https://api.venice.ai/api/v1",
        "https://docs.venice.ai",
    );
    pub const CLOUDFLARE_WORKERS_AI: Self = hosted(
        "cloudflare-workers-ai",
        "Cloudflare Workers AI",
        "Open models on Cloudflare's network, billed to your account.",
        "https://api.cloudflare.com/client/v4/accounts/{account_id}/ai/v1",
        "https://developers.cloudflare.com/workers-ai/configuration/open-ai-compatibility/",
    )
    .editable()
    .models_at(None);
    pub const CLOUDFLARE_AI_GATEWAY: Self = hosted(
        "cloudflare-ai-gateway",
        "Cloudflare AI Gateway",
        "Your Cloudflare gateway in front of many providers.",
        "https://gateway.ai.cloudflare.com/v1/{account_id}/{gateway_id}/compat",
        "https://developers.cloudflare.com/ai-gateway/usage/chat-completion/",
    )
    .editable()
    .models_at(None);
    pub const VERCEL_AI_GATEWAY: Self = hosted(
        "vercel-ai-gateway",
        "Vercel AI Gateway",
        "One key for many providers' models, through Vercel.",
        "https://ai-gateway.vercel.sh/v1",
        "https://vercel.com/docs/ai-gateway/sdks-and-apis/openai-chat-completions",
    );
    pub const REQUESTY: Self = hosted(
        "requesty",
        "Requesty",
        "A router with one key for many providers' models.",
        "https://router.requesty.ai/v1",
        "https://docs.requesty.ai",
    );
    pub const LITELLM: Self = hosted(
        "litellm",
        "LiteLLM proxy",
        "A LiteLLM proxy you run, with its virtual key.",
        "http://127.0.0.1:4000",
        "https://docs.litellm.ai/docs/proxy/quick_start",
    )
    .self_run(EndpointLocation::ThisDevice);
    pub const OLLAMA: Self = hosted(
        "ollama",
        "Ollama",
        "Models Ollama runs on this device.",
        "http://127.0.0.1:11434/v1",
        "https://docs.ollama.com/api/openai-compatibility",
    )
    .on_this_device();
    /// LM Studio's local server on this device.
    pub const LM_STUDIO: Self = hosted(
        "lm-studio",
        "LM Studio",
        "Models LM Studio serves on this device.",
        "http://127.0.0.1:1234/v1",
        "https://lmstudio.ai/docs/developer/openai-compat",
    )
    .on_this_device();
    pub const VLLM: Self = hosted(
        "vllm",
        "vLLM",
        "A vLLM server you run on your network.",
        "http://{host}:8000/v1",
        "https://docs.vllm.ai/en/latest/serving/online_serving/",
    )
    .self_run(EndpointLocation::PrivateNetwork);
    pub const LLAMA_CPP: Self = hosted(
        "llama-cpp",
        "llama.cpp server",
        "A llama.cpp server you run.",
        "http://127.0.0.1:9931/v1",
        "https://github.com/ggml-org/llama.cpp/blob/master/tools/server/README.md",
    )
    .on_this_device();
    pub const LOCALAI: Self = hosted(
        "localai",
        "LocalAI",
        "A LocalAI server you run.",
        "http://127.0.0.1:8080/v1",
        "https://localai.io/docs/",
    )
    .on_this_device();
    pub const JAN: Self = hosted(
        "jan",
        "Jan",
        "Models Jan's local API server runs on this device.",
        "http://127.0.0.1:1337/v1",
        "https://docs.jan.ai/desktop/api-server",
    )
    .on_this_device();
    /// Any other OpenAI-compatible endpoint, typed by hand.
    pub const COMPATIBLE: Self = hosted(
        "openai-compatible",
        "OpenAI-compatible provider",
        "Any other endpoint that speaks OpenAI's chat completions.",
        "https://{host}/v1",
        "https://platform.openai.com/docs/api-reference/chat",
    )
    .editable();
    /// Any other Anthropic-compatible endpoint, typed by hand.
    pub const ANTHROPIC_COMPATIBLE: Self = hosted(
        "anthropic-compatible",
        "Anthropic-compatible provider",
        "Any other endpoint that speaks Anthropic's Messages API.",
        "https://{host}",
        "https://platform.claude.com/docs/en/api/messages",
    )
    .anthropic()
    .editable();
    /// A `ChatGPT` subscription, signed in with the person's `OpenAI` account.
    pub const CHATGPT: Self = hosted(
        "chatgpt",
        "ChatGPT",
        "Your ChatGPT subscription, signed in with your OpenAI account.",
        CHATGPT_ENDPOINT,
        "https://help.openai.com/en/articles/11369540-using-codex-with-your-chatgpt-plan",
    )
    .signs_in(SignInIssuer::ChatGpt);
    /// A GitHub Copilot subscription, signed in with GitHub.
    pub const COPILOT: Self = hosted(
        "github-copilot",
        "GitHub Copilot",
        "Your GitHub Copilot subscription, signed in with GitHub.",
        COPILOT_ENDPOINT,
        "https://docs.github.com/en/copilot",
    )
    .signs_in(SignInIssuer::GitHubCopilot);

    /// Every preset, in the order Settings offers them: subscriptions and
    /// the large providers first, then the other hosted ones, gateways,
    /// servers on this device, and the generic entries last.
    pub const ALL: &'static [Self] = &[
        Self::CHATGPT,
        Self::OPENAI,
        Self::ANTHROPIC,
        Self::GEMINI,
        Self::COPILOT,
        Self::OPENROUTER,
        Self::MISTRAL,
        Self::XAI,
        Self::DEEPSEEK,
        Self::DEEPSEEK_ANTHROPIC,
        Self::AZURE_OPENAI,
        Self::GROQ,
        Self::CEREBRAS,
        Self::TOGETHER,
        Self::FIREWORKS,
        Self::HUGGING_FACE,
        Self::PERPLEXITY,
        Self::COHERE,
        Self::MOONSHOT,
        Self::MOONSHOT_ANTHROPIC,
        Self::DASHSCOPE,
        Self::ZAI,
        Self::ZAI_CODING,
        Self::ZAI_ANTHROPIC,
        Self::MINIMAX,
        Self::MINIMAX_ANTHROPIC,
        Self::NEBIUS,
        Self::SAMBANOVA,
        Self::NOVITA,
        Self::DEEPINFRA,
        Self::HYPERBOLIC,
        Self::VENICE,
        Self::CLOUDFLARE_WORKERS_AI,
        Self::CLOUDFLARE_AI_GATEWAY,
        Self::VERCEL_AI_GATEWAY,
        Self::REQUESTY,
        Self::LITELLM,
        Self::OLLAMA,
        Self::LM_STUDIO,
        Self::VLLM,
        Self::LLAMA_CPP,
        Self::LOCALAI,
        Self::JAN,
        Self::COMPATIBLE,
        Self::ANTHROPIC_COMPATIBLE,
    ];

    /// The stable name an action takes.
    pub const fn slug(self) -> &'static str {
        self.slug
    }

    pub fn from_slug(slug: &str) -> Option<Self> {
        Self::ALL.iter().copied().find(|preset| preset.slug == slug)
    }

    /// The provider's name.
    pub const fn label(self) -> &'static str {
        self.label
    }

    /// What the provider is, in one line.
    pub const fn description(self) -> &'static str {
        self.description
    }

    /// Where to read about the provider's API and get a key.
    pub const fn docs(self) -> &'static str {
        self.docs
    }

    pub const fn protocol(self) -> ProviderProtocol {
        self.protocol
    }

    /// The endpoint a new profile starts with. One naming the person's
    /// account has the part they fill in braces ([`unfilled_part`]).
    pub const fn endpoint(self) -> &'static str {
        self.endpoint
    }

    /// Whether the person types or completes the endpoint, rather than
    /// taking the provider's.
    pub const fn endpoint_editable(self) -> bool {
        self.endpoint_editable
    }

    /// The header the key travels in.
    pub const fn key_header(self) -> KeyHeader {
        self.key_header
    }

    /// Where the provider lists its models under the endpoint; `None` when
    /// it lists none.
    pub const fn models_path(self) -> Option<&'static str> {
        self.models_path
    }

    pub const fn location(self) -> EndpointLocation {
        self.location
    }

    /// How a new profile authenticates. Gemini, `OpenRouter` and Hugging
    /// Face take an API key; signing in is the other way to set them up.
    pub const fn authentication(self) -> Authentication {
        self.authentication
    }

    /// Whether this preset is set up by signing in, which only a host that
    /// runs the sign-in offers.
    pub const fn signs_in_only(self) -> bool {
        matches!(self.authentication, Authentication::SignIn(_))
    }

    /// The preset `profile` was made from, as far as its protocol and
    /// endpoint tell: the first catalogue row with both. A profile whose
    /// endpoint was changed matches none.
    pub fn matching(profile: &ProviderProfile) -> Option<Self> {
        let endpoint = profile.endpoint.trim_end_matches('/');
        Self::ALL.iter().copied().find(|preset| {
            preset.protocol == profile.protocol && preset.endpoint.trim_end_matches('/') == endpoint
        })
    }

    /// A new profile from this preset, with a fresh id. Its model is chosen
    /// afterwards, from the provider's list or typed.
    pub fn profile(self) -> ProviderProfile {
        let id = uuid::Uuid::new_v4().to_string();
        let (credential, sign_in) = match self.authentication {
            Authentication::None => (None, None),
            Authentication::ApiKey => (Some(SecretReference::provider_api_key(&id)), None),
            Authentication::SignIn(issuer) => (None, Some(SignInCredential::new(issuer))),
        };
        let protocol_lists = match self.protocol {
            ProviderProtocol::OpenAiCompatible => "models",
            ProviderProtocol::Anthropic => "v1/models",
        };
        ProviderProfile {
            label: self.label.to_string(),
            protocol: self.protocol,
            endpoint: self.endpoint.to_string(),
            location: self.location,
            credential,
            sign_in,
            token_limit_param: self.token_limit_param,
            openai_api: self.openai_api,
            key_header: (self.key_header != KeyHeader::of(self.protocol))
                .then_some(self.key_header),
            models_path: match self.models_path {
                Some(path) if path == protocol_lists => None,
                Some(path) => Some(path.to_owned()),
                None => Some(String::new()),
            },
            id,
            ..ProviderProfile::default()
        }
    }
}

/// The part of `endpoint` the person still has to fill in, in braces as a
/// preset wrote it (`{resource}`), or `None` when it is complete.
pub fn unfilled_part(endpoint: &str) -> Option<&str> {
    let (_, open) = endpoint.split_once('{')?;
    let (part, _) = open.split_once('}')?;
    let start = endpoint.len().checked_sub(open.len())?.checked_sub(1)?;
    endpoint.get(start..start.checked_add(part.len())?.checked_add(2)?)
}

/// The Gemini API's OpenAI-compatible endpoint.
pub const GEMINI_ENDPOINT: &str = "https://generativelanguage.googleapis.com/v1beta/openai";
pub const OPENROUTER_ENDPOINT: &str = "https://openrouter.ai/api/v1";
/// The `ChatGPT` subscription backend the Codex clients use: `OpenAI`
/// Responses requests, authorised by the person's `ChatGPT` sign-in.
pub const CHATGPT_ENDPOINT: &str = "https://chatgpt.com/backend-api/codex";
/// GitHub Copilot's chat endpoint.
pub const COPILOT_ENDPOINT: &str = "https://api.githubcopilot.com";

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ProviderConfig;

    #[test]
    fn every_preset_makes_a_valid_profile() {
        for preset in Preset::ALL.iter().copied() {
            assert_eq!(Preset::from_slug(preset.slug()), Some(preset));
            let mut profile = preset.profile();
            // An account's part of the endpoint is the person's to fill in;
            // until then the profile says so.
            let unfilled = unfilled_part(&profile.endpoint).map(str::to_owned);
            assert_eq!(unfilled.is_some(), preset.endpoint().contains('{'));
            let config = ProviderConfig {
                profiles: vec![profile.clone()],
                ..ProviderConfig::default()
            };
            let issues: Vec<_> = config
                .validation_errors()
                .into_iter()
                .map(|issue| issue.message)
                .collect();
            match &unfilled {
                Some(part) => assert_eq!(issues, [format!("Fill in {part} in the endpoint.")]),
                None => assert_eq!(issues, Vec::<String>::new(), "{preset:?}"),
            }
            if unfilled.is_some() {
                while let Some(part) = unfilled_part(&profile.endpoint).map(str::to_owned) {
                    profile.endpoint = profile.endpoint.replace(&part, "acme");
                }
                let config = ProviderConfig {
                    profiles: vec![profile],
                    ..ProviderConfig::default()
                };
                assert_eq!(config.validation_errors(), [], "{preset:?}");
            }
        }
    }

    #[test]
    fn the_catalogue_is_complete_and_unambiguous() {
        let slugs: std::collections::BTreeSet<_> =
            Preset::ALL.iter().map(|preset| preset.slug()).collect();
        assert_eq!(slugs.len(), Preset::ALL.len());
        assert!(Preset::ALL.len() >= 40, "{}", Preset::ALL.len());
        for preset in Preset::ALL {
            assert!(!preset.label().is_empty() && !preset.description().is_empty());
            assert!(preset.description().ends_with('.'), "{preset:?}");
            assert!(preset.docs().starts_with("https://"), "{preset:?}");
            // A hosted provider is reached over TLS.
            if preset.location() == EndpointLocation::Hosted && !preset.endpoint_editable() {
                assert!(preset.endpoint().starts_with("https://"), "{preset:?}");
            }
        }
        let anthropic: Vec<_> = Preset::ALL
            .iter()
            .filter(|preset| preset.protocol() == ProviderProtocol::Anthropic)
            .map(|preset| preset.slug())
            .collect();
        assert_eq!(
            anthropic,
            [
                "anthropic",
                "deepseek-anthropic",
                "moonshot-anthropic",
                "zai-anthropic",
                "minimax-anthropic",
                "anthropic-compatible"
            ]
        );
    }

    #[test]
    fn profiles_carry_their_key_header_and_models_path() {
        let azure = Preset::AZURE_OPENAI.profile();
        assert_eq!(azure.key_header, Some(KeyHeader::ApiKey));
        assert_eq!(azure.models_path, None);
        let anthropic = Preset::ANTHROPIC.profile();
        assert_eq!((anthropic.key_header, anthropic.models_path), (None, None));
        let fireworks = Preset::FIREWORKS.profile();
        assert_eq!(fireworks.models_path.as_deref(), Some(""));
        assert_eq!(crate::transform::models_url(&fireworks), None);
        // Moonshot's Anthropic endpoint lists its models beside its OpenAI
        // one, and takes the key as a bearer token.
        let kimi = Preset::MOONSHOT_ANTHROPIC.profile();
        assert_eq!(kimi.key_header, Some(KeyHeader::Bearer));
        assert_eq!(
            crate::transform::models_url(&kimi).as_deref(),
            Some("https://api.moonshot.ai/v1/models")
        );
        // Two profiles of one preset are two providers.
        let (first, second) = (Preset::OPENROUTER.profile(), Preset::OPENROUTER.profile());
        assert_ne!(first.id, second.id);
        assert_ne!(first.credential, second.credential);
        assert_eq!(Preset::matching(&first), Some(Preset::OPENROUTER));
    }

    #[test]
    fn local_servers_need_no_key_and_subscriptions_sign_in() {
        let ollama = Preset::OLLAMA.profile();
        assert_eq!(ollama.location, EndpointLocation::ThisDevice);
        assert!(ollama.credential.is_none() && ollama.sign_in.is_none());
        assert!(crate::is_supervised_loopback(&ollama));
        let chatgpt = Preset::CHATGPT.profile();
        assert_eq!(chatgpt.sign_in_issuer(), Some(SignInIssuer::ChatGpt));
        assert!(chatgpt.credential.is_none());
        let openai = Preset::OPENAI.profile();
        assert_eq!(
            openai.credential,
            Some(SecretReference::provider_api_key(&openai.id))
        );
        assert_eq!(
            openai.token_limit_param,
            Some(TokenLimitParam::MaxCompletionTokens)
        );
    }
}
