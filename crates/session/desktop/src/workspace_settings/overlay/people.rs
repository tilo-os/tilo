//! The People tab's body, laid out by the toolkit: how many share the
//! workspace and where ownership stands, wrapped in full with its one
//! action under it, then one list row per person whose role and Remove
//! move under the name where the two do not fit beside each other.

use ui::icons::Icon;
use ui::text_render::TextRenderer;
use ui::theme::Theme;
use ui::tokens::{Rhythm, TextRole};
use ui::widget::{
    Button, ButtonRole, ChipStyle, Column, ControlSize, Element, EmptyState, ListRow, Row, Section,
    Text,
};

use super::super::{
    CapRole, UserId, Workspace, member_row_name, members_authority_note, ordered_roster,
    ownership_transfer_status, role_label,
};
use crate::i18n::{t, t_args};

/// The ownership action under the summary.
pub(super) const OWNERSHIP: &str = "ws-people-ownership";

pub(super) fn role_id(index: usize) -> String {
    format!("ws-people-role-{index}")
}

pub(super) fn remove_id(index: usize) -> String {
    format!("ws-people-remove-{index}")
}

/// One person of the list, in the viewer's words.
#[derive(Clone, Debug, PartialEq)]
pub(super) struct Person {
    pub user: UserId,
    pub name: String,
    pub you: bool,
    pub role: String,
    /// Their extra permissions, only when they have any.
    pub extra: Option<String>,
    pub offers: Offers,
    /// Whether the viewer may change their role and remove them.
    pub can_manage: bool,
}

/// What a person's row offers the viewer.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Offers {
    /// Nothing: the owner's role is a fact of the row, not a choice.
    Nothing,
    /// Their role, but not Remove: no one removes themselves here.
    Role,
    RoleAndRemove,
}

/// Everything the People tab shows, resolved to the person's words. The
/// page is rebuilt only when this changes.
#[derive(Clone, Debug, PartialEq)]
pub(super) struct PeopleContent {
    pub count: String,
    /// Where ownership stands, or what removing someone does.
    pub status: String,
    /// The ownership action the viewer can take, by what it does.
    pub ownership: Option<String>,
    pub header: String,
    pub you_tag: String,
    pub remove: String,
    pub people: Vec<Person>,
    /// What an empty list says instead: its title and detail.
    pub empty: (String, String),
    pub note: Option<String>,
}

pub(super) type PeoplePage = super::page::Page<PeopleContent>;

impl PeoplePage {
    /// Lay the page out `width` wide for `content`. Returns its height.
    pub(super) fn lay_out(
        &mut self,
        content: &PeopleContent,
        width: f64,
        theme: &Theme,
        text: &mut TextRenderer,
    ) -> f64 {
        self.lay_out_with(content, width, theme, text, build)
    }
}

/// What the People tab shows of `ws` to `viewer`.
pub(super) fn content(
    ws: &Workspace,
    names: &crate::collaborators::PeopleNames,
    viewer: &UserId,
    is_owner: bool,
) -> PeopleContent {
    let roster = ordered_roster(ws);
    let people = roster
        .iter()
        .map(|(user, role)| {
            let owns = ws.is_owner(user);
            let subject = tilo_action_policy::Subject::Person(user.clone());
            let grants = ws
                .policy
                .grants
                .iter()
                .filter(|grant| grant.subject == subject)
                .count();
            let (offers_role, offers_remove) = super::super::member_row_offers(owns, user, viewer);
            Person {
                user: user.clone(),
                name: member_row_name(names, ws, viewer, user),
                you: user == viewer,
                // An owner by role who does not own the workspace reads as
                // Admin.
                role: if *role == CapRole::Owner && !owns {
                    t("workspace-settings-role-admin")
                } else {
                    role_label(*role)
                },
                extra: (grants > 0).then(|| {
                    t_args(
                        "workspace-settings-member-extra",
                        &[("count", grants.into())],
                    )
                }),
                offers: match (offers_role, offers_remove) {
                    (true, true) => Offers::RoleAndRemove,
                    (true, false) => Offers::Role,
                    (false, _) => Offers::Nothing,
                },
                can_manage: is_owner && !owns && user != viewer,
            }
        })
        .collect();
    PeopleContent {
        count: t_args(
            "workspace-settings-members-count",
            &[("count", roster.len().into())],
        ),
        status: ownership_transfer_status(ws, names, viewer)
            .unwrap_or_else(|| t("workspace-settings-members-help")),
        ownership: super::super::ownership_button(ws, viewer).map(|(label, _)| label),
        header: t("workspace-settings-people-header"),
        you_tag: t("workspace-settings-you"),
        remove: t("workspace-settings-remove"),
        people,
        empty: (
            t("workspace-settings-members-empty-title"),
            t("workspace-settings-members-empty-detail"),
        ),
        note: members_authority_note(is_owner),
    }
}

