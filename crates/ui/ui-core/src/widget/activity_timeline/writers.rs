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

    /// Where row `i` put each of its parts, as it last painted them: the
    /// author's chip, the change's own mark, the body's room and the time's
    /// slot. `None` before the row has been drawn, or once it is off screen.
    pub fn row_parts(&self, i: usize) -> Option<RowParts> {
        let id = self.row_entry(i)?.id;
        let row = self.row_rect(i)?;
        self.row_parts
            .borrow()
            .get(&id)
            .copied()
            .filter(|parts| parts.row == row)
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

    /// The chip naming what wrote an attributed change: an agent tool, an app
    /// or a program, beside the face of the author it wrote for. An agent's
    /// chip wears the agent mark in its own hue, the mark it shows wherever it
    /// writes; an app's and a program's wear their name alone. Software that is
    /// the author itself has no chip: the row's face is already its own.
    pub(super) fn writer_chip(
        entry: &HistoryEntry,
        text_renderer: &mut TextRenderer,
        theme: &Theme,
    ) -> Option<(String, f64, ChipLead)> {
        let label = crate::history_source::sentence::writer_badge(&entry.author)?;
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
        if self.rect.width() >= f64::from(theme.tokens.metrics.history_writer_name_width)
            || matches!(lead, ChipLead::None)
        {
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
            let label = tilo_ui_base::i18n::widget_format("author-bot-chip", &[]);
            let lead = ChipLead::None;
            let width =
                crate::widget::ChipFace::intrinsic_width(&label, lead, false, text_renderer, theme);
            (label, width, lead)
        })
    }

    /// A band's one author on its second line, from `at` (its left edge, at the line's
    /// centre) up to `end`: their face, their name, and the agent that wrote for them as a
    /// chip, as rows draw theirs. The name and the chip keep a bounded share of the line,
    /// so the facts after them still read and neither reaches the time; a chip whose name
    /// does not fit keeps the writer's mark alone. Answers where the facts start.
    pub(super) fn draw_band_lead(
        ctx: &mut PaintCtx<'_>,
        lead: &HistoryAuthor,
        at: Point,
        end: f64,
    ) -> f64 {
        let (parts, acted_for) = crate::history_source::sentence::lead_parts(lead);
        // The lead names who ran the band; the facts after it keep the rest of
        // the line, and the time its own gutter past that.
        let lead_end = (at.x + (end - at.x).max(0.0) * LEAD_MAX_SHARE).min(end);
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
        // The agent that wrote for the person is part of who did it: its mark keeps its room
        // and the person's name truncates first.
        let writer_mark = parts.writer.as_ref().map_or(0.0, |writer| {
            let lead_mark = author_writer_lead(lead, writer, theme);
            crate::widget::ChipFace::intrinsic_width("", lead_mark, false, tr, theme) + 6.0
        });
        let name = fit_text(
            tr,
            &parts.person,
            &style,
            (lead_end - x - writer_mark).max(0.0),
        );
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
            let named =
                crate::widget::ChipFace::intrinsic_width(writer, lead_mark, false, tr, theme);
            let mark = crate::widget::ChipFace::intrinsic_width("", lead_mark, false, tr, theme);
            let room = (lead_end - x).max(0.0);
            if room >= mark {
                // The agent by its mark and name, or by its mark alone where its name does not
                // fit: the name stays in the hover, never cut to an ellipsis.
                let (label, width) = if named <= room {
                    (writer.as_str(), named)
                } else {
                    ("", mark)
                };
                let height = crate::widget::ChipFace::height(theme);
                let chip = Rect::new(x, at.y - height / 2.0, x + width, at.y + height / 2.0);
                crate::widget::ChipFace::new(chip, label)
                    .lead(lead_mark)
                    .paint(scene, transform, tr, theme);
                x += width + 6.0;
            }
        }
        if let Some(acted_for) = acted_for {
            let note = fit_text(tr, &acted_for, &style, (lead_end - x).max(0.0));
            let (note_w, note_h) = tr.measure_style(&note, None, &style);
            tr.draw_style(
                scene,
                &note,
                theme.tokens.colors.text_dim,
                transform.pre_translate((x, at.y - f64::from(note_h) / 2.0).into()),
                None,
                &style,
            );
            x += f64::from(note_w) + 6.0;
        }
        x.min(end)
    }
}

