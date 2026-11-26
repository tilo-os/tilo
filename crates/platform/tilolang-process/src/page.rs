//! A page window: a viewer's page, its app's view laid out here, in the app's own process, and
//! written as a page for whoever looks at it through the device that started it
//! ([`tilo_server_protocol::page_window`]).
//!
//! The device hands the page its connection where a session hands a window's patch connection
//! ([`Shows::Page`](runtime_portal::process_link::Shows::Page)). The app's code draws into a
//! window of the toolkit's, side by side with it; what the person does on the page is acted on
//! that window as their click or their typing is, and each answer is the page whole or the
//! elements that changed.

use std::io;
use std::os::fd::{AsFd, AsRawFd as _};
use std::os::unix::net::UnixStream;

use tilo_app_window::AppWindow;
use tilo_render_protocol::patch::local::End;
use tilo_server_protocol::page::PageAction;
use tilo_server_protocol::page_window::{self, Answer, Ask};
use ui::composition::page::{PAGE_CSS, Page, theme_css};
use ui::headless::FrameHarness;

use crate::hosted::Hosted;

/// The size a page's window is laid out at, in logical pixels: a browser lays the page out again
/// at its own, so this only decides which of a split's columns the window shows.
const VIEWPORT: (u32, u32) = (1280, 800);

/// How often the window and the app's code may answer each other before a page is written as it
/// is: the rows a list shows, then the rows those rows show.
const EXCHANGES: usize = 8;

/// One viewer's page window and the connection it is asked on.
pub struct PageWindow {
    connection: UnixStream,
    harness: FrameHarness,
    page: Page,
    css: String,
    /// Whether the page last written had no element at all: the app had drawn nothing yet, for
    /// instance while it read its document. There is nothing of it in the page to patch, so its
    /// first drawing is answered whole rather than as the elements that changed.
    empty: bool,
}

impl PageWindow {
    /// The page window that shows what the app's code sends to `window`'s other end, asked on
    /// `connection`.
    pub fn new(connection: UnixStream, window: End) -> Self {
        let mut theme = ui::Theme::light();
        theme.reduce_motion = true;
        let css = theme_css(&theme) + PAGE_CSS;
        let harness = FrameHarness::new_with_theme(
            ui::State::new(),
            Box::new(AppWindow::new(Box::new(window), Vec::new())),
            VIEWPORT,
            theme,
        );
        Self {
            connection,
            harness,
            page: Page::new(),
            css,
            empty: true,
        }
    }

