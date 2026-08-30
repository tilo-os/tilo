//! Editing semantics for the math field: caret movement, word motion,
//! selection, delete/backspace (with GeoGebra-style structure unwrapping),
//! structured input building (`/`, `^`, `sqrt`, `pi`→π, …) and clipboard.
//!
//! The caret is a [`Path`] (a real position in the structured model), never a
//! `|` character in the content. A selection is an anchor path + the caret;
//! both must share the same row (selections don't span structure boundaries,
//! matching `GeoGebra`), so a selection is represented as a `(steps, a..b)`
//! range over one row.

use super::model::{MathExpr, Node, Path, Row, Slot, node_slots};

/// A selection over a single row: the row's `steps` and an inclusive-start /
/// exclusive-end index range, plus which end is the moving caret.
#[derive(Clone, Debug, PartialEq)]
pub struct Sel {
    pub steps: Vec<(usize, Slot)>,
    pub anchor: usize,
    pub caret: usize,
}

impl Sel {
    pub fn lo(&self) -> usize {
        self.anchor.min(self.caret)
    }
    pub fn hi(&self) -> usize {
        self.anchor.max(self.caret)
    }
    pub fn is_empty(&self) -> bool {
        self.anchor == self.caret
    }
}

/// The full editor state for one field.
#[derive(Clone, Debug)]
pub struct Editor {
    pub expr: MathExpr,
    pub caret: Path,
    /// When set, the selection anchor (same row as the caret).
    pub anchor: Option<usize>,
}

impl Editor {
    pub fn new(expr: MathExpr) -> Self {
        let off = expr.root.len();
        Editor {
            expr,
            caret: Path::root(off),
            anchor: None,
        }
    }

    pub fn from_linear(s: &str) -> Self {
        Self::new(super::model::parse(s))
    }

    pub fn to_linear(&self) -> String {
        super::model::to_linear(&self.expr)
    }

    /// Current selection as a single-row range, if any non-empty selection.
    pub fn selection(&self) -> Option<Sel> {
        let a = self.anchor?;
        if a == self.caret.offset {
            return None;
        }
        Some(Sel {
            steps: self.caret.steps.clone(),
            anchor: a,
            caret: self.caret.offset,
        })
    }

    fn clear_sel(&mut self) {
        self.anchor = None;
    }

    fn row(&self) -> &Row {
        self.expr
            .row_at(&self.caret.steps)
            .unwrap_or(&self.expr.root)
    }

    fn row_len(&self) -> usize {
        self.row().len()
    }

    // -- caret movement ------------------------------------------------------

    /// Move left one atom. Steps out of / into structures GeoGebra-style.
    pub fn move_left(&mut self, extend: bool) {
        if !extend && let Some(sel) = self.selection() {
            // Collapse to the left edge instead of moving past it.
            self.caret.offset = sel.lo();
            self.anchor = None;
            return;
        }
        self.pre_move(extend);
        if self.caret.offset > 0 {
            // Step into the structure to the left if it has an entry slot.
            let idx = self.caret.offset - 1;
            if let Some(node) = self.row().get(idx).cloned()
                && !extend
                && let Some(slot) = enter_from_right(&node)
            {
                self.caret.steps.push((idx, slot));
                let inner = self.row_len();
                self.caret.offset = inner;
                return;
            }
            self.caret.offset -= 1;
        } else if !self.caret.steps.is_empty() && !extend {
            // At row start: pop out to the left of the owning node.
            let (idx, _slot) = self.caret.steps.pop().unwrap();
            self.caret.offset = idx;
        }
    }

    /// Move right one atom.
    pub fn move_right(&mut self, extend: bool) {
        if !extend && let Some(sel) = self.selection() {
            self.caret.offset = sel.hi();
            self.anchor = None;
            return;
        }
        self.pre_move(extend);
        let len = self.row_len();
        if self.caret.offset < len {
            let idx = self.caret.offset;
            if let Some(node) = self.row().get(idx).cloned()
                && !extend
                && let Some(slot) = enter_from_left(&node)
            {
                self.caret.steps.push((idx, slot));
                self.caret.offset = 0;
                return;
            }
            self.caret.offset += 1;
        } else if !self.caret.steps.is_empty() && !extend {
            // At row end: pop out to the right of the owning node.
            let (idx, _slot) = self.caret.steps.pop().unwrap();
            self.caret.offset = idx + 1;
        }
    }

