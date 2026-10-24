//! Known model providers: what a new profile for each starts with.
//!
//! A preset is a constructor, not stored state: it fills the name, the
//! endpoint, the protocol and where the endpoint runs, and says how the
//! person authenticates. Everything it fills stays editable, and a profile
//! made from a preset is an ordinary [`ProviderProfile`]: nothing records
//! which preset made it, and listing its models follows from its protocol
//! and sign-in alone ([`crate::transform::models_url`]).

use crate::{
    EndpointLocation, OpenAiApi, ProviderProfile, ProviderProtocol, SecretReference,
    SignInCredential, SignInIssuer, TokenLimitParam,
};

/// One known provider.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Preset {
    OpenAi,
    Anthropic,
    /// Google's Gemini API (AI Studio), through its OpenAI-compatible
    /// surface; signs in with an API key or with Google.
    Gemini,
    Mistral,
    OpenRouter,
    Groq,
    Xai,
    DeepSeek,
    Together,
    Fireworks,
    HuggingFace,
    Ollama,
    /// LM Studio's local server on this device.
    LmStudio,
    /// Any other OpenAI-compatible endpoint, typed by hand.
    Compatible,
    /// A `ChatGPT` subscription, signed in with the person's `OpenAI` account.
    ChatGpt,
    /// A GitHub Copilot subscription, signed in with GitHub.
    Copilot,
}

/// How a preset's profile authenticates.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Authentication {
    /// No credential: a local server.
    None,
    /// An API key in this device's vault.
    ApiKey,
    /// A sign-in held by account custody.
    SignIn(SignInIssuer),
}

impl Preset {
    /// Every preset, in the order Settings offers them.
    pub const ALL: [Self; 16] = [
        Self::ChatGpt,
        Self::OpenAi,
        Self::Anthropic,
        Self::Gemini,
        Self::Copilot,
        Self::OpenRouter,
        Self::Mistral,
        Self::Groq,
        Self::Xai,
        Self::DeepSeek,
        Self::Together,
        Self::Fireworks,
        Self::HuggingFace,
        Self::Ollama,
        Self::LmStudio,
        Self::Compatible,
    ];

