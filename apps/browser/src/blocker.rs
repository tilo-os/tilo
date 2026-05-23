use adblock::engine::Engine;
use adblock::lists::FilterSet;

pub struct AdGuard {
    engine: Option<Engine>,
}

impl AdGuard {
    pub fn new() -> Self {
        Self {
            engine: None,
        }
    }

    pub fn load_initial_rules(&mut self) {
        let filter_set = FilterSet::new(false);
        self.engine = Some(Engine::from_filter_set(filter_set, true));
    }

    #[allow(dead_code)]
    pub fn generate_css(&self, url: &str) -> String {
        if let Some(ref engine) = self.engine {
            let resources = engine.url_cosmetic_resources(url);
            if !resources.hide_selectors.is_empty() {
                let selectors: Vec<String> = resources.hide_selectors.iter().map(|s| s.to_string()).collect();
                let css = format!("{} {{ display: none !important; }}", selectors.join(", "));
                return format!(
                    "const style = document.createElement('style'); style.textContent = `{}`; document.head.append(style);",
                    css
                );
            }
        }
        String::new()
    }
}