    /// Up: move to a higher sibling box (denominator→numerator, into exponent).
    pub fn move_up(&mut self) {
        self.clear_sel();
        self.vertical(true);
    }

    /// Down: move to a lower sibling box (numerator→denominator).
    pub fn move_down(&mut self) {
        self.clear_sel();
        self.vertical(false);
    }

    fn vertical(&mut self, up: bool) {
        // If the caret is inside a structure, try moving between that
        // structure's vertically-stacked slots.
        if let Some(&(idx, slot)) = self.caret.steps.last() {
            let parent_steps = &self.caret.steps[..self.caret.steps.len() - 1];
            if let Some(parent_row) = self.expr.row_at(parent_steps)
                && let Some(node) = parent_row.get(idx)
            {
                let slots = node_slots(node);
                if let Some(pos) = slots.iter().position(|s| *s == slot) {
                    let target = if up {
                        pos.checked_sub(1)
                    } else if pos + 1 < slots.len() {
                        Some(pos + 1)
                    } else {
                        None
                    };
                    if let Some(tp) = target {
                        let new_slot = slots[tp];
                        let mut steps = parent_steps.to_vec();
                        steps.push((idx, new_slot));
                        let new_len = self.expr.row_at(&steps).map_or(0, std::vec::Vec::len);
                        self.caret.steps = steps;
                        self.caret.offset = self.caret.offset.min(new_len);
                        return;
                    }
                }
            }
            // Can't move within this structure: step out toward the target dir.
            if let Some(&(pidx, _)) = self.caret.steps.last() {
                self.caret.steps.pop();
                self.caret.offset = if up { pidx } else { pidx + 1 };
            }
        }
    }

    pub fn home(&mut self, extend: bool) {
        self.pre_move(extend);
        self.caret.offset = 0;
    }

    pub fn end(&mut self, extend: bool) {
        self.pre_move(extend);
        self.caret.offset = self.row_len();
    }

    /// Ctrl+Left: jump to the start of the word/number/identifier to the left.
    pub fn move_word_left(&mut self, extend: bool) {
        self.pre_move(extend);
        let row = self.row().clone();
        let mut i = self.caret.offset;
        // skip immediate non-word atoms
        while i > 0 && !is_word_node(&row[i - 1]) {
            i -= 1;
            if i == 0 {
                break;
            }
            // stop after consuming a single structure / operator
            if !is_word_node(&row[i]) {
                self.caret.offset = i;
                return;
            }
        }
        while i > 0 && is_word_node(&row[i - 1]) {
            i -= 1;
        }
        self.caret.offset = i;
    }

    /// Ctrl+Right: jump to the end of the word/number to the right.
    pub fn move_word_right(&mut self, extend: bool) {
        self.pre_move(extend);
        let row = self.row().clone();
        let len = row.len();
        let mut i = self.caret.offset;
        // Stop after consuming a single structure / operator, mirroring
        // move_word_left. This was a `while` that returned on its first
        // iteration, so it only ever ran once anyway.
        if i < len && !is_word_node(&row[i]) {
            i += 1;
            self.caret.offset = i;
            return;
        }
        while i < len && is_word_node(&row[i]) {
            i += 1;
        }
        self.caret.offset = i;
    }

    pub fn select_all(&mut self) {
        // Select the whole current row.
        self.anchor = Some(0);
        self.caret.offset = self.row_len();
    }

    /// Select the token (number / identifier run / single structure) under
    /// `index` — used for double-click.
    pub fn select_token_at(&mut self, steps: Vec<(usize, Slot)>, index: usize) {
        self.caret.steps = steps;
        let row = self.row().clone();
        if row.is_empty() {
            self.anchor = None;
            self.caret.offset = 0;
            return;
        }
        let probe = index.min(row.len().saturating_sub(1));
        if is_word_node(&row[probe]) {
            let mut lo = probe;
            while lo > 0 && is_word_node(&row[lo - 1]) {
                lo -= 1;
            }
            let mut hi = probe + 1;
            while hi < row.len() && is_word_node(&row[hi]) {
                hi += 1;
            }
            self.anchor = Some(lo);
            self.caret.offset = hi;
        } else {
            self.anchor = Some(probe);
            self.caret.offset = probe + 1;
        }
    }