    /// The stable name an action takes.
    pub const fn slug(self) -> &'static str {
        match self {
            Self::OpenAi => "openai",
            Self::Anthropic => "anthropic",
            Self::Gemini => "gemini",
            Self::Mistral => "mistral",
            Self::OpenRouter => "openrouter",
            Self::Groq => "groq",
            Self::Xai => "xai",
            Self::DeepSeek => "deepseek",
            Self::Together => "together",
            Self::Fireworks => "fireworks",
            Self::HuggingFace => "huggingface",
            Self::Ollama => "ollama",
            Self::LmStudio => "lm-studio",
            Self::Compatible => "openai-compatible",
            Self::ChatGpt => "chatgpt",
            Self::Copilot => "github-copilot",
        }
    }

    pub fn from_slug(slug: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|preset| preset.slug() == slug)
    }

    /// The provider's name.
    pub const fn label(self) -> &'static str {
        match self {
            Self::OpenAi => "OpenAI",
            Self::Anthropic => "Anthropic",
            Self::Gemini => "Google Gemini",
            Self::Mistral => "Mistral",
            Self::OpenRouter => "OpenRouter",
            Self::Groq => "Groq",
            Self::Xai => "xAI",
            Self::DeepSeek => "DeepSeek",
            Self::Together => "Together AI",
            Self::Fireworks => "Fireworks AI",
            Self::HuggingFace => "Hugging Face",
            Self::Ollama => "Ollama",
            Self::LmStudio => "LM Studio",
            Self::Compatible => "OpenAI-compatible provider",
            Self::ChatGpt => "ChatGPT",
            Self::Copilot => "GitHub Copilot",
        }
    }

    pub const fn protocol(self) -> ProviderProtocol {
        match self {
            Self::Anthropic => ProviderProtocol::Anthropic,
            _ => ProviderProtocol::OpenAiCompatible,
        }
    }

    /// The endpoint a new profile starts with.
    pub const fn endpoint(self) -> &'static str {
        match self {
            Self::OpenAi | Self::Compatible => "https://api.openai.com/v1",
            Self::Anthropic => "https://api.anthropic.com",
            Self::Gemini => GEMINI_ENDPOINT,
            Self::Mistral => "https://api.mistral.ai/v1",
            Self::OpenRouter => OPENROUTER_ENDPOINT,
            Self::Groq => "https://api.groq.com/openai/v1",
            Self::Xai => "https://api.x.ai/v1",
            Self::DeepSeek => "https://api.deepseek.com/v1",
            Self::Together => "https://api.together.xyz/v1",
            Self::Fireworks => "https://api.fireworks.ai/inference/v1",
            Self::HuggingFace => "https://router.huggingface.co/v1",
            Self::Ollama => "http://127.0.0.1:11434/v1",
            Self::LmStudio => "http://127.0.0.1:1234/v1",
            Self::ChatGpt => CHATGPT_ENDPOINT,
            Self::Copilot => COPILOT_ENDPOINT,
        }
    }

    pub const fn location(self) -> EndpointLocation {
        match self {
            Self::Ollama | Self::LmStudio => EndpointLocation::ThisDevice,
            _ => EndpointLocation::Hosted,
        }
    }

    /// How a new profile authenticates. Gemini, `OpenRouter` and Hugging
    /// Face take an API key; signing in is the other way to set them up.
    pub const fn authentication(self) -> Authentication {
        match self {
            Self::Ollama | Self::LmStudio => Authentication::None,
            Self::ChatGpt => Authentication::SignIn(SignInIssuer::ChatGpt),
            Self::Copilot => Authentication::SignIn(SignInIssuer::GitHubCopilot),
            _ => Authentication::ApiKey,
        }
    }

    /// The output-token field this provider takes, when it is not the one
    /// callers send. `OpenAI`'s current models refuse `max_tokens`.
    const fn token_limit_param(self) -> Option<TokenLimitParam> {
        match self {
            Self::OpenAi => Some(TokenLimitParam::MaxCompletionTokens),
            _ => None,
        }
    }

    /// A new profile from this preset, with a fresh id. Its model is chosen
    /// afterwards, from the provider's list or typed.
    pub fn profile(self) -> ProviderProfile {
        let id = uuid::Uuid::new_v4().to_string();
        let (credential, sign_in) = match self.authentication() {
            Authentication::None => (None, None),
            Authentication::ApiKey => (Some(SecretReference::provider_api_key(&id)), None),
            Authentication::SignIn(issuer) => (None, Some(SignInCredential::new(issuer))),
        };
        ProviderProfile {
            label: self.label().to_string(),
            protocol: self.protocol(),
            endpoint: self.endpoint().to_string(),
            location: self.location(),
            credential,
            sign_in,
            token_limit_param: self.token_limit_param(),
            openai_api: (self == Self::OpenAi).then_some(OpenAiApi::Responses),
            id,
            ..ProviderProfile::default()
        }
    }
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
        for preset in Preset::ALL {
            assert_eq!(Preset::from_slug(preset.slug()), Some(preset));
            let config = ProviderConfig {
                profiles: vec![preset.profile()],
                ..ProviderConfig::default()
            };
            assert_eq!(config.validation_errors(), [], "{preset:?}");
        }
        let slugs: std::collections::BTreeSet<_> =
            Preset::ALL.iter().map(|preset| preset.slug()).collect();
        assert_eq!(slugs.len(), Preset::ALL.len());
    }

    #[test]
    fn local_servers_need_no_key_and_subscriptions_sign_in() {
        let ollama = Preset::Ollama.profile();
        assert_eq!(ollama.location, EndpointLocation::ThisDevice);
        assert!(ollama.credential.is_none() && ollama.sign_in.is_none());
        assert!(crate::is_supervised_loopback(&ollama));
        let chatgpt = Preset::ChatGpt.profile();
        assert_eq!(chatgpt.sign_in_issuer(), Some(SignInIssuer::ChatGpt));
        assert!(chatgpt.credential.is_none());
        let openai = Preset::OpenAi.profile();
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
