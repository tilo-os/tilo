//! Model lists as providers answer them, read into what each model can do.
//!
//! `openrouter-models.json` is a trimmed recording of `OpenRouter`'s public
//! `GET /api/v1/models`. The others follow the providers' documented list
//! schemas (Anthropic's `capabilities`, Gemini's own list, Mistral's
//! `capabilities`, Moonshot's flat flags), since their lists need a key.

#[cfg(test)]
mod listings {
    use agent_provider_config::transform::parse_models;
    use agent_provider_config::{CapabilitySource, ModelDescriptor};

    fn listed(fixture: &str) -> Vec<ModelDescriptor> {
        let path = format!("{}/tests/fixtures/{fixture}", env!("CARGO_MANIFEST_DIR"));
        parse_models(&std::fs::read_to_string(path).unwrap()).unwrap()
    }

    fn model<'a>(models: &'a [ModelDescriptor], id: &str) -> &'a ModelDescriptor {
        models
            .iter()
            .find(|model| model.id == id)
            .unwrap_or_else(|| panic!("{id} is listed"))
    }

    /// (vision, tool calling, context) of `id`.
    fn caps(models: &[ModelDescriptor], id: &str) -> (bool, bool, Option<u32>) {
        let model = model(models, id);
        (model.vision, model.tool_calling, model.context_window)
    }

    #[test]
    fn openrouter_says_what_each_model_sees_and_whether_it_calls_tools() {
        let models = listed("openrouter-models.json");
        assert_eq!(models.len(), 9);
        assert_eq!(
            caps(&models, "anthropic/claude-sonnet-4.5"),
            (true, true, Some(1_000_000))
        );
        assert_eq!(
            caps(&models, "deepseek/deepseek-chat-v3.1"),
            (false, true, Some(163_840))
        );
        // A model that sees but takes no tools, and one that does neither.
        assert_eq!(
            caps(&models, "moondream/moondream3-preview"),
            (true, false, Some(32_000))
        );
        assert_eq!(
            caps(&models, "tencent/hy-mt2-7b"),
            (false, false, Some(8192))
        );
        assert_eq!(
            model(&models, "openai/gpt-4o-mini").label,
            "OpenAI: GPT-4o-mini"
        );
        assert!(
            models
                .iter()
                .all(|model| model.capabilities_from == CapabilitySource::Provider)
        );
    }

    #[test]
    fn anthropic_lists_image_input_and_its_context() {
        let models = listed("anthropic-models.json");
        assert_eq!(
            caps(&models, "claude-sonnet-4-5"),
            (true, true, Some(1_000_000))
        );
        // Anthropic's list says nothing of tools: the family table says Claude
        // calls them.
        assert_eq!(
            caps(&models, "claude-haiku-text-only"),
            (false, true, Some(200_000))
        );
        assert_eq!(
            model(&models, "claude-sonnet-4-5").label,
            "Claude Sonnet 4.5"
        );
    }

    #[test]
    fn geminis_own_list_reads_its_limits_and_embedding_models() {
        let models = listed("gemini-models.json");
        let pro = model(&models, "models/gemini-2.5-pro");
        assert_eq!(pro.label, "Gemini 2.5 Pro");
        // Its list says nothing of images: the family table does.
        assert_eq!(
            (pro.vision, pro.tool_calling, pro.context_window),
            (true, true, Some(1_048_576))
        );
        assert_eq!(
            caps(&models, "models/gemini-embedding-001"),
            (false, false, Some(2048))
        );
    }

    #[test]
    fn mistral_and_moonshot_flags_are_read() {
        let mistral = listed("mistral-models.json");
        assert_eq!(
            caps(&mistral, "pixtral-large-latest"),
            (true, true, Some(131_072))
        );
        assert_eq!(caps(&mistral, "mistral-embed"), (false, false, Some(8192)));
        let moonshot = listed("moonshot-models.json");
        assert_eq!(
            caps(&moonshot, "kimi-k2-0905-preview"),
            (false, true, Some(262_144))
        );
        assert_eq!(caps(&moonshot, "kimi-latest"), (true, true, Some(131_072)));
    }
}