    /// Common pre-move bookkeeping for the shift/non-shift cases. With
    /// `extend`, start an anchor at the current caret if none exists; without
    /// it, drop any anchor (the left/right callers handle selection collapse
    /// to the correct edge before reaching here).
    fn pre_move(&mut self, extend: bool) {
        if extend {
            if self.anchor.is_none() {
                self.anchor = Some(self.caret.offset);
            }
        } else {
            self.anchor = None;
        }
    }

    // -- deletion ------------------------------------------------------------

    /// Backspace: delete selection, else the atom before the caret. At the
    /// front of a structure's box, unwrap that structure (`GeoGebra` style).
    pub fn backspace(&mut self) {
        if self.delete_selection() {
            return;
        }
        if self.caret.offset > 0 {
            let idx = self.caret.offset - 1;
            if let Some(row) = self.expr.row_at_mut(&self.caret.steps) {
                row.remove(idx);
            }
            self.caret.offset = idx;
        } else if !self.caret.steps.is_empty() {
            // At the front of a child box: unwrap the structure, splicing the
            // box contents into the parent row in place of the node.
            self.unwrap_parent();
        }
    }

    /// Delete (forward): delete selection, else the atom after the caret.
    /// At the end of a child box, also unwrap the parent structure forward.
    pub fn delete(&mut self) {
        if self.delete_selection() {
            return;
        }
        let len = self.row_len();
        if self.caret.offset < len {
            if let Some(row) = self.expr.row_at_mut(&self.caret.steps) {
                row.remove(self.caret.offset);
            }
        } else if !self.caret.steps.is_empty() {
            self.unwrap_parent();
        }
    }

    /// Replace the parent structure with the concatenation of all its child
    /// rows, placing the caret where the current box used to start.
    fn unwrap_parent(&mut self) {
        let (idx, _slot) = match self.caret.steps.last().copied() {
            Some(x) => x,
            None => return,
        };
        let parent_steps = self.caret.steps[..self.caret.steps.len() - 1].to_vec();
        let node = match self.expr.row_at(&parent_steps).and_then(|r| r.get(idx)) {
            Some(n) => n.clone(),
            None => return,
        };
        let flat = flatten_node(&node);
        if let Some(parent) = self.expr.row_at_mut(&parent_steps) {
            parent.remove(idx);
            let n = flat.len();
            for (k, atom) in flat.into_iter().enumerate() {
                parent.insert(idx + k, atom);
            }
            self.caret.steps = parent_steps;
            self.caret.offset = idx + n;
        }
    }

    /// Delete the active selection if any; returns whether it did.
    pub fn delete_selection(&mut self) -> bool {
        let sel = match self.selection() {
            Some(s) => s,
            None => return false,
        };
        let (lo, hi) = (sel.lo(), sel.hi());
        if let Some(row) = self.expr.row_at_mut(&sel.steps) {
            row.drain(lo..hi);
        }
        self.caret.offset = lo;
        self.anchor = None;
        true
    }

    // -- insertion -----------------------------------------------------------

    /// Insert a node at the caret, replacing any selection first.
    fn insert_node(&mut self, node: Node) {
        self.delete_selection();
        let at = self.caret.offset;
        if let Some(row) = self.expr.row_at_mut(&self.caret.steps) {
            row.insert(at, node);
        }
        self.caret.offset = at + 1;
    }

    /// Insert a literal character (digit, letter, `.`, operator).
    pub fn insert_char(&mut self, ch: char) {
        self.insert_node(Node::Char(ch));
    }

    /// Type one logical character with GeoGebra-style structured building.
    pub fn type_char(&mut self, ch: char) {
        match ch {
            '/' => self.start_fraction(),
            '^' => self.start_power(),
            '*' => self.insert_char('·'),
            '·' => self.insert_char('·'),
            '(' => self.start_paren(),
            ')' => self.skip_or_insert_close(),
            '√' => self.insert_sqrt(),
            'π' => self.insert_char('π'),
            _ => {
                self.delete_selection();
                self.insert_char(ch);
                self.maybe_promote_function();
            }
        }
    }

