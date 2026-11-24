//! How a block editor answers for the size of its block stack: measured at the width a layout
//! asks for, at a width nothing fits in and without a width at all, and stacked again at the
//! width a tree solved it at before the editor places its blocks there.

use taffy::Size;

use super::BlockEditor;
use ui::text_render::TextRenderer;

impl BlockEditor {
    /// The width a line assumes where no definite one is given: the theme's reading measure.
    fn assumed_line_width(&self) -> f32 {
        self.layout_theme.tokens.metrics.reading_measure
    }

    /// Measure the block stack. A query without a definite width asks for an
    /// intrinsic size: answer it from the last stack rather than reshaping
    /// every paragraph at a width it will never be painted at.
    pub(super) fn measure_stack(
        &mut self,
        known: Size<Option<f32>>,
        available: Size<taffy::AvailableSpace>,
        text_renderer: &mut TextRenderer,
    ) -> Size<f32> {
        self.settle_restack();
        let definite = known.width.or(match available.width {
            taffy::AvailableSpace::Definite(width) => Some(width),
            _ => None,
        });
        let width = match (definite, self.stack.width) {
            (Some(width), _) if width > 0.0 => width,
            // A width nothing fits in is a min-content contribution, not a
            // layout: answer it from the stack already measured, if there is
            // one, or at the width a line assumes, but leave the blocks
            // stacked at the width they are laid out at, or the host's apply
            // without a text renderer has no heights left to paint them with.
            (Some(width), _) => {
                return if self.stack.tops.len() == self.blocks.len() {
                    Size {
                        width: if self.stack.measured { width } else { 0.0 },
                        height: self.stack.total,
                    }
                } else {
                    self.probe_stack(width, text_renderer)
                };
            }
            (None, Some(_)) if self.stack.tops.len() == self.blocks.len() => {
                return Size {
                    width: if self.stack.measured {
                        self.assumed_line_width()
                    } else {
                        0.0
                    },
                    height: self.stack.total,
                };
            }
            (None, _) => self.assumed_line_width(),
        };
        self.stack_blocks(width, Some(text_renderer));
        Size {
            width: if self.stack.measured { width } else { 0.0 },
            height: self.stack.total,
        }
    }

    /// Stack the blocks at `width`, the width a tree solved the stack at, measuring each line
    /// there that holds no height for it: what the editor's apply then places, rather than a
    /// restack without a renderer from heights measured at other widths.
    pub(super) fn stack_at(&mut self, width: f32, text_renderer: &mut TextRenderer) {
        self.settle_restack();
        if width <= 0.0
            || (self.stack.tops.len() == self.blocks.len()
                && self.stack.width.map(f32::to_bits) == Some(width.to_bits()))
        {
            return;
        }
        // Every line that holds its height at this width keeps it, as the apply would; a line
        // that does not is measured here, where a renderer is at hand.
        let measured = self.blocks.iter().all(|block| {
            block
                .line
                .stacked_height(width, block.line.row_bands())
                .is_some()
        });
        self.stack_blocks(width, (!measured).then_some(text_renderer));
    }

    /// The size the blocks take at the width a line assumes, measured and then dropped, so a
    /// query that reaches no layout of its own leaves the recorded stack be.
    fn probe_stack(&mut self, width: f32, text_renderer: &mut TextRenderer) -> Size<f32> {
        let recorded = std::mem::take(&mut self.stack);
        self.stack_blocks(width.max(self.assumed_line_width()), Some(text_renderer));
        let size = Size {
            width: if self.stack.measured { width } else { 0.0 },
            height: self.stack.total,
        };
        let probed = std::mem::replace(&mut self.stack, recorded);
        // The lines were stacked in the probe's bands at the probe's width: put back the bands
        // the stack that stands belongs to.
        for (block, band) in self.blocks.iter_mut().zip(probed.bands) {
            block.line.set_row_bands(band);
        }
        size
    }
}