fn caption(text: &str) -> Element {
    Box::new(Text::new(text).with_role(TextRole::Caption).dimmed())
}

/// One person's row: their role and Remove trail the name where both fit
/// and wrap under it where they do not, so the name is never cut short.
fn person_row(content: &PeopleContent, index: usize, person: &Person) -> Element {
    let mut row = ListRow::new(format!("ws-people-person-{index}"), person.name.clone())
        .two_line_title()
        .avatar(person.name.clone(), person.user.clone());
    if person.you {
        row = row.tags(&[(content.you_tag.as_str(), ChipStyle::Neutral)]);
    }
    if let Some(extra) = &person.extra {
        row = row.secondary(extra.clone());
    }
    if person.offers == Offers::Nothing {
        return Box::new(row.value(person.role.clone()));
    }
    let mut controls = Row::new()
        .with_rhythm(Rhythm::Related)
        .align_start()
        .push(Box::new(
            // The role is a choice, so it reads as one; the lens answers the
            // press with its own role sheet, so the menu holds nothing.
            Button::new(&role_id(index), &person.role)
                .control_size(ControlSize::Small)
                .with_enabled(person.can_manage)
                .with_menu(Vec::new()),
        ));
    if person.offers == Offers::RoleAndRemove {
        controls = controls.push(Box::new(
            Button::new(&remove_id(index), &content.remove)
                .role(ButtonRole::Destructive)
                .control_size(ControlSize::Small)
                .with_enabled(person.can_manage),
        ));
    }
    Box::new(row.trailing(Box::new(controls)))
}