    /// After typing letters, if the run immediately before the caret spells a
    /// known function name AND the user just typed `(` would be next — we
    /// don't auto-promote on each keystroke; promotion happens when `(` is
    /// typed via [`start_paren`]. This hook is reserved for `pi`→π and `sqrt`.
    fn maybe_promote_function(&mut self) {
        // pi -> π
        if self.run_before_is("pi") {
            self.replace_run_before(2, vec![Node::Char('π')]);
        }
    }

    /// `(`: if the identifier run just before the caret is a known function
    /// (or `sqrt`), build the structured call; otherwise insert a paren pair
    /// and place the caret inside.
    fn start_paren(&mut self) {
        self.delete_selection();
        let run = self.ident_run_before();
        if run == "sqrt" {
            self.replace_run_before(
                4,
                vec![Node::Sqrt {
                    index: None,
                    radicand: Vec::new(),
                }],
            );
            self.descend_into_last(Slot::Radicand);
            return;
        }
        if super::model::FUNCS.contains(&run.as_str()) && !run.is_empty() {
            let n = run.chars().count();
            self.replace_run_before(
                n,
                vec![Node::Func {
                    name: run,
                    arg: Vec::new(),
                }],
            );
            self.descend_into_last(Slot::Arg);
            return;
        }
        // plain group
        self.insert_node(Node::Paren { body: Vec::new() });
        self.descend_into_last(Slot::Body);
    }

    /// `)`: step out of the current group if we're directly inside one;
    /// otherwise (defensively) insert a literal — but normal flow keeps
    /// groups balanced so this just exits.
    fn skip_or_insert_close(&mut self) {
        self.delete_selection();
        if let Some(&(idx, slot)) = self.caret.steps.last()
            && matches!(slot, Slot::Body | Slot::Arg | Slot::Radicand)
        {
            self.caret.steps.pop();
            self.caret.offset = idx + 1;
        }
        // Not inside a group: ignore stray close to keep the tree balanced.
    }

    /// Build an empty radical at the caret and drop into its radicand — the
    /// structured equivalent of typing `sqrt(`. This backs the `√` glyph as
    /// entered input (routed here by [`type_char`]), so a `√` from the keyboard
    /// / an IME / a paste builds a real radical rather than a literal character
    /// the engine can't parse.
    pub fn insert_sqrt(&mut self) {
        self.insert_node(Node::Sqrt {
            index: None,
            radicand: Vec::new(),
        });
        self.descend_into_last(Slot::Radicand);
    }

    /// `/`: wrap the operand immediately left of the caret as the numerator
    /// and drop the caret into an empty denominator.
    fn start_fraction(&mut self) {
        self.delete_selection();
        let num = self.take_left_operand();
        self.insert_node(Node::Frac {
            num,
            den: Vec::new(),
        });
        self.descend_into_last(Slot::Den);
    }

    /// `^`: open a superscript box and move the caret inside.
    fn start_power(&mut self) {
        self.delete_selection();
        self.insert_node(Node::Sup { exp: Vec::new() });
        self.descend_into_last(Slot::Exp);
    }

    /// Move the caret into the slot of the node just inserted (now at
    /// `offset-1`).
    fn descend_into_last(&mut self, slot: Slot) {
        let idx = self.caret.offset - 1;
        self.caret.steps.push((idx, slot));
        self.caret.offset = 0;
    }

    /// Pull the operand directly left of the caret out of the current row to
    /// serve as a fraction numerator: a trailing number, identifier run,
    /// or a single structure / parenthesized group.
    fn take_left_operand(&mut self) -> Row {
        let row = match self.expr.row_at_mut(&self.caret.steps) {
            Some(r) => r,
            None => return Vec::new(),
        };
        let end = self.caret.offset;
        if end == 0 {
            return Vec::new();
        }
        let prev = &row[end - 1];
        // A single structure / group / func is the whole operand.
        if !matches!(prev, Node::Char(_)) {
            let node = row.remove(end - 1);
            self.caret.offset = end - 1;
            return match node {
                Node::Paren { body } => body,
                Node::Delimited { body, .. } => body,
                other => vec![other],
            };
        }
        // Otherwise pull a run of number/identifier chars (not operators).
        let mut start = end;
        while start > 0 {
            match &row[start - 1] {
                Node::Char(c) if c.is_ascii_alphanumeric() || *c == '.' || *c == 'π' => {
                    start -= 1;
                }
                _ => break,
            }
        }
        let taken: Row = row.drain(start..end).collect();
        self.caret.offset = start;
        taken
    }

