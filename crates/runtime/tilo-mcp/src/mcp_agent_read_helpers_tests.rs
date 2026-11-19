//! A listing's own tests read it through what a structured read answers and the
//! names and paths the listing writes.
use super::*;

/// The actions a listing writes, as the names an agent would call: one line
/// each, `#name(params?)  What it does`.
pub(super) fn listed_actions(listing: &str) -> Vec<String> {
    listing
        .lines()
        .filter_map(|line| line.strip_prefix('#'))
        .map(|rest| rest.split(['(', ' ']).next().unwrap_or(rest).to_string())
        .collect()
}

/// The resources a listing names under `app`, as the paths it writes them by:
/// `Todo/  notes.page` is `notes/Todo`.
pub(super) fn listed_paths(listing: &str, app: &str) -> Vec<String> {
    listing
        .lines()
        .map(str::trim_end)
        .filter(|line| !line.starts_with('#') && !line.starts_with(&format!("{app}  (")))
        .filter_map(|line| line.split_whitespace().next())
        .map(|name| format!("{app}/{}", name.trim_end_matches('/')))
        .collect()
}

tilo_test_env::scenario! {
    fn tilo_find_answers_by_the_desktops_search_in_the_paths_ls_lists;

    phase find_answers_by_the_desktops_one_search_or_says_there_is_none;
    // `find` answers by the one search the desktop runs, and a notes without one
    // says so: it never answers by a ranking of its own, which is what the
    // palette and text typed on the canvas would not agree with.
    {
        let notes = Notes::new(true);
        run(async {
            let (text, failed) = tool(&agent(&notes), "tilo_find", json!({"query": "Groceries"})).await;
            assert!(!failed, "{text}");
            // The note leads: a row about the note's body is about the note, too,
            // and every row names the app's path down to what it is about.
            assert_eq!(
                text.lines().next(),
                Some("notes/Notebook/Groceries  notes.page"),
                "{text}"
            );
            // A notes with no search of its own is told so, never given a second
            // ranking's answers.
            let no_search = NoSearch::new(&notes);
            let (text, failed) = tool(
                &agent(&no_search),
                "tilo_find",
                json!({"query": "Groceries"}),
            )
            .await;
            assert!(failed, "{text}");
            assert!(text.contains("keeps no search of its workspace"), "{text}");
        });
    }

    phase a_search_narrows_to_an_app_by_name_or_id_and_names_the_apps_when_none_is;
    // A search narrows to an app by the name `tilo_ls` lists or by its whole
    // id, and an app that is not there is an error naming the ones that are,
    // never an empty search.
    {
        let notes = Notes::new(true);
        run(async {
            let query = |app: &str| json!({"query": "append paragraph", "app": app});
            let (by_name, failed) = tool(&agent(&notes), "tilo_find", query("notes")).await;
            assert!(!failed, "{by_name}");
            assert!(
                by_name
                    .lines()
                    .any(|line| line.starts_with("notes/Notebook/Todo/body#append-paragraph")),
                "{by_name}"
            );
            for app in [APP, "SO.TILO.NOTES", " Notes "] {
                let (text, failed) = tool(&agent(&notes), "tilo_find", query(app)).await;
                assert_eq!((text.as_str(), failed), (by_name.as_str(), false), "{app}");
            }
            let (text, failed) = tool(&agent(&notes), "tilo_find", query("so.tilo.nope")).await;
            assert!(failed, "{text}");
            assert_eq!(text, "no app so.tilo.nope; the apps: notes, sheets");
        });
    }

    phase a_search_narrowed_to_an_app_says_when_the_installed_apps_could_not_be_read;
    // A search narrowed to an app that is not running, while the installed
    // apps cannot be read, says why the agent cannot know whether it is
    // installed: it is never told there is no such app. A running app named
    // still searches.
    {
        run(async {
            let query = |app: &str| json!({"query": "append paragraph", "app": app});
            let stopped = Notes::new(false).installed(Installed::Unreadable);
            let (text, failed) = tool(&agent(&stopped), "tilo_find", query("notes")).await;
            assert!(failed, "{text}");
            assert_eq!(
                text,
                "no running app is named notes, and the installed apps could not be read: this session's workspaces cannot be read"
            );
            let running = Notes::new(true).installed(Installed::Unreadable);
            let (text, failed) = tool(&agent(&running), "tilo_find", query(APP)).await;
            assert!(!failed, "{text}");
            assert!(
                text.lines()
                    .any(|line| line.starts_with("notes/Notebook/Todo/body#append-paragraph")),
                "{text}"
            );
            let (text, failed) = tool(&agent(&running), "tilo_find", query("sheets")).await;
            assert!(
                failed
                    && text.starts_with("no running app is named sheets")
                    && text.contains("installed apps could not be read"),
                "{text}"
            );
        });
    }

    phase every_row_find_answers_is_one_ls_lists;
    // Every row `find` answers is named by the path `ls` lists that resource
    // at, and `ls` there answers the row's own kind: one path model, read back
    // through the same API.
    {
        let notes = Notes::new(true);
        run(async {
            for query in ["Todo", "Groceries", "append paragraph", "new note"] {
                let (text, failed) = tool(&agent(&notes), "tilo_find", json!({"query": query})).await;
                assert!(!failed, "{query}: {text}");
                for line in text.lines() {
                    let (path, rest) = line
                        .split_once("  ")
                        .unwrap_or_else(|| panic!("{query}: {line} names no kind"));
                    let resource = path.split('#').next().unwrap_or(path);
                    let (listed, failed) =
                        tool(&agent(&notes), "tilo_ls", json!({"path": resource})).await;
                    assert!(
                        !failed,
                        "{query}: find says {line}, ls reads no {resource}: {listed}"
                    );
                    let header = listed.lines().next().unwrap_or_default();
                    assert!(
                        header.starts_with(&format!("{resource}  (")),
                        "{query}: find says {line}, and ls reads {listed}"
                    );
                    // A resource row names the resource itself: `ls` there
                    // answers it as the kind find wrote. An action row names the
                    // resource it runs the action on, with the action after `#`:
                    // `ls` there lists that action.
                    if let Some((_, action)) = path.split_once('#') {
                        let action = action.split('(').next().unwrap_or(action);
                        assert!(
                            listed.lines().skip(1).any(|held| {
                                held.strip_prefix('#')
                                    .is_some_and(|held| held.split(['(', ' ']).next() == Some(action))
                            }),
                            "{query}: find says {line}, and ls there lists no #{action}: {listed}"
                        );
                    } else {
                        assert!(
                            header.ends_with(&format!("({})", rest.split("  ").next().unwrap_or(rest))),
                            "{query}: find says {line}, and ls reads {listed}"
                        );
                    }
                }
            }
        });
    }
}