    /// The connection the page is asked on.
    #[must_use]
    pub fn connection(&self) -> std::os::fd::BorrowedFd<'_> {
        self.connection.as_fd()
    }

    /// One turn of the app's code and its window, and an answer to every ask that arrived: whether
    /// anything changed.
    ///
    /// # Errors
    /// The connection broke: the page is gone.
    pub fn turn(&mut self, hosted: &mut Hosted) -> io::Result<bool> {
        let mut changed = self.exchange(hosted);
        while readable(self.connection.as_fd()) {
            let ask = page_window::read(&mut self.connection)?;
            let answer = self.answer(ask, hosted);
            page_window::write(&mut self.connection, &answer)?;
            changed = true;
        }
        Ok(changed)
    }

    fn answer(&mut self, ask: Ask, hosted: &mut Hosted) -> Answer {
        match ask {
            Ask::Show { show } => {
                // The address names what to show by the semantic target its element carries (a
                // note's row): activated as a click on that row is.
                if let Some(show) = show {
                    let widget = self
                        .harness
                        .composition()
                        .targeted(&show)
                        .map(str::to_owned);
                    let shown = widget
                        .ok_or_else(|| "no element carries it".to_owned())
                        .and_then(|widget| {
                            self.act_on(&widget, Some(&show), &PageAction::Activate)
                        });
                    if let Err(reason) = shown {
                        tracing::debug!(%reason, show, "the page's address names nothing it shows");
                    }
                    self.exchange(hosted);
                }
                self.page = Page::new();
                let body = self.page.render(&self.harness.composition());
                self.empty = body.is_empty();
                Answer::Shown {
                    css: self.css.clone(),
                    body,
                }
            }
            Ask::Act { id, target, action } => {
                // An element a page numbers for a part of a widget (a text's block, a
                // control) is acted on through the widget it is a part of.
                let Some(widget) = self.page.path(id).map(|path| {
                    path.split_once('#')
                        .map_or(path, |(widget, _)| widget)
                        .to_owned()
                }) else {
                    return Answer::Failed(format!("the page numbers no element {id}"));
                };
                if let Err(reason) = self.act_on(&widget, target.as_deref(), &action) {
                    tracing::debug!(%reason, widget, "the page's action was refused");
                }
                self.exchange(hosted);
                Answer::Patched(self.patch())
            }
            Ask::Changes => {
                // A page the app had drawn nothing into when it was asked for it has no element
                // to patch: what it draws next is written whole.
                if self.empty {
                    self.page = Page::new();
                    let body = self.page.render(&self.harness.composition());
                    if !body.is_empty() {
                        self.empty = false;
                        return Answer::Shown {
                            css: self.css.clone(),
                            body,
                        };
                    }
                }
                Answer::Patched(self.patch())
            }
        }
    }

    fn act_on(
        &mut self,
        widget: &str,
        target: Option<&str>,
        action: &PageAction,
    ) -> Result<(), String> {
        let action = match action {
            PageAction::Activate => ui::a11y::Action::Activate,
            PageAction::Toggle => ui::a11y::Action::ToggleCheck,
            PageAction::Focus => ui::a11y::Action::Focus,
            PageAction::Value(value) => ui::a11y::Action::SetValue(value.clone()),
            PageAction::Select { anchor, focus } => ui::a11y::Action::SetSelection {
                anchor: usize::try_from(*anchor).unwrap_or(usize::MAX),
                focus: usize::try_from(*focus).unwrap_or(usize::MAX),
            },
        };
        self.harness.perform_semantic_named(widget, target, &action)
    }

    fn patch(&mut self) -> Vec<tilo_server_protocol::page::PagePatch> {
        self.page
            .patch(&self.harness.composition())
            .into_iter()
            .map(|patch| tilo_server_protocol::page::PagePatch {
                id: patch.id,
                html: patch.html,
            })
            .collect()
    }

    fn window(&mut self) -> Option<&mut AppWindow> {
        self.harness
            .root_widget_mut()
            .as_any_mut()?
            .downcast_mut::<AppWindow>()
    }

    /// Let the window and the app's code answer each other until neither has more to say, laying
    /// the window out after each answer.
    fn exchange(&mut self, hosted: &mut Hosted) -> bool {
        let mut changed = false;
        for _ in 0..EXCHANGES {
            if let Some(window) = self.window() {
                window.flush();
            }
            let moved = hosted.turn();
            let received = self.window().is_some_and(AppWindow::receive);
            let owing = self.window().is_some_and(|window| window.owes());
            self.harness.relayout();
            changed |= moved || received;
            if !moved && !received && !owing {
                break;
            }
        }
        if let Some(why) = self
            .window()
            .and_then(|window| window.failed().map(str::to_owned))
        {
            tracing::error!(%why, "the page's window refused what the app sent");
        }
        changed
    }
}

/// Whether the socket `fd` holds something to read.
fn readable(fd: std::os::fd::BorrowedFd<'_>) -> bool {
    let mut poll = libc::pollfd {
        fd: fd.as_raw_fd(),
        events: libc::POLLIN,
        revents: 0,
    };
    // SAFETY: one initialized pollfd naming an owned descriptor; a zero timeout makes it a
    // nonblocking query.
    let ready = unsafe { libc::poll(&raw mut poll, 1, 0) };
    ready > 0 && poll.revents & libc::POLLIN != 0
}
