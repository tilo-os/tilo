//! The words a window shows and where their ink stands: the text its widgets lay out, and the
//! words the window's bar paints itself. A word is read where it stands alone, so two of them
//! drawn in one place are a finding wherever in the window they come from.

use super::{
    EPSILON, Kind, Problem, floats, floats_in_window, has_area, is_text, paints_surface, rect,
    scrolls, size, words, words_of,
};
use ui::headless::InspectionNode;
use ui::kurbo::Rect;
use ui::window_toolbar::BarLabel;

/// How much of a label a finding quotes: enough for a person to know which words they are looking
/// at, without a paragraph in a report line.
const QUOTE: usize = 24;

struct Word {
    text: String,
    at: Rect,
    keys: Vec<String>,
    widget: String,
    id: Option<String>,
    grounds: Vec<(usize, Rect)>,
    order: usize,
}

/// Every word `root` shows and the words the window's bar painted, drawn
/// on the same window: `root` is laid out `top` below the window's top
/// edge, under the bar, so its words are read where the window shows
/// them.
pub fn text_on_text(
    root: &InspectionNode,
    viewport: (u32, u32),
    bar: &[BarLabel],
    top: f64,
) -> Vec<Problem> {
    let window = Rect::new(0.0, 0.0, f64::from(viewport.0), f64::from(viewport.1));
    let mut found = Vec::new();
    let mut walk = Collecting {
        window,
        top,
        grounds: Vec::new(),
        keys: Vec::new(),
        next: 0,
        out: &mut found,
    };
    walk.node(root, window);
    for label in bar {
        found.push(Word {
            text: words(&label.text),
            at: rect(label.rect),
            keys: Vec::new(),
            widget: label.widget.to_owned(),
            id: None,
            grounds: Vec::new(),
            order: 0,
        });
    }
    for (order, word) in found.iter_mut().enumerate() {
        word.order = order;
    }
    // Two words are only read against each other where their ink stands
    // in the same rows, so the words are read top to bottom and each is
    // held against those that follow it while they can still meet it.
    let mut rows: Vec<&Word> = found.iter().collect();
    rows.sort_by(|a, b| a.at.y0.total_cmp(&b.at.y0));
    let mut problems = Vec::new();
    for (position, first) in rows.iter().enumerate() {
        let below = rows
            .iter()
            .skip(position.saturating_add(1))
            .take_while(|next| next.at.y0 < first.at.y1 - EPSILON);
        for second in below {
            // Words stand in the order the window paints them, so a pair is read top last: what a
            // word covers is what already stood under it.
            let (under, over) = if first.order < second.order {
                (first, second)
            } else {
                (second, first)
            };
            let both = under.at.intersect(over.at);
            if !has_area(both) || parted(under, over, both) {
                continue;
            }
            problems.push(Problem {
                kind: Kind::TextOnText,
                keys: over.keys.clone(),
                widget: over.widget.clone(),
                id: over.id.clone(),
                detail: format!(
                    "{} {} stands on {} {}: two words in one place, and neither on a surface of \
                     its own",
                    said(over),
                    size(over.at),
                    said(under),
                    size(under.at),
                ),
            });
        }
    }
    problems
}

/// Whether a ground of one word's own stands between the two: a surface that carries one word and
/// not the other, drawn over the place where their ink meets.
fn parted(a: &Word, b: &Word, meet: Rect) -> bool {
    let carries = |mine: &Word, theirs: &Word| {
        mine.grounds.iter().any(|(id, ground)| {
            !theirs.grounds.iter().any(|(other, _)| other == id) && covers(*ground, meet)
        })
    };
    carries(a, b) || carries(b, a)
}

/// Whether `ground` holds all of `meet`: an edge on a fraction of a pixel still holds it.
fn covers(ground: Rect, meet: Rect) -> bool {
    ground.x0 <= meet.x0 + EPSILON
        && ground.y0 <= meet.y0 + EPSILON
        && ground.x1 >= meet.x1 - EPSILON
        && ground.y1 >= meet.y1 - EPSILON
}

/// Whether the widget `widget` paints a ground of its own over what lies beneath it, so what it
/// shows stands on that ground rather than on the words under it.
fn ground(widget: &str) -> bool {
    paints_surface(widget) || floats_in_window(widget) || widget.contains("Overlay")
}

struct Collecting<'a> {
    window: Rect,
    top: f64,
    grounds: Vec<(usize, Rect)>,
    keys: Vec<String>,
    next: usize,
    out: &'a mut Vec<Word>,
}

impl Collecting<'_> {
    /// Gather the words `node` shows, its own and what it
    /// holds, as much of each as the window can show in
    /// `clip`.
    fn node(&mut self, node: &InspectionNode, clip: Rect) {
        // What floats on the pointer (a row's hover toolbar) is shown for the moment a person is on
        // it, and leaves the geometry it stands for alone.
        if floats(&node.widget) {
            return;
        }
        // A menu, a tooltip or a popover stands against the
        // window it floats in, not the box that opened it.
        let clip = if floats_in_window(&node.widget) {
            self.window
        } else {
            clip
        };
        let own = rect(node.rect);
        let inside = own.intersect(clip);
        if has_area(own) && !has_area(inside) {
            return;
        }
        let pushed = node.test_id.clone().map(|id| self.keys.push(id)).is_some();
        if is_text(node) {
            let shown = Rect::new(
                inside.x0,
                inside.y0 + self.top,
                inside.x1,
                inside.y1 + self.top,
            );
            if has_area(shown) {
                let grounds = self
                    .grounds
                    .iter()
                    .filter(|(_, ground)| covers(*ground, shown))
                    .copied()
                    .collect();
                self.out.push(Word {
                    text: words_of(node),
                    at: shown,
                    keys: self.keys.iter().rev().cloned().collect(),
                    widget: node.widget.clone(),
                    id: node.test_id.clone(),
                    grounds,
                    order: 0,
                });
            }
        } else {
            let mut inner = clip;
            if let Some(child_clip) = node.child_clip {
                inner = clip.intersect(rect(child_clip));
            } else {
                let scrolling = scrolls(node);
                if (scrolling.down || scrolling.across) && has_area(own) {
                    inner = clip.intersect(own);
                }
            }
            // A ground is read where the window shows it, as its words are.
            let here = ground(&node.widget).then(|| {
                self.next = self.next.saturating_add(1);
                let shown = inside;
                (
                    self.next,
                    Rect::new(shown.x0, shown.y0 + self.top, shown.x1, shown.y1 + self.top),
                )
            });
            if let Some(here) = here {
                self.grounds.push(here);
            }
            for child in &node.children {
                self.node(child, inner);
            }
            if here.is_some() {
                self.grounds.pop();
            }
        }
        if pushed {
            self.keys.pop();
        }
    }
}

/// `word` as a finding quotes it: what it says, shortened where it runs long.
fn said(word: &Word) -> String {
    let kept: String = word.text.chars().take(QUOTE).collect();
    let longer = kept.chars().count() < word.text.chars().count();
    match (kept.is_empty(), longer) {
        (true, _) => word.widget.clone(),
        (false, false) => format!("{} \"{kept}\"", word.widget),
        (false, true) => format!("{} \"{kept}…\"", word.widget),
    }
}