/// The chip naming what wrote a change, as a row draws it: beside the
/// author's face, inside the bounded share of the row it may take.
pub(super) struct AuthorCluster {
    pub(super) label: String,
    pub(super) lead: ChipLead,
    pub(super) chip: Rect,
    /// Where the change's own mark and sentence start past the cluster.
    pub(super) body_left: f64,
}

/// Where a change row put each of its parts, for a press or a test that
/// reads the row's geometry. Recorded as it paints, so what this says is what
/// the row drew.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct RowParts {
    /// The row's indented rect.
    pub row: Rect,
    /// The author cluster's chip naming what wrote the change, when it holds
    /// one: its mark alone where its name does not fit.
    pub chip: Option<Rect>,
    /// The change's own kind mark, when the row draws one (a change the app
    /// presents itself draws its own body instead).
    pub mark: Option<Rect>,
    /// The room the change's mark and sentence take.
    pub body: Rect,
    /// The trailing time's slot.
    pub time: Rect,
}

impl ActivityTimeline {
    /// The chip a row draws for what wrote the change, and where the change's
    /// own mark starts after it: the author cluster keeps at most
    /// [`AUTHOR_MAX_SHARE`] of the room past the face, so a long writer name
    /// elides in its chip rather than taking the row; one too narrow even for
    /// the writer's mark is dropped. Answers [`None`] when nothing wrote.
    pub(super) fn row_author_cluster(
        &self,
        entry: &HistoryEntry,
        text: &mut TextRenderer,
        theme: &Theme,
        row: Rect,
        cy: f64,
        body_end: f64,
    ) -> Option<AuthorCluster> {
        let (label, width, lead) = Self::bot_chip(entry, text, theme)
            .or_else(|| self.row_writer_chip(entry, text, theme))?;
        let available = (body_end - row.x0).max(0.0);
        let cap = (available * AUTHOR_MAX_SHARE)
            .max(AUTHOR_SLOT_W)
            .min(available.max(AUTHOR_SLOT_W));
        let cluster_end = row.x0 + (AUTHOR_SLOT_W + width + 6.0).min(cap);
        let chip_x = row.x0 + 8.0 + AVATAR_DIAM + 6.0;
        let room = (cluster_end - 6.0 - chip_x).max(0.0);
        let mark = crate::widget::ChipFace::intrinsic_width("", lead, false, text, theme);
        if room < mark {
            return None;
        }
        let width = width.min(room);
        let height = crate::widget::ChipFace::height(theme).min(row.height());
        let chip = Rect::new(chip_x, cy - height / 2.0, chip_x + width, cy + height / 2.0);
        let body_left = (chip.x1 + 6.0).max(row.x0 + AUTHOR_SLOT_W).min(body_end);
        Some(AuthorCluster {
            label,
            lead,
            chip,
            body_left,
        })
    }

    pub(super) fn draw_row_author(ctx: &mut PaintCtx<'_>, cluster: Option<&AuthorCluster>) {
        let PaintCtx {
            scene,
            transform,
            text: text_renderer,
            theme,
        } = ctx.reborrow();
        let Some(cluster) = cluster else {
            return;
        };
        // The chip's rect was capped where the row laid out its columns, so
        // its own name elides inside it and it never reaches the change's
        // mark or the time.
        crate::widget::ChipFace::new(cluster.chip, &cluster.label)
            .lead(cluster.lead)
            .paint(scene, transform, text_renderer, theme);
    }
}

/// The most of a band's second line its one author's name and chip may take: who did it
/// comes before how much moved, so the facts after them truncate first.
const LEAD_MAX_SHARE: f64 = 0.7;
