//! The desktop's tree as the person's layout reads and changes it: whichever shell holds the tree
//! hands the layout this, and every layout action reads and replaces the tree through it.

use std::rc::Rc;

use tilo_desktop_model::{FloatPlace, GroupId, Layout, TabId, WorkspaceId};

/// A window the person floated or put back, for the layout to make an arranging action of.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct AskedFloat {
    /// The window in front of the group, which the action names.
    pub tab: TabId,
    pub group: u64,
    /// Where it floats; `None` puts it back in its strip.
    pub float: Option<FloatPlace>,
}

/// What holds the desktop's tree.
pub trait DesktopTree {
    /// Read the tree as it stands.
    fn with_tree(&self, read: &mut dyn FnMut(&Layout));
    /// A fresh id for something new in the tree, unique across the person's devices.
    fn mint_id(&self) -> u64;
    /// Replace the tree with `tree`, as a change of its own.
    fn replace_tree(&self, tree: Layout);
    /// Put the keyboard on the window `tab`, bringing it forward: whether it is in the tree.
    fn focus_tab(&self, tab: TabId) -> bool;
    /// The windows the person floated or put back since the last call, in the order asked.
    fn take_asked_floats(&self) -> Vec<AskedFloat>;
    /// Bumped on every change of the tree.
    fn revision(&self) -> u64;
    /// The window group `group` of `workspace` drew `height` pixels of what it shows: what a place
    /// that hugs its content takes. Whether the tree took it.
    fn report_drawn(&self, workspace: WorkspaceId, group: GroupId, height: u32) -> bool;
}

/// A shared [`DesktopTree`]. Cheap to clone.
#[derive(Clone)]
pub struct TreeHandle(Rc<dyn DesktopTree>);

impl TreeHandle {
    #[must_use]
    pub fn new(tree: Rc<dyn DesktopTree>) -> Self {
        Self(tree)
    }

    /// What `read` makes of the tree as it stands.
    #[expect(
        clippy::expect_used,
        reason = "every desktop tree calls the read it is handed exactly once"
    )]
    pub fn read<R>(&self, read: impl FnOnce(&Layout) -> R) -> R {
        let mut read = Some(read);
        let mut out = None;
        self.0.with_tree(&mut |tree| {
            if let Some(read) = read.take() {
                out = Some(read(tree));
            }
        });
        out.expect("a desktop tree reads its tree once")
    }

    /// See [`DesktopTree::mint_id`].
    #[must_use]
    pub fn mint_id(&self) -> u64 {
        self.0.mint_id()
    }

    /// See [`DesktopTree::replace_tree`].
    pub fn replace_tree(&self, tree: Layout) {
        self.0.replace_tree(tree);
    }

    /// See [`DesktopTree::focus_tab`].
    pub fn focus_tab(&self, tab: TabId) -> bool {
        self.0.focus_tab(tab)
    }

    /// See [`DesktopTree::take_asked_floats`].
    #[must_use]
    pub fn take_asked_floats(&self) -> Vec<AskedFloat> {
        self.0.take_asked_floats()
    }

    /// See [`DesktopTree::report_drawn`].
    pub fn report_drawn(&self, workspace: WorkspaceId, group: GroupId, height: u32) -> bool {
        self.0.report_drawn(workspace, group, height)
    }

    /// See [`DesktopTree::revision`].
    #[must_use]
    pub fn revision(&self) -> u64 {
        self.0.revision()
    }
}
