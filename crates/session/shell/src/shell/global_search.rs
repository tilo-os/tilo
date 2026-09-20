//! `GlobalSearch` — the Cmd+K palette overlay.
//!
//! Thin wrapper around [`ui::widget::command_palette::CommandPalette`]
//! that seeds a default set of system commands and lets the shell install a
//! context-aware prefix filter (files, workspace, app). Commands are tagged by
//! id (`ws.*`, `file.*`, `app.*`, `settings.*`) so context switching just
//! installs a `set_filter` closure that keeps the matching prefixes plus
//! `settings.*`, which is always reachable.

use ui::widget::command_palette::{CommandEntry, CommandPalette};

/// Alpha the shell overlay host tints the `GlobalSearch` surface with. Kept
/// in sync with [`crate::shell::overlay_host::MODAL_SURFACE_ALPHA`].
pub const PALETTE_SURFACE_ALPHA: f32 = 1.0;

/// Which slice of the command catalog the palette is focusing on. `Any`
/// shows everything; narrower variants restrict to a prefix family while
/// always leaving `settings.*` visible as the escape hatch.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SearchContext {
    Any,
    Files,
    Workspace,
    App,
}

/// The shell-level Cmd+K palette.
pub struct GlobalSearch {
    palette: CommandPalette,
    context: SearchContext,
}

impl GlobalSearch {
    pub fn new() -> Self {
        Self {
            palette: CommandPalette::new(),
            context: SearchContext::Any,
        }
    }

    pub fn seed_default_commands(&mut self) {
        let defaults: &[(&str, &str)] = &[
            ("ws.create", "Create Workspace"),
            ("ws.rename", "Rename Workspace"),
            ("ws.switch", "Switch Workspace…"),
            ("settings.open", "Open Settings"),
            ("file.open", "Open File"),
        ];
        for (id, label) in defaults {
            self.palette.push(CommandEntry::new(*id, *label));
        }
    }

    pub fn palette(&self) -> &CommandPalette {
        &self.palette
    }

    pub fn palette_mut(&mut self) -> &mut CommandPalette {
        &mut self.palette
    }

    pub fn context(&self) -> &SearchContext {
        &self.context
    }

    /// Tint alpha for the palette surface — solid slate-900, depth comes
    /// from the scrim beneath it, not from letting the desktop through.
    pub fn surface_alpha() -> f32 {
        PALETTE_SURFACE_ALPHA
    }

    /// Install a context-aware filter: `settings.*` always stays visible
    /// as the escape hatch, narrow contexts additionally keep their prefix
    /// family (`file.*`, `ws.*`, `app.*`).
    pub fn set_context_kind(&mut self, ctx: SearchContext) {
        self.context = ctx;
        match ctx {
            SearchContext::Any => {
                self.palette
                    .set_filter(Box::new(|_entry: &CommandEntry| true));
            }
            SearchContext::Files => {
                self.palette.set_filter(Box::new(|entry: &CommandEntry| {
                    entry.id.starts_with("file.") || entry.id.starts_with("settings.")
                }));
            }
            SearchContext::Workspace => {
                self.palette.set_filter(Box::new(|entry: &CommandEntry| {
                    entry.id.starts_with("ws.") || entry.id.starts_with("settings.")
                }));
            }
            SearchContext::App => {
                self.palette.set_filter(Box::new(|entry: &CommandEntry| {
                    entry.id.starts_with("app.") || entry.id.starts_with("settings.")
                }));
            }
        }
    }
}

impl Default for GlobalSearch {
    fn default() -> Self {
        Self::new()
    }
}
