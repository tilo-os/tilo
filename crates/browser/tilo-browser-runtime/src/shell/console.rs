//! The console layout: the page's desktop with the Console app over all of
//! it, for a person who has agents work for them and wants to see and steer
//! what they do rather than work at the desktop themselves.
//!
//! It is the same desktop, session and windows as the desktop layout, not a
//! page of its own: Console is an ordinary window, held fullscreen, so the
//! bar and the strips are out of sight and everything Console reads (the
//! workspaces' history, the agent door) is served the way it is served to
//! any window. The page holds Console in front until the person opens
//! something in it — takes over a run, opens what an action changed, opens a
//! workspace in Tilo. That window comes to the front of the desktop, and the
//! page leaves the console for the full desktop, Console's window still on
//! it to go back to.

use tilo_desktop_surface::navigation::{self, ChromeEffect};

use super::BrowserShell;
pub(crate) use crate::console_hold::ConsoleHold;
use crate::console_hold::{CONSOLE_APP, Front, Step};

impl BrowserShell {
    /// Hold Console in front while the page shows the console; answers
    /// whether it asked the desktop for anything.
    pub(super) fn hold_console(&mut self) -> bool {
        let Some(hold) = self.console else {
            return false;
        };
        let front = self.handle.read(|tree| {
            let Some(route) = navigation::active_route(tree) else {
                return Front::Nothing;
            };
            let app = tree
                .tab(route.tab)
                .and_then(|tab| tilo_desktop_model::apps::canonical_app_id(&tab.tile));
            if app.as_deref() == Some(CONSOLE_APP) {
                Front::Console {
                    group: route.group,
                    fullscreen: tree.fullscreen_group() == Some(route.group),
                }
            } else {
                Front::Other {
                    fullscreen: tree.fullscreen_group(),
                }
            }
        });
        let (next, step) = hold.step(front);
        self.console = Some(next);
        match step {
            Step::Rest => return false,
            Step::Show => self.dispatch(ChromeEffect::ShowApp {
                app: CONSOLE_APP.into(),
            }),
            Step::Fill(group) => self.dispatch(ChromeEffect::Fullscreen {
                group: group.to_string(),
                on: true,
            }),
            Step::Release(fullscreen) => {
                if let Some(group) = fullscreen {
                    self.dispatch(ChromeEffect::Fullscreen {
                        group: group.to_string(),
                        on: false,
                    });
                }
            }
        }
        true
    }
}
