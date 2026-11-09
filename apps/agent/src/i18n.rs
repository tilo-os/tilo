//! The agent's catalogue: the sentences a person reads in the reply a failed
//! turn leaves.
//!
//! The agent has no window, but what it writes into a conversation is read
//! wherever the interface speaks another language, so its copy goes through
//! the same catalogue an app's does. The catalogue is embedded, so a test
//! resolves the source copy on any machine.

ui::embedded_catalog!(pub fn, ["en-US", "de-DE"]);

#[cfg(test)]
mod tests {
    const SOURCE_FTL: &str = include_str!("../locales/en-US.ftl");
    const DE_FTL: &str = include_str!("../locales/de-DE.ftl");

    /// Every message the source catalogue has, the German one has too, so a
    /// German conversation never mixes languages.
    #[test]
    fn the_german_catalogue_covers_every_message() {
        let ids = |source: &str| -> Vec<String> {
            source
                .lines()
                .filter(|line| !line.starts_with([' ', '#', '*', '[', '}']) && line.contains(" ="))
                .filter_map(|line| line.split(" =").next().map(str::to_string))
                .collect()
        };
        let english = ids(SOURCE_FTL);
        let german = ids(DE_FTL);
        assert!(!english.is_empty());
        for id in &english {
            assert!(german.contains(id), "de-DE lacks {id}");
        }
        for id in &german {
            assert!(english.contains(id), "en-US lacks {id}");
        }
    }

    /// Every message the source catalogue has reads as a sentence a person
    /// could be shown: it is not a key, and it carries nothing only the agent's
    /// record needs.
    #[test]
    fn no_failure_sentence_carries_the_runs_record() {
        let messages: Vec<&str> = SOURCE_FTL
            .lines()
            .filter(|line| !line.starts_with([' ', '#', '*', '[', '}']) && line.contains(" ="))
            .collect();
        assert!(!messages.is_empty());
        for line in messages {
            let said = line.split_once(" = ").map_or("", |(_, said)| said);
            assert!(!said.trim().is_empty(), "{line} says nothing");
            assert!(!said.contains("Retry inference"), "{line}");
            assert!(!said.contains("replay completed actions"), "{line}");
        }
    }
}
