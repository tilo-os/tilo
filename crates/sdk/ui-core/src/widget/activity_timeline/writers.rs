//! What a History pane shows of the software that wrote for people (agents, apps, programs)
//! and of the one thing a change acted on: the writer chips, the writer filter, a band's lead
//! author and a row's target link.
use super::*;

impl ActivityTimeline {
    /// Open what row `i`'s change acted on ([`HistoryEntry::target`]), as its link does.
    pub(super) fn open_target(&mut self, i: usize) -> bool {
        let Some(id) = self
            .row_entry(i)
            .filter(|entry| entry.target.is_some() && entry.redaction.is_none())
            .map(|entry| entry.id)
        else {
            return false;
        };
        let Some(open) = self.on_open_entry.as_mut() else {
            return false;
        };
        open(&id);
        true
    }

    /// Where row `i`'s target link stands, when the row shows one.
    pub fn target_link_rect(&self, i: usize) -> Option<Rect> {
        let id = self.row_entry(i)?.id;
        let link = *self.target_links.borrow().get(&id)?;
        let row = self.row_rect(i)?;
        row.contains(link.center()).then_some(link)
    }

    /// Show only what the agent (or app, or program) named `writer` wrote for anyone.
    pub fn filter_by_writer(&mut self, writer: &str) {
        let writers = vec![writer.to_owned()];
        if self.filter.writers != writers || !self.filter.authors.is_empty() {
            self.filter.writers = writers;
            self.filter.authors.clear();
            self.rebuild();
            self.dirty = true;
        }
    }

    /// The agents, apps and programs that wrote for the people in this history, each once
    /// by the name their badge shows, in the order their latest change is listed.
    pub fn writers(&self) -> Vec<String> {
        let mut order: Vec<usize> = (0..self.entries.len()).collect();
        if self.newest_first {
            order.compact_sort_by_key(|&i| {
                std::cmp::Reverse(self.entries.get(i).map(|e| e.timestamp))
            });
        }
        let mut writers: Vec<String> = Vec::new();
        for entry in order.into_iter().filter_map(|i| self.entries.get(i)) {
            if let Some(writer) = crate::history_source::sentence::writer_badge(&entry.author)
                && !writers.contains(&writer)
            {
                writers.push(writer);
            }
        }
        writers
    }

    /// Where the strip's chip for the agent (or app, or program) named `writer` is.
    pub fn writer_chip_rect(&self, writer: &str) -> Option<Rect> {
        self.strip_chips.iter().find_map(|chip| match &chip.target {
            StripTarget::Writer(placed) if placed == writer => Some(chip.hit),
            _ => None,
        })
    }

    /// The chip naming what wrote an attributed change: an agent tool, an app
    /// or a program, beside the person whose avatar the row wears. An agent's
    /// chip wears the agent mark in its own hue, the mark it shows wherever it
    /// writes; an app's and a program's wear their name alone.
    pub(super) fn writer_chip(
        entry: &HistoryEntry,
        text_renderer: &mut TextRenderer,
        theme: &Theme,
    ) -> Option<(String, f64, ChipLead)> {
        // An agent's own device writes under the agent's name; an attributed
        // session names its tool. Either way the chip is the software's own.
        let label = crate::history_source::sentence::writer_badge(&entry.author).or_else(|| {
            (entry.author.relation == crate::widget::history::AuthorRelation::Software)
                .then(|| entry.author.name.clone())
        })?;
        let lead = writer_lead(entry, &label, theme);
        let width =
            crate::widget::ChipFace::intrinsic_width(&label, lead, false, text_renderer, theme);
        (width > 0.0).then_some((label, width, lead))
    }

    /// [`Self::writer_chip`] as a row of this pane draws it: in a pane too narrow for the
    /// chip's name beside what the change did, the chip is the writer's mark alone, its name
    /// still in the row's spoken description and hover.
    pub(super) fn row_writer_chip(
        &self,
        entry: &HistoryEntry,
        text_renderer: &mut TextRenderer,
        theme: &Theme,
    ) -> Option<(String, f64, ChipLead)> {
        let (label, width, lead) = Self::writer_chip(entry, text_renderer, theme)?;
        if self.rect.width() >= COMPACT_WRITER_WIDTH || matches!(lead, ChipLead::None) {
            return Some((label, width, lead));
        }
        let width = crate::widget::ChipFace::intrinsic_width("", lead, false, text_renderer, theme);
        Some((String::new(), width, lead))
    }

    pub(super) fn bot_chip(
        entry: &HistoryEntry,
        text_renderer: &mut TextRenderer,
        theme: &Theme,
    ) -> Option<(String, f64, ChipLead)> {
        is_bot_author(&entry.author).then(|| {
            let label = crate::i18n::widget_format("author-bot-chip", &[]);
            let lead = ChipLead::None;
            let width =
                crate::widget::ChipFace::intrinsic_width(&label, lead, false, text_renderer, theme);
            (label, width, lead)
        })
    }

    /// A band's one author on its second line, from `at` (its left edge, at the line's
    /// centre) up to `end`: their face, their name, and the agent that wrote for them as a
    /// chip, as rows draw theirs. Answers where the line's facts start after it.
    pub(super) fn draw_band_lead(
        ctx: &mut PaintCtx<'_>,
        lead: &HistoryAuthor,
        at: Point,
        end: f64,
    ) -> f64 {
        let parts = crate::history_source::sentence::author_parts(lead);
        let face_d = AVATAR_DIAM;
        let (r, g, b) = lead.rgb();
        let face = Rect::new(
            at.x,
            at.y - face_d / 2.0,
            at.x + face_d,
            at.y + face_d / 2.0,
        );
        author_face(lead, Color::from_rgb8(r, g, b)).paint(ctx, face);
        let mut x = face.x1 + 6.0;
        let PaintCtx {
            scene,
            transform,
            text: tr,
            theme,
        } = ctx.reborrow();
        let style = theme.text(TextRole::Caption);
        let name = fit_text(tr, &parts.person, &style, (end - x).max(0.0));
        let (name_w, name_h) = tr.measure_style(&name, None, &style);
        tr.draw_style(
            scene,
            &name,
            theme.tokens.colors.text_dim,
            transform.pre_translate((x, at.y - f64::from(name_h) / 2.0).into()),
            None,
            &style,
        );
        x += f64::from(name_w) + 6.0;
        if let Some(writer) = &parts.writer {
            let lead_mark = author_writer_lead(lead, writer, theme);
            let width =
                crate::widget::ChipFace::intrinsic_width(writer, lead_mark, false, tr, theme);
            if x + width <= end {
                let height = crate::widget::ChipFace::height(theme);
                let chip = Rect::new(x, at.y - height / 2.0, x + width, at.y + height / 2.0);
                crate::widget::ChipFace::new(chip, writer)
                    .lead(lead_mark)
                    .paint(scene, transform, tr, theme);
                x += width + 6.0;
            }
        }
        x
    }
}