fn build(content: &PeopleContent, _field: f64) -> Column {
    let mut summary = vec![caption(&content.status)];
    if let Some(label) = &content.ownership {
        summary.push(Box::new(
            Row::new()
                .with_rhythm(Rhythm::Related)
                .align_start()
                .push(Box::new(Button::new(OWNERSHIP, label))),
        ));
    }
    let mut groups: Vec<Element> = vec![Box::new(Section::titled(content.count.clone(), summary))];
    if content.people.is_empty() {
        let (title, detail) = &content.empty;
        groups.push(Box::new(
            EmptyState::new(title.clone())
                .with_icon(Icon::Shield)
                .with_message(detail.clone())
                .inline(),
        ));
    } else {
        groups.push(Box::new(Section::titled(
            content.header.clone(),
            content
                .people
                .iter()
                .enumerate()
                .map(|(index, person)| person_row(content, index, person))
                .collect(),
        )));
    }
    if let Some(note) = &content.note {
        groups.push(caption(note));
    }
    groups.into_iter().fold(
        Column::new()
            .with_no_padding()
            .with_intrinsic_height()
            .with_rhythm(Rhythm::Section),
        Column::push,
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use ui::widget::Widget;
    use vello::Scene;
    use vello::kurbo::{Affine, Point, Rect};

    /// Someone looking at four people with long names, offered ownership.
    fn content() -> PeopleContent {
        let person = |index: usize, name: &str| Person {
            user: format!("user-{index}"),
            name: name.to_owned(),
            you: index == 0,
            role: role_label(CapRole::Editor),
            extra: (index == 2)
                .then(|| t_args("workspace-settings-member-extra", &[("count", 2.into())])),
            offers: if index == 0 {
                Offers::Nothing
            } else {
                Offers::RoleAndRemove
            },
            can_manage: index != 0,
        };
        PeopleContent {
            count: t_args("workspace-settings-members-count", &[("count", 4.into())]),
            status: t_args(
                "workspace-settings-ownership-offered",
                &[("owner", "Morgan Ashworth-Reyes".into())],
            ),
            ownership: Some(t("workspace-settings-ownership-accept-phrase")),
            header: t("workspace-settings-people-header"),
            you_tag: t("workspace-settings-you"),
            remove: t("workspace-settings-remove"),
            people: vec![
                person(0, "Xaver Hellauer"),
                person(1, "Morgan Ashworth-Reyes"),
                person(2, "Ada Lovelace-Byron"),
                person(3, "Bo Li Andersson"),
            ],
            empty: (String::new(), String::new()),
            note: None,
        }
    }

    fn drawn(width: f64) -> PeoplePage {
        let theme = Theme::dark();
        let mut text = TextRenderer::new();
        let mut page = PeoplePage::default();
        page.lay_out(&content(), width, &theme, &mut text);
        page.place(Point::ZERO);
        page.draw(&mut Scene::new(), Affine::IDENTITY, &mut text, &theme);
        page
    }

    fn find<'a>(widget: &'a dyn Widget, id: &str) -> Option<&'a dyn Widget> {
        if widget.test_id() == Some(id) {
            return Some(widget);
        }
        widget
            .children()
            .into_iter()
            .find_map(|child| find(child, id))
    }

    #[test]
    fn a_phone_wide_list_shows_every_name_whole_with_its_controls_apart() {
        let width = 390.0;
        let page = drawn(width);
        let root = page.widget().expect("the page is built");
        for (index, person) in content().people.iter().enumerate() {
            let row = find(root, &format!("ws-people-person-{index}")).expect("each person a row");
            let rect = row.get_rect();
            // A row whose title was cut short offers it in full where it
            // is pointed at; none may.
            for (i, j) in (0..96_u32).flat_map(|i| (0..24_u32).map(move |j| (i, j))) {
                let point = Point::new(
                    rect.x0 + rect.width() * f64::from(i) / 96.0,
                    rect.y0 + rect.height() * f64::from(j) / 24.0,
                );
                let tip = row.tooltip_target_at(point);
                assert!(
                    tip.as_ref().is_none_or(|(_, tip)| *tip != person.name),
                    "{:?} is cut short at {width}",
                    person.name
                );
            }
            let role = page.rect(&role_id(index));
            let remove = page.rect(&remove_id(index));
            if person.offers == Offers::Nothing {
                assert_eq!(role, Rect::ZERO);
                continue;
            }
            assert!(role.area() > 0.0 && remove.area() > 0.0);
            assert!(
                role.intersect(remove).area() <= 0.0,
                "role and Remove overlap"
            );
            for control in [role, remove] {
                assert!(
                    control.x0 >= rect.x0 && control.x1 <= rect.x1.min(width),
                    "{control:?} leaves {rect:?}"
                );
                assert!(control.y0 >= rect.y0 && control.y1 <= rect.y1);
            }
        }
    }

    #[test]
    fn a_phone_wide_notice_wraps_in_full_with_its_action_under_it() {
        let narrow = drawn(390.0).rect(OWNERSHIP);
        let wide = drawn(4000.0).rect(OWNERSHIP);
        assert!(narrow.area() > 0.0);
        // The action leads its own line under the notice, which takes more
        // lines where it is narrower.
        assert!((narrow.x0 - wide.x0).abs() < 1.0, "{narrow:?} vs {wide:?}");
        assert!(narrow.y0 > wide.y0, "{narrow:?} vs {wide:?}");
    }
}
