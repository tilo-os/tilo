//! `GlobalSearch` — the Cmd+K palette overlay.
//!
//! Thin wrapper around [`quick_ui::widget::command_palette::CommandPalette`]
//! that seeds a default set of system commands and lets the shell install a
//! context-aware prefix filter (files, workspace, app). Commands are tagged by
//! id (`ws.*`, `file.*`, `app.*`, `settings.*`) so context switching just
//! installs a `set_filter` closure that keeps the matching prefixes plus
//! `settings.*`, which is always reachable.
//!
//! Spec: `docs/superpowers/specs/2026-04-18-nirios-shell-design.md` — line 210
//! (GlobalSearch: command palette + context-aware filters).

use quick_ui::widget::command_palette::{CommandEntry, CommandPalette};

/// Alpha the shell overlay host tints the GlobalSearch palette surface
/// with. Concept §5.1 specifies `bg-slate-900/70 backdrop-blur-md`; until
/// the pinned Vello grows a blur primitive we honour just the /70 alpha
/// split (visual-gap #3 in `docs/nirios/visual-gaps.md`). This constant is
/// the source of truth the `shell_chrome_test` suite pins the palette tint
/// against, and it is kept in sync with `overlay_host::MODAL_SURFACE_ALPHA`.
pub const PALETTE_SURFACE_ALPHA: f32 = 0.70;

/// Which slice of the command catalog the palette is focusing on.
///
/// `Any` shows everything; the narrower variants restrict to a prefix family
/// (`file.*`, `ws.*`, `app.*`) while always leaving `settings.*` visible so
/// the user never loses the escape hatch to Settings.
#[derive(Clone, Debug, PartialEq, Eq)]
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

    /// Seed the default system command set. Idempotent-safe to call once at
    /// construction; see spec §9 for the canonical v1 list.
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

    /// Translucent-tint fallback alpha for the palette surface. See
    /// [`PALETTE_SURFACE_ALPHA`] for background — the palette sits at
    /// `bg-slate-900/70` over the desktop scrim, and losing this alpha
    /// step would collapse the command palette back into an opaque flat
    /// rectangle (visual-gap #3).
    pub fn surface_alpha() -> f32 {
        PALETTE_SURFACE_ALPHA
    }

    /// Install a context-aware filter on the underlying palette.
    ///
    /// `settings.*` entries are always kept visible so the user can reach
    /// Settings from any context; the narrow contexts additionally keep the
    /// matching prefix family (`file.*`, `ws.*`, `app.*`).
    pub fn set_context_kind(&mut self, ctx: SearchContext) {
        self.context = ctx.clone();
        match ctx {
            SearchContext::Any => {
                // Restore "everything visible" by installing a pass-all filter.
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