    // -- helpers over the char run before the caret --------------------------

    fn ident_run_before(&self) -> String {
        let row = self.row();
        let mut s = String::new();
        let mut i = self.caret.offset;
        while i > 0 {
            if let Node::Char(c) = &row[i - 1]
                && c.is_ascii_alphabetic()
            {
                s.insert(0, *c);
                i -= 1;
                continue;
            }
            break;
        }
        s
    }

    fn run_before_is(&self, want: &str) -> bool {
        self.ident_run_before().ends_with(want) && {
            // ensure exactly the run (not part of a longer ident)
            let row = self.row();
            let n = want.chars().count();
            let off = self.caret.offset;
            if off < n {
                return false;
            }
            // char before the run must not be a letter
            if off > n
                && let Node::Char(c) = &row[off - n - 1]
                && c.is_ascii_alphabetic()
            {
                return false;
            }
            true
        }
    }

    fn replace_run_before(&mut self, count: usize, with: Vec<Node>) {
        let off = self.caret.offset;
        let start = off - count;
        if let Some(row) = self.expr.row_at_mut(&self.caret.steps) {
            row.drain(start..off);
            let n = with.len();
            for (k, node) in with.into_iter().enumerate() {
                row.insert(start + k, node);
            }
            self.caret.offset = start + n;
        }
    }

    // -- clipboard -----------------------------------------------------------

    /// The selected sub-row serialized to a linear string (for copy/cut).
    pub fn selected_linear(&self) -> Option<String> {
        let sel = self.selection()?;
        let row = self.expr.row_at(&sel.steps)?;
        let slice: Row = row[sel.lo()..sel.hi()].to_vec();
        Some(super::model::to_linear(&MathExpr { root: slice }))
    }

    /// Paste a linear string at the caret (replacing any selection), parsed
    /// back into structured nodes.
    pub fn paste_linear(&mut self, s: &str) {
        self.delete_selection();
        let parsed = super::model::parse(s);
        let at = self.caret.offset;
        let n = parsed.root.len();
        if let Some(row) = self.expr.row_at_mut(&self.caret.steps) {
            for (k, node) in parsed.root.into_iter().enumerate() {
                row.insert(at + k, node);
            }
        }
        self.caret.offset = at + n;
    }

    /// Move the caret inside the first structure found scanning the root row
    /// left→right: into a fraction's numerator (after its first atom), a
    /// power's exponent, or a radical's radicand. No-op if the root row has no
    /// structures. Used to seed a representative static frame.
    pub fn park_in_first_structure(&mut self) {
        for (i, node) in self.expr.root.iter().enumerate() {
            let slot = match node {
                Node::Frac { .. } => Some(Slot::Num),
                Node::Sup { .. } => Some(Slot::Exp),
                Node::Sqrt { index: None, .. } => Some(Slot::Radicand),
                Node::Sqrt { index: Some(_), .. } => Some(Slot::Radicand),
                Node::Func { .. } => Some(Slot::Arg),
                _ => None,
            };
            if let Some(slot) = slot {
                self.caret.steps = vec![(i, slot)];
                let len = self.row_len();
                // Sit one atom in (after the first), so the caret is clearly
                // *within* the structure, not just at its leading edge.
                self.caret.offset = len.min(1);
                self.anchor = None;
                return;
            }
        }
    }

    /// Seed a visible selection: select the contents of the first structure's
    /// first box (e.g. the numerator `x²+1`), else select the whole root row.
    pub fn park_demo_selection(&mut self) {
        for (i, node) in self.expr.root.iter().enumerate() {
            let slot = match node {
                Node::Frac { .. } => Some(Slot::Num),
                Node::Sqrt { index: None, .. } => Some(Slot::Radicand),
                Node::Func { .. } => Some(Slot::Arg),
                _ => None,
            };
            if let Some(slot) = slot {
                self.caret.steps = vec![(i, slot)];
                let len = self.row_len();
                if len > 0 {
                    self.anchor = Some(0);
                    self.caret.offset = len;
                    return;
                }
            }
        }
        // No structure: select the whole root row.
        self.caret.steps.clear();
        if !self.expr.root.is_empty() {
            self.anchor = Some(0);
            self.caret.offset = self.expr.root.len();
        }
    }
}

