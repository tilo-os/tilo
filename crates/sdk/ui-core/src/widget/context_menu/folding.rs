//! Folding a run of a resource's actions into one menu row that opens them:
//! `Due` in a task's menu stands for Today, Tomorrow, Next week and the rest.

use super::MenuItem;

/// What a row of a folded menu (`MenuItem::fold_numbered`) stands for: the command
/// whose index the row was given, or the indices of the run of commands a group row
/// opens, in the order the person meets them.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum MenuPick {
    One(usize),
    Group(Vec<usize>),
}

impl MenuItem {
    /// It takes something away: it is undone, not confirmed, and is drawn as such.
    #[must_use]
    pub fn with_destructive(mut self, destructive: bool) -> Self {
        if let MenuItem::Action {
            destructive: item, ..
        } = &mut self
        {
            *item = destructive;
        }
        self
    }

    /// Name the run of commands this row stands in (`group: "Due"`): consecutive
    /// rows of one run fold into a row of their own that opens them
    /// ([`MenuItem::fold_groups`]).
    #[must_use]
    pub fn with_group(mut self, group: impl Into<String>) -> Self {
        if let MenuItem::Action {
            group: item_group, ..
        } = &mut self
        {
            *item_group = Some(group.into());
        }
        self
    }

    /// The run of commands the row stands in, when it names one.
    pub fn group(&self) -> Option<&str> {
        match self {
            MenuItem::Action { group, .. } => group.as_deref(),
            MenuItem::Separator | MenuItem::Caption(_) | MenuItem::Emoji(_) => None,
        }
    }

    /// The rows with each run of consecutive commands of one group (`group: "Due"`)
    /// folded into one row named for the group, which opens them: the first group
    /// row keeps the run's icon and shows a check when one of its commands is on.
    /// A run of one command stays the command itself, since a menu that opens one
    /// row says nothing its label does not. The commands keep their order and
    /// their keys, and say why they cannot run now.
    #[must_use]
    pub fn fold_groups(items: Vec<MenuItem>) -> Vec<MenuItem> {
        Self::fold_numbered(items.into_iter().enumerate().collect()).0
    }

    /// [`Self::fold_groups`], keeping what each folded row stands for: the command
    /// whose index it was given, or the indices of the run a group row opens.
    #[must_use]
    pub fn fold_numbered(
        rows: Vec<(usize, MenuItem)>,
    ) -> (Vec<MenuItem>, Vec<crate::widget::MenuPick>) {
        use crate::widget::MenuPick;
        let mut folded: Vec<MenuItem> = Vec::with_capacity(rows.len());
        let mut picks: Vec<MenuPick> = Vec::with_capacity(rows.len());
        let mut rest = rows.into_iter().peekable();
        while let Some((index, item)) = rest.next() {
            let Some(group) = item.group().map(str::to_owned) else {
                folded.push(item);
                picks.push(MenuPick::One(index));
                continue;
            };
            let mut run = vec![(index, item)];
            while rest
                .peek()
                .is_some_and(|(_, next)| next.group() == Some(group.as_str()))
            {
                if let Some(next) = rest.next() {
                    run.push(next);
                }
            }
            if run.len() == 1 {
                let (index, item) = run.pop().unwrap_or((index, MenuItem::separator()));
                folded.push(item);
                picks.push(MenuPick::One(index));
                continue;
            }
            let icon = match &run[0].1 {
                MenuItem::Action { icon, .. } => *icon,
                _ => None,
            };
            let enabled = run.iter().any(|(_, item)| item.is_enabled());
            let checked = run.iter().find_map(|(_, item)| item.checked());
            let indices: Vec<usize> = run.iter().map(|(index, _)| *index).collect();
            let members: Vec<MenuItem> = run
                .into_iter()
                .map(|(_, item)| item.without_group())
                .collect();
            let mut row = MenuItem::submenu(group, members);
            if let MenuItem::Action {
                icon: row_icon,
                enabled: row_enabled,
                checked: row_checked,
                ..
            } = &mut row
            {
                *row_icon = icon;
                *row_enabled = enabled;
                *row_checked = checked;
            }
            folded.push(row);
            picks.push(MenuPick::Group(indices));
        }
        (folded, picks)
    }

