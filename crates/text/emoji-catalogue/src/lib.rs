//! The emoji catalogue (`emoji`) handed to the toolkit's shared emoji
//! picker and reaction strip ([`ui::emoji_catalogue::CATALOGUE`]).
//!
//! The toolkit does not link the catalogue: it is about half a megabyte of
//! names and keywords that only programs showing a picker need. Every
//! program that does (Chat, a document app with reactions, the shell that
//! paints window bars) calls [`offer`] before its first frame. Natively the
//! catalogue then comes from its engine library (`tilo-emoji-engine`) where
//! the app is built thin; in the browser desktop it stays in the module of
//! the app that offers it.

/// Hand the catalogue to the toolkit's picker and strip, in whichever
/// language the interface speaks when they ask. Calling it again is free,
/// so a program may call it wherever it loads its own messages.
pub fn offer() {
    static OFFERED: std::sync::Once = std::sync::Once::new();
    OFFERED.call_once(|| ui::emoji_catalogue::CATALOGUE.install(shared_catalogue));
}

/// One locale's catalogue as the picker reads it.
struct Shared(&'static emoji::Catalogue);

fn shared_catalogue(tag: &str) -> &'static dyn ui::emoji_catalogue::EmojiCatalogue {
    // One small wrapper per locale ever asked for (a handful), kept for the
    // process like the catalogues themselves.
    static SHARED: std::sync::Mutex<Vec<&'static Shared>> = std::sync::Mutex::new(Vec::new());
    let catalogue = emoji::Catalogue::for_locale(tag);
    let mut shared = SHARED
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    if let Some(found) = shared.iter().find(|s| std::ptr::eq(s.0, catalogue)) {
        return *found;
    }
    let made: &'static Shared = Box::leak(Box::new(Shared(catalogue)));
    shared.push(made);
    made
}

impl ui::emoji_catalogue::EmojiCatalogue for Shared {
    fn groups(&self) -> Vec<ui::emoji_catalogue::CatalogueGroup> {
        self.0
            .groups()
            .iter()
            .map(|group| ui::emoji_catalogue::CatalogueGroup {
                id: group.id(),
                label: group.label(),
                icon: group.icon(),
            })
            .collect()
    }

    fn group_emoji(&self, index: usize) -> Vec<&'static str> {
        self.0.groups().get(index).map_or_else(Vec::new, |group| {
            self.0
                .group_emoji(group)
                .iter()
                .map(emoji::Emoji::text)
                .collect()
        })
    }

    fn search(&self, query: &str, limit: usize) -> Vec<&'static str> {
        self.0
            .search(query, limit)
            .into_iter()
            .map(emoji::Emoji::text)
            .collect()
    }

    fn info(&self, text: &str) -> Option<ui::emoji_catalogue::EmojiInfo> {
        let found = self.0.lookup(text)?;
        Some(ui::emoji_catalogue::EmojiInfo {
            name: found.emoji.name(),
            tone: found
                .tone
                .and_then(|tone| emoji::SkinTone::ALL.iter().position(|t| *t == tone))
                .and_then(|at| u8::try_from(at).ok()),
            takes_tone: found.emoji.takes_skin_tone(),
        })
    }

    fn tones(&self, text: &str) -> Vec<&'static str> {
        let Some(found) = self.0.lookup(text) else {
            return Vec::new();
        };
        if !found.emoji.takes_skin_tone() {
            return Vec::new();
        }
        std::iter::once(None)
            .chain(emoji::SkinTone::ALL.into_iter().map(Some))
            .filter_map(|tone| self.0.with_skin_tone(found.emoji.text(), tone))
            .collect()
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn the_offered_catalogue_names_and_finds_emoji() {
        super::offer();
        let catalogue = ui::emoji_catalogue::catalogue().expect("offered");
        assert!(!catalogue.groups().is_empty());
        assert!(!catalogue.group_emoji(0).is_empty());
        assert!(catalogue.search("thumbs", 8).contains(&"👍"));
        assert!(catalogue.info("👍").is_some_and(|info| info.takes_tone));
        assert!(catalogue.tones("👍").len() > 1);
    }
}