/// Whether a node counts as part of a "word" for word-wise motion / token
/// selection: number/identifier characters and whole structures, not bare
/// operators.
fn is_word_node(node: &Node) -> bool {
    match node {
        Node::Char(c) => c.is_ascii_alphanumeric() || *c == '.' || *c == 'π',
        _ => true,
    }
}

/// The slot the caret enters when stepping *into* a structure from its left.
fn enter_from_left(node: &Node) -> Option<Slot> {
    match node {
        Node::Frac { .. } => Some(Slot::Num),
        Node::Sup { .. } => Some(Slot::Exp),
        Node::Sub { .. } => Some(Slot::Sub),
        Node::SubSup { .. } => Some(Slot::Sup),
        Node::Sqrt { index: Some(_), .. } => Some(Slot::Index),
        Node::Sqrt { index: None, .. } => Some(Slot::Radicand),
        Node::Paren { .. } => Some(Slot::Body),
        Node::Delimited { .. } => Some(Slot::Body),
        Node::Func { .. } => Some(Slot::Arg),
        Node::Accent { .. } => Some(Slot::Base),
        Node::UnderOver { .. } => Some(Slot::Base),
        Node::Table { rows } if rows.first().is_some_and(|row| !row.is_empty()) => {
            Some(Slot::Cell(0, 0))
        }
        Node::Table { .. } => None,
        Node::Char(_) | Node::Text(_) => None,
    }
}

/// The slot the caret enters when stepping *into* a structure from its right.
fn enter_from_right(node: &Node) -> Option<Slot> {
    match node {
        Node::Frac { .. } => Some(Slot::Den),
        Node::Sup { .. } => Some(Slot::Exp),
        Node::Sub { .. } => Some(Slot::Sub),
        Node::SubSup { .. } => Some(Slot::Sub),
        Node::Sqrt { .. } => Some(Slot::Radicand),
        Node::Paren { .. } => Some(Slot::Body),
        Node::Delimited { .. } => Some(Slot::Body),
        Node::Func { .. } => Some(Slot::Arg),
        Node::Accent { .. } => Some(Slot::Base),
        Node::UnderOver { .. } => Some(Slot::Base),
        Node::Table { rows } => rows.iter().enumerate().rev().find_map(|(row, cells)| {
            let column = cells.len().checked_sub(1)?;
            Some(Slot::Cell(
                u16::try_from(row).ok()?,
                u16::try_from(column).ok()?,
            ))
        }),
        Node::Char(_) | Node::Text(_) => None,
    }
}

/// Concatenate a structure's child rows when unwrapping it.
fn flatten_node(node: &Node) -> Row {
    match node {
        Node::Char(c) => vec![Node::Char(*c)],
        Node::Frac { num, den } => {
            let mut r = num.clone();
            r.extend(den.clone());
            r
        }
        Node::Sup { exp } => exp.clone(),
        Node::Sub { sub } => sub.clone(),
        Node::SubSup { sub, sup } => {
            let mut row = sub.clone();
            row.extend(sup.clone());
            row
        }
        Node::Sqrt { index, radicand } => {
            let mut r = index.clone().unwrap_or_default();
            r.extend(radicand.clone());
            r
        }
        Node::Paren { body } => body.clone(),
        Node::Delimited { body, .. } => body.clone(),
        Node::Func { arg, .. } => arg.clone(),
        Node::Accent { base, .. } => base.clone(),
        Node::Text(_) => vec![node.clone()],
        Node::UnderOver { base, under, over } => {
            let mut row = base.clone();
            row.extend(under.clone().unwrap_or_default());
            row.extend(over.clone().unwrap_or_default());
            row
        }
        Node::Table { rows } => rows
            .iter()
            .flat_map(|cells| cells.iter().flat_map(|cell| cell.clone()))
            .collect(),
    }
}
