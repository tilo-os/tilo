// The keys the Browser keeps for itself among those an extension's command
// could ask for (extension_commands.h, BrowserKeeps): it answers each before
// a page sees it, so no command is given one. One entry a line, its platform
// and then the key as a manifest spells it there (on macOS "Ctrl" is
// Command and "MacCtrl" the control key). "(host)" marks one this host
// answers itself (host.cc, Key) rather than the Browser.
//
// The Browser's test (apps/browser, browser_window.rs) presses every key a
// command could hold through its own key handling and fails when what it
// answers and this list differ, so the two cannot drift apart. Keep each
// entry a plain string on a line of its own: that test reads this file.

// Linux: Find and its next match, the bookmark, reload and zoom back, the
// desktop's tab keys, a private window, the address, back and forward.
"linux Ctrl+F",
"linux Ctrl+G",
"linux Ctrl+D",
"linux Ctrl+R",
"linux Ctrl+T",
"linux Ctrl+Tab",
"linux Ctrl+Shift+F",
"linux Ctrl+Shift+G",
"linux Ctrl+Shift+D",
"linux Ctrl+Shift+R",
"linux Ctrl+Shift+T",
"linux Ctrl+Shift+Tab",
"linux Ctrl+0",
"linux Ctrl+W",
"linux Ctrl+L",
"linux Ctrl+Shift+N",
"linux Alt+Left",
"linux Alt+Right",
"linux Alt+Shift+Left",
"linux Alt+Shift+Right",

// macOS: the same with Command, where Find, its next match, the bookmark
// and zoom back want Command and reload, the address, a private window and
// the tab keys take Control as well; and the editing keys this host runs.
"mac Ctrl+F",
"mac Ctrl+G",
"mac Ctrl+D",
"mac Ctrl+Shift+F",
"mac Ctrl+Shift+G",
"mac Ctrl+Shift+D",
"mac MacCtrl+Ctrl+F",
"mac MacCtrl+Ctrl+G",
"mac MacCtrl+Ctrl+D",
"mac MacCtrl+Ctrl+Shift+F",
"mac MacCtrl+Ctrl+Shift+G",
"mac MacCtrl+Ctrl+Shift+D",
"mac Ctrl+0",
"mac MacCtrl+Ctrl+0",
"mac Ctrl+R",
"mac Ctrl+T",
"mac Ctrl+Tab",
"mac Ctrl+Shift+R",
"mac Ctrl+Shift+T",
"mac Ctrl+Shift+Tab",
"mac MacCtrl+R",
"mac MacCtrl+T",
"mac MacCtrl+Tab",
"mac MacCtrl+Shift+R",
"mac MacCtrl+Shift+T",
"mac MacCtrl+Shift+Tab",
"mac MacCtrl+Ctrl+R",
"mac MacCtrl+Ctrl+T",
"mac MacCtrl+Ctrl+Tab",
"mac MacCtrl+Ctrl+Shift+R",
"mac MacCtrl+Ctrl+Shift+T",
"mac MacCtrl+Ctrl+Shift+Tab",
"mac Ctrl+W",
"mac Ctrl+L",
"mac MacCtrl+W",
"mac MacCtrl+L",
"mac MacCtrl+Ctrl+W",
"mac MacCtrl+Ctrl+L",
"mac Ctrl+Shift+N",
"mac MacCtrl+Shift+N",
"mac MacCtrl+Ctrl+Shift+N",
"mac Ctrl+A (host)",
"mac Ctrl+C (host)",
"mac Ctrl+X (host)",
"mac Ctrl+V (host)",
"mac Ctrl+Z (host)",
"mac Ctrl+Shift+A (host)",
"mac Ctrl+Shift+C (host)",
"mac Ctrl+Shift+X (host)",
"mac Ctrl+Shift+V (host)",
"mac Ctrl+Shift+Z (host)",
