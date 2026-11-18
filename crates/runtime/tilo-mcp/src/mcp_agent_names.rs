//! The name a caller writes for an app, read as the app it means.
//!
//! An app is named exactly by its id, by the last part of its id and by the name the listing
//! shows it by. A model asked to work in the word processor writes `word-processor`,
//! `word_processor`, `Word Processor`, `WordProcessor`, `Word` or `so.tilo.word_processor`, and
//! each of those is that one app: a name is compared in lower case and without the hyphens,
//! underscores and blanks that part its words, and the start of a name stands for it where one
//! app alone begins so.
//!
//! A name is only ever read among the apps the caller was listed already, so reading it more
//! freely finds no app a stricter spelling could not have named; what the caller may do with
//! the app it finds is decided where it always was. A name that fits several apps equally well
//! chooses none of them.

/// The fewest letters the start of a name stands for the name with.
const START_CHARS: usize = 3;

/// `name` as names are compared: in lower case, without the hyphens, underscores and blanks
/// that part its words.
pub(super) fn folded(name: &str) -> String {
    name.trim()
        .chars()
        .filter(|character| !matches!(character, '-' | '_' | ' '))
        .flat_map(char::to_lowercase)
        .collect()
}

/// The app `name` means among `apps`, each an id with the name the listing shows it by (empty
/// where it shows none): the app whose id it is, else whose id ends in it, else that is listed
/// by it, else the one whose name begins with it. `None` when it names none of them. A name
/// with a dot in it is an id and is compared with ids whole.
///
/// # Errors
/// `name` fits several apps equally well: they are named, and none is chosen.
pub(super) fn app_meant(name: &str, apps: &[(&str, &str)]) -> Result<Option<String>, String> {
    let wanted = folded(name);
    if wanted.is_empty() {
        return Ok(None);
    }
    let dotted = wanted.contains('.');
    let begins = wanted.chars().count() >= START_CHARS;
    let last = |app_id: &str| folded(app_id.rsplit('.').next().unwrap_or(app_id));
    let fits = |how: u8, app_id: &str, label: &str| match how {
        0 => folded(app_id) == wanted,
        1 => !dotted && last(app_id) == wanted,
        2 => !dotted && !label.is_empty() && folded(label) == wanted,
        _ => {
            !dotted
                && begins
                && (last(app_id).starts_with(&wanted)
                    || (!label.is_empty() && folded(label).starts_with(&wanted)))
        }
    };
    for how in 0..4u8 {
        let mut meant: Vec<&str> = Vec::new();
        for &(app_id, label) in apps {
            if fits(how, app_id, label) && !meant.contains(&app_id) {
                meant.push(app_id);
            }
        }
        match meant.as_slice() {
            [] => {}
            [one] => return Ok(Some((*one).to_owned())),
            several => {
                return Err(format!(
                    "{name} names several apps: {}; name one",
                    several.join(", ")
                ));
            }
        }
    }
    Ok(None)
}

#[cfg(test)]
mod tests {
    use super::{app_meant, folded};

    const APPS: &[(&str, &str)] = &[
        ("so.tilo.word-processor", "Word Processor"),
        ("so.tilo.notes", "Notes"),
        ("so.tilo.tasks", "Tasks"),
        ("so.tilo.file-explorer", "Files"),
        ("so.tilo.video-player", "Video Player"),
        ("so.tilo.video-editor", "Video Editor"),
        ("so.tilo.word-processor", "Word Processor"),
    ];

    #[test]
    fn a_name_is_its_app_in_every_spelling_a_model_writes() {
        for spelled in [
            "word-processor",
            "word_processor",
            "word processor",
            "Word Processor",
            "WordProcessor",
            "WORD-PROCESSOR",
            "Word",
            " word_processor ",
            "so.tilo.word-processor",
            "so.tilo.word_processor",
            "So.Tilo.Word-Processor",
        ] {
            assert_eq!(
                app_meant(spelled, APPS),
                Ok(Some("so.tilo.word-processor".to_owned())),
                "{spelled}"
            );
        }
        for spelled in [
            "files",
            "Files",
            "file_explorer",
            "File Explorer",
            "FILE-EXPLORER",
        ] {
            assert_eq!(
                app_meant(spelled, APPS),
                Ok(Some("so.tilo.file-explorer".to_owned())),
                "{spelled}"
            );
        }
        assert_eq!(folded(" Word_Processor "), "wordprocessor");
    }

    #[test]
    fn a_name_that_fits_several_apps_chooses_none_of_them() {
        let several = app_meant("video", APPS).expect_err("two apps begin so");
        assert!(
            several.contains("names several")
                && several.contains("so.tilo.video-player")
                && several.contains("so.tilo.video-editor"),
            "{several}"
        );
        // A whole name is its app, whatever else begins with it.
        let apps = [
            ("so.tilo.notes", "Notes"),
            ("so.tilo.notes-archive", "Notes Archive"),
        ];
        assert_eq!(
            app_meant("Notes", &apps),
            Ok(Some("so.tilo.notes".to_owned()))
        );
        assert!(app_meant("not", &apps).is_err());
    }

    #[test]
    fn a_name_no_app_has_names_none() {
        for spelled in [
            "",
            "  ",
            "-",
            "wo",
            "window",
            "processor",
            "tilo",
            "so.tilo",
            "so.tilo.word",
            "word.processor",
            "tasks.tracker",
        ] {
            assert_eq!(app_meant(spelled, APPS), Ok(None), "{spelled}");
        }
    }
}
