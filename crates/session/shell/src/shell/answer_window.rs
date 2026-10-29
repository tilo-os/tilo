//! Where an agent's answer opens: a view answer is a resource (the Views app keeps it), opened
//! as a window beside what the person asked about (agent-ux.md, "Answers as views"; canvas.md,
//! "Answers"). This is the one interface a surface implements to place it. The strip places by
//! window, so it opens the answer beside the window the anchor lies in; a 2D canvas places by
//! position and takes free space near the anchor itself. Either way the answer never covers its
//! subject and nothing already there moves.

use vello::kurbo::Rect;

use crate::shell::state::{GroupId, WorkspaceTree};
use crate::shell::tab_binding::Placement;

/// An answer to open, and what it is placed by.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct AnswerPlacement {
    /// The answer: the resource that keeps it, in its app.
    pub resource: tilo_reference::ResourceAt,
    /// Where its subject is drawn on the desktop, when that is known: the selection the
    /// person asked about, else what the composer kept clear of.
    pub anchor: Option<Rect>,
    /// The app the subject is in, when the anchor is not known or lies in no window.
    pub beside_app: Option<String>,
}

/// Where the strip opens `answer`: beside the window its anchor lies in (`windows`: each window
/// group on screen and where it is drawn), else beside a window of the subject's app, else
/// beside the window the person is in. `None` when there is no window to open it beside.
pub(crate) fn strip_placement(
    answer: &AnswerPlacement,
    windows: &[(GroupId, Rect)],
    tree: &WorkspaceTree,
) -> Option<Placement> {
    let anchored = answer.anchor.and_then(|anchor| {
        let centre = anchor.center();
        windows
            .iter()
            .find(|(_, rect)| rect.contains(centre))
            .or_else(|| {
                // The subject straddles windows: the one holding most of it.
                windows
                    .iter()
                    .map(|window| (window, window.1.intersect(anchor).area()))
                    .filter(|(_, area)| *area > 0.0)
                    .max_by(|a, b| a.1.total_cmp(&b.1))
                    .map(|(window, _)| window)
            })
            .map(|(group, _)| *group)
    });
    if let Some(group) = anchored {
        return Some(Placement::Beside(group));
    }
    let beside_app = answer.beside_app.as_deref().and_then(|app| {
        crate::shell::placement::group_showing(tree.desktop.active_workspace()?, app)
    });
    beside_app
        .map(Placement::Beside)
        .or_else(|| crate::shell::placement::aside(tree))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn answer(anchor: Option<Rect>) -> AnswerPlacement {
        AnswerPlacement {
            resource: tilo_reference::ResourceAt {
                app: "so.tilo.lang.views".into(),
                record: None,
                reference: tilo_reference::ResourceRef::whole(
                    "tl.so.tilo.lang.views.Answer",
                    "tl:so.tilo.lang.views:a1",
                    "What is left for the release",
                ),
            },
            anchor,
            beside_app: None,
        }
    }

    #[test]
    fn an_answer_opens_beside_the_window_its_subject_is_in() {
        let tree = WorkspaceTree::default();
        let (left, right): (GroupId, GroupId) = (101, 102);
        let windows = [
            (left, Rect::new(0.0, 0.0, 600.0, 800.0)),
            (right, Rect::new(610.0, 0.0, 1280.0, 800.0)),
        ];
        assert_eq!(
            strip_placement(
                &answer(Some(Rect::new(700.0, 300.0, 900.0, 340.0))),
                &windows,
                &tree
            ),
            Some(Placement::Beside(right))
        );
        assert_eq!(
            strip_placement(
                &answer(Some(Rect::new(500.0, 300.0, 640.0, 340.0))),
                &windows,
                &tree
            ),
            Some(Placement::Beside(left)),
            "the window holding most of a subject that straddles two"
        );
        assert_eq!(
            strip_placement(&answer(None), &windows, &tree),
            None,
            "no subject and no window the person is in: nothing to open beside"
        );
    }
}
