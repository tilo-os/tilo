//! How an action reads in a list people see.

/// One action as a person reads it in a list: what it does, and under it
/// what it acts on.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct ListedRow {
    pub title: String,
    pub subtitle: Option<String>,
}

/// The row for an action labelled `label`, which acts on the item named
/// `item` (none for the app or document itself): the label alone as the
/// title, the outcome in the person's words, and the item's name under it
/// when it acts on one. An action's description is written for agents and
/// scripts (what it takes, what it answers) and is never the line under a
/// person's row; it still finds the row in a search. Never the label joined
/// to the resource's name, a kind of control ("— Focus"), an id or a
/// separator glyph: every list of actions people see (an app's palette,
/// desktop search) builds its rows here.
#[must_use]
pub fn listed_row(label: &str, item: Option<&str>) -> ListedRow {
    let title = label.trim().to_owned();
    let subtitle = item
        .map(str::trim)
        .filter(|item| !item.is_empty() && *item != title)
        .map(str::to_owned);
    ListedRow { title, subtitle }
}

/// What is wrong with how `row` reads, if anything: a raw id (a run of ten
/// or more digits, a long hex string) anywhere, or in its label a separator
/// glyph joining facts or a control's own verb ("— Focus"). Empty for a row
/// that reads as an outcome in the person's words. The line under the label
/// may quote the person's own content (an article's title), so only ids are
/// looked for there. Every list of actions people see is held to this
/// ([`listed_row`] builds rows that pass).
#[must_use]
pub fn listed_row_problems(row: &ListedRow) -> Vec<&'static str> {
    let mut problems = Vec::new();
    for (index, text) in std::iter::once(row.title.as_str())
        .chain(row.subtitle.as_deref())
        .enumerate()
    {
        if text
            .split(|c: char| !c.is_ascii_alphanumeric())
            .any(|word| {
                (word.len() >= 10 && word.chars().all(|c| c.is_ascii_digit()))
                    || (word.len() >= 16
                        && word.chars().all(|c| c.is_ascii_hexdigit())
                        && word.chars().any(|c| c.is_ascii_digit()))
            })
        {
            problems.push("a raw id");
        }
        if index > 0 {
            continue;
        }
        if ['\u{b7}', '\u{2022}', '|']
            .iter()
            .any(|glyph| text.contains(*glyph))
            || text.contains(" \u{2014} ")
            || text.contains(" \u{2013} ")
        {
            problems.push("a separator glyph");
        }
        if ["Focus", "Activate", "Show context menu", "Set value"]
            .iter()
            .any(|verb| text.ends_with(&format!(" {verb}")) || text == *verb)
        {
            problems.push("a control's own verb");
        }
    }
    problems.dedup();
    problems
}