    /// The command without its group, so a group row's members do not fold again.
    #[must_use]
    fn without_group(mut self) -> Self {
        if let MenuItem::Action { group, .. } = &mut self {
            *group = None;
        }
        self
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn labels(items: &[MenuItem]) -> Vec<String> {
        items
            .iter()
            .filter_map(|item| item.label().map(str::to_owned))
            .collect()
    }

    /// Rows of one run stand as the run's own row, in the run's place, and a row of no run and
    /// a run of one stand as they were given.
    #[test]
    fn consecutive_rows_of_one_run_fold_into_its_row() {
        let folded = MenuItem::fold_groups(vec![
            MenuItem::selection("Complete"),
            MenuItem::selection("Today").with_group("Due"),
            MenuItem::selection("Tomorrow").with_group("Due"),
            MenuItem::selection("Next week").with_group("Due"),
            MenuItem::selection("Delete"),
            MenuItem::selection("Low").with_group("Priority"),
            MenuItem::selection("High").with_group("Priority"),
            MenuItem::selection("Alone").with_group("Rare"),
        ]);
        assert_eq!(
            labels(&folded),
            ["Complete", "Due", "Delete", "Priority", "Alone"],
            "each run stands as one row where it was declared"
        );
        let MenuItem::Action {
            submenu: Some(due), ..
        } = &folded[1]
        else {
            panic!("a run of three opens its members");
        };
        assert_eq!(labels(due), ["Today", "Tomorrow", "Next week"]);
        assert_eq!(
            folded[1].group(),
            None,
            "the run's own row is not a member of itself"
        );
        assert_eq!(
            due[0].group(),
            None,
            "a member answers to its own name only"
        );
        let MenuItem::Action {
            submenu: Some(priority),
            ..
        } = &folded[3]
        else {
            panic!("a run of two opens its members");
        };
        assert_eq!(labels(priority), ["Low", "High"]);
        assert!(matches!(folded[4], MenuItem::Action { submenu: None, .. }));
    }

    /// Two runs that meet at only one row apart do not merge, and a run a row breaks stands
    /// as two rows of the same name.
    #[test]
    fn a_row_of_no_run_breaks_a_run() {
        let folded = MenuItem::fold_groups(vec![
            MenuItem::selection("Today").with_group("Due"),
            MenuItem::selection("Tomorrow").with_group("Due"),
            MenuItem::selection("Delete"),
            MenuItem::selection("Next week").with_group("Due"),
            MenuItem::selection("No date").with_group("Due"),
        ]);
        assert_eq!(labels(&folded), ["Due", "Delete", "Due"]);
    }

    /// A run's members keep their places, so a choice made in a folded row names the action
    /// the person read, not the row it now stands in.
    #[test]
    fn folding_keeps_each_rows_place() {
        let (items, picks) = MenuItem::fold_numbered(vec![
            (0, MenuItem::selection("Complete")),
            (1, MenuItem::selection("Today").with_group("Due")),
            (2, MenuItem::selection("Tomorrow").with_group("Due")),
            (3, MenuItem::selection("Delete")),
        ]);
        assert_eq!(labels(&items), ["Complete", "Due", "Delete"]);
        assert_eq!(
            picks,
            [
                MenuPick::One(0),
                MenuPick::Group(vec![1, 2]),
                MenuPick::One(3)
            ]
        );
    }

    /// A run's row says what the run is for and whether any member can run now: it is drawn
    /// as unavailable only while every member is.
    #[test]
    fn a_run_is_unavailable_only_when_all_of_its_members_are() {
        let folded = MenuItem::fold_groups(vec![
            MenuItem::selection("No priority")
                .with_group("Priority")
                .with_enabled(false)
                .with_description("It has no priority already"),
            MenuItem::selection("High").with_group("Priority"),
        ]);
        let MenuItem::Action {
            enabled,
            icon,
            submenu: Some(members),
            ..
        } = &folded[0]
        else {
            panic!("a run of two opens its members");
        };
        assert!(enabled, "one member can run, so the run opens");
        assert_eq!(*icon, None, "the run takes the first row's glyph");
        assert!(!members[0].is_enabled(), "the member keeps its own refusal");
    }
}
