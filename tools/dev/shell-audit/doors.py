"""The doors each side opens a surface by, named once for both.

A scenario in `catalogue.json` never names an action id, a shortcut or a
coordinate: it says what a person does ("open quick settings"), and this file
says how each side does it — its own action, its own shortcut, its own bar
item. When a side has no such door the door raises `DoorMissing` with what the
side offers instead, and the run records that against the scenario. A door is
never skipped and never emulated by pressing the other side's keys.

Everything here is side knowledge only: no scenario logic, no capture.
"""
from __future__ import annotations

import os
import shutil
import subprocess
import time

from sides import DoorMissing, Failed, LOCALE


class Doors:
    def __init__(self, side):
        self.side = side
        self.new = side.name == "new"
        self.table = {
            "desk.search": self.search,
            "desk.search.close": self.search_close,
            "desk.overview": self.overview,
            "desk.strips": self.strips,
            "desk.strip.next": lambda: self.strip_step(1),
            "desk.strip.previous": lambda: self.strip_step(-1),
            "desk.strip.create": self.strip_create,
            "desk.workspace.next": lambda: self.workspace_step(1),
            "desk.workspace.previous": lambda: self.workspace_step(-1),
            "desk.workspace.switch": self.workspace_switch,
            "desk.workspace.create": self.workspace_create,
            "desk.workspace.leave": self.workspace_leave,
            "desk.workspace.rename": self.workspace_rename,
            "desk.quick-settings": self.quick_settings,
            "desk.quick-settings.section": self.quick_settings_section,
            "desk.notifications": self.notifications,
            "desk.notifications.clear": self.notifications_clear,
            "window.hung": self.hung,
            "desk.osd": self.osd,
            "desk.volume": self.volume,
            "desk.brightness": self.brightness,
            "desk.tilo-menu": self.tilo_menu,
            "desk.tray": self.tray,
            "desk.clock": self.clock,
            "desk.workspace-settings": self.workspace_settings,
            "desk.workspace-menu": self.workspace_menu,
            "desk.person-card": self.person_card,
            "desk.power-menu": self.power_menu,
            "desk.lock": self.lock,
            "desk.logout": self.logout,
            "desk.restart": self.restart,
            "desk.shutdown": self.shutdown,
            "desk.theme": self.theme,
            "desk.shortcut": self.shortcut,
            "desk.settings": self.settings_app,
            "desk.settings-pane": self.settings_pane,
            "desk.displays": self.displays,
            "desk.display-move": self.display_move,
            "desk.display-layout": self.display_layout,
            "desk.files-shared": self.files_shared,
            "desk.file-portal": self.file_portal,
            "desk.dnd": self.dnd,
            "desk.wifi": self.wifi,
            "desk.bluetooth": self.bluetooth,
            "desk.escape": self.escape,
            "desk.inert": self.inert,
            "desk.menu": self.desk_menu,
            "desk.capture": self.capture,
            "desk.emoji": self.emoji,
            "desk.clipboard-history": self.clipboard_history,
            "desk.shortcut-help": self.shortcut_help,
            "desk.tour": self.tour,
            "desk.collaborators": self.collaborators,
            "desk.activity": self.activity,
            "desk.invite": self.invite,
            "desk.join-workspace": self.join_workspace,
            "window.launch": self.launch,
            "window.launchpad": self.launchpad,
            "desk.app-picker": self.app_picker,
            "desk.pick": self.pick,
            "window.close": self.window_close,
            "window.close-all": self.window_close_all,
            "window.maximize": lambda: self.maximize(True),
            "window.restore": lambda: self.maximize(False),
            "window.fullscreen": lambda: self.fullscreen(True),
            "window.unfullscreen": lambda: self.fullscreen(False),
            "window.minimize": self.minimize,
            "window.menu": self.window_menu,
            "window.tab-menu": self.tab_menu,
            "window.next": lambda: self.window_step(1),
            "window.previous": lambda: self.window_step(-1),
            "window.cycle-held": self.cycle_held,
            "window.cycle-release": self.cycle_release,
            "window.move-left": lambda: self.group_move("left"),
            "window.move-right": lambda: self.group_move("right"),
            "window.wider": lambda: self.group_width("wider"),
            "window.narrower": lambda: self.group_width("narrower"),
            "window.split-right": self.split_right,
            "window.new-group": self.new_group,
            "tab.new": self.tab_new,
            "tab.close": self.tab_close,
            "tab.next": lambda: self.tab_step(1),
            "tab.previous": lambda: self.tab_step(-1),
            "tab.rename": self.tab_rename,
            "tab.detach": self.tab_detach,
            "tab.reopen": self.tab_reopen,
            "tab.duplicate": self.tab_duplicate,
            "clipboard.set": self.clipboard_set,
            "clipboard.paste": self.clipboard_paste,
        }

    # -- plumbing ---------------------------------------------------------

    def open(self, door: str, **args):
        named = self.table.get(door)
        if named is None:
            raise DoorMissing(f"the audit has no door `{door}`")
        return named(**args)

    def tail(self, *tails: str, args: dict | None = None) -> dict:
        """The first of these action ids the side lists, invoked.

        Only a missing action is a missing door; an action that is there and
        refuses is that side's answer and stays an error."""
        for tail in tails:
            row = self.side.find_action(tail)
            if row is not None:
                return self.side.invoke(row, args)
        raise DoorMissing(f"none of the actions {', '.join(tails)}")

    def tail_contains(self, fragment: str, args: dict | None = None) -> dict:
        row = self.side.find_action(contains=fragment)
        if row is None:
            raise DoorMissing(f"no action id holding `{fragment}`")
        return self.side.invoke(row, args)

    def target(self, *needles: str) -> dict:
        row = self.side.target(*needles)
        if row is None:
            raise DoorMissing(f"no control matching {needles}")
        return row

    def press(self, *needles: str):
        row = self.target(*needles)
        rect = row["rect"]
        x, y = rect[0] + rect[2] / 2, rect[1] + rect[3] / 2
        self.side.move(x, y)
        time.sleep(0.15)
        self.side.click(x, y)
        return row

    def press_at(self, row: dict):
        rect = row["rect"]
        self.side.click(rect[0] + rect[2] / 2, rect[1] + rect[3] / 2)
        return row

    def click_window_body(self):
        """Put the pointer in the window in front, so it takes focus."""
        rows = [row for row in self.side.targets()
                if "foreign-surface" in str(row.get("name")) or "surface" in str(row.get("name"))]
        boxes = [row for row in rows if row["rect"][3] > 80]
        if not boxes:
            # A session that hosts app windows as their own surfaces names no
            # box for them; its desk area is where the window in front stands.
            if not self.side.windows():
                raise DoorMissing("no window on the desk")
            desks = [row for row in self.side.targets()
                     if "hosted applications" in str(row.get("name")).lower()]
            if not desks:
                raise DoorMissing("the desk lists no area to press")
            rect = max(desks, key=lambda entry: entry["rect"][2] * entry["rect"][3])["rect"]
            self.side.click(rect[0] + rect[2] / 2, rect[1] + rect[3] / 2)
            return {"rect": rect}
        row = max(boxes, key=lambda entry: entry["rect"][2] * entry["rect"][3])
        rect = row["rect"]
        self.side.click(rect[0] + rect[2] / 2, rect[1] + rect[3] / 2)
        return row

    # -- desk surfaces ----------------------------------------------------

    def search(self, query: str = ""):
        if self.new:
            return self.tail("desktop:desk\x1fsearch", args={"query": query})
        self.side.chord("Space", logo=True)
        if query:
            time.sleep(0.4)
            self.side.type_text(query)
        return "chord Super+Space"

    def search_close(self):
        if self.new:
            self.side.key("Escape")
            return "Escape"
        self.side.chord("Space", logo=True)
        return "chord Super+Space"

    def overview(self, open: bool = True):
        if self.new:
            return self.tail("desktop:chrome\x1foverview", args={"open": open})
        return self.tail("overview.toggle")

    def strips(self):
        if self.new:
            return self.tail("desktop:chrome\x1fshow-strips")
        raise DoorMissing("the old shell has no 'show strips' action")

    def strip_step(self, steps: int):
        if self.new:
            return self.tail("desktop:chrome\x1fstep-strip", args={"steps": steps})
        return self.tail("strip.focus-next" if steps > 0 else "strip.focus-previous")

    def strip_create(self, name: str = ""):
        if self.new:
            # The strips surface adds the strip itself; a programmatic caller is
            # refused that verb, so press the surface's own control.
            if self.side.target("new strip") is None and \
                    self.side.target("neuer streifen") is None:
                self.strips()
                time.sleep(0.6)
            row = self.side.target("new strip") or self.side.target("neuer streifen")
            if row is None:
                raise DoorMissing("the strips surface lists no control to add a strip")
            self.press_at(row)
            return row
        return self.tail("strip.create")

    def workspace_step(self, delta: int):
        if self.new:
            return self.tail("desktop:chrome\x1fswitch-workspace", args={"delta": delta})
        return self.tail("workspace.next" if delta > 0 else "workspace.previous")

    def workspace_switch(self, workspace: str):
        if self.new:
            return self.tail("desktop:chrome\x1fswitch-workspace", args={"workspace": workspace})
        row = self.side.find_action(contains="workspace.select.")
        if row is None:
            raise DoorMissing("no workspace.select action")
        return self.side.invoke(row)

    def workspace_create(self, name: str = ""):
        if self.new:
            return self.tail("desktop:chrome\x1fnew-workspace", args={"name": name})
        return self.tail("workspace.create", args={"name": name} or None)

    def workspace_leave(self, workspace: str = ""):
        if self.new:
            return self.tail("desktop:chrome\x1fleave-workspace", args={"workspace": workspace})
        raise DoorMissing("the old shell has no leave-workspace; it publishes and unpublishes")

    def workspace_rename(self, name: str):
        if self.new:
            # The open workspace, by the name it has now, is what gets renamed.
            tree = self.side.state().get("workspace_tree") or {}
            open_workspace = next((entry.get("name") for entry in tree.get("workspaces") or []
                                   if entry.get("id") == tree.get("active")), "")
            if not open_workspace:
                raise DoorMissing("no open workspace to rename")
            return self.tail("desktop:chrome\x1frename",
                             args={"workspace": open_workspace, "name": name})
        return self.tail("workspace.rename", args={"name": name})

    def quick_settings(self, open: bool | None = None):
        if self.new:
            return self.tail("desktop:chrome\x1fquick-settings")
        showing = bool(self.side.overlays().get("quick_settings"))
        if open is None or open != showing:
            self.press("topbar:quick-settings")
            time.sleep(0.6)
        return "tray press"

    def quick_settings_section(self, section: str):
        self.quick_settings(open=True)
        time.sleep(0.9)
        if self.new:
            names = {"wifi": "wifi", "network": "wifi", "bluetooth": "bluetooth",
                     "sound": "sound", "clipboard": "clipboard"}
            if section == "notifications":
                return self.tail_contains("show-notifications")
            if section == "controls":
                return self.tail_contains("show-controls")
            return self.tail_contains("open-section", args={"section": names[section]})
        ids = {"controls": ("show-controls",),
               "wifi": ("toggle-wifi",),
               "network": ("toggle-wifi",),
               "bluetooth": ("toggle-bluetooth",),
               "sound": ("sound-section",),
               "clipboard": ("paste",),
               "notifications": ("show-notifications",)}
        return self.tail(*ids[section])

    def notifications(self, open: bool = True):
        if self.new:
            return self.tail("desktop:chrome\x1fnotifications", args={"open": open})
        return self.tail("notifications.open") if open else self.tail("notifications.open")

    def notifications_clear(self):
        """Clear every notification in the centre."""
        if self.new:
            # The centre carries a control of its own for this.
            row = self.side.target("clear all notifications") or \
                self.side.target("alle benachrichtigungen")
            if row is not None:
                return self.press_at(row)
            return self.tail_contains("clear-notifications")
        return self.tail("notifications.clear")

    def hung(self):
        """What a person can do about an app that stopped answering."""
        if self.new:
            return self.tail("desktop:desk\x1fforce-quit-app", args={"tab": self.focused_tab()})
        raise DoorMissing("the old shell lists no force-quit action for a hung app")

    def osd(self, kind: str):
        if self.new:
            return self.tail("desktop:osd\x1fshow", args={"kind": kind})
        return self.tail(f"{kind}-up")

    def volume(self, delta: int = 1):
        if self.new:
            key = "AudioVolumeUp" if delta > 0 else "AudioVolumeDown"
            return self.side.key(key)
        self.quick_settings(open=True)
        time.sleep(0.8)
        return self.tail("volume-up" if delta > 0 else "volume-down")

    def brightness(self, delta: int = 1):
        if self.new:
            return self.tail("desktop:osd\x1fshow", args={"kind": "brightness"})
        self.quick_settings(open=True)
        time.sleep(0.8)
        return self.tail("brightness-up" if delta > 0 else "brightness-down")

    def tilo_menu(self, open: bool = True):
        if self.new:
            return self.tail("desktop:chrome\x1ftilo-menu", args={"open": open})
        raise DoorMissing("the old shell's bar has no Tilo menu; its tray opens quick settings")

    def clock(self):
        """The bar's clock."""
        if self.new:
            for row in self.side.targets():
                name = str(row.get("name") or "")
                rect = row["rect"]
                if rect[1] < 32 and rect[3] <= 32 and any(char.isdigit() for char in name) \
                        and ":" in name:
                    return self.press(name)
            raise DoorMissing("the new session's bar lists no clock to press")
        return self.press("clock")

    def tray(self):
        if self.new:
            return self.press("Benachrichtigungen") if self.side.target("Benachrichtigungen") \
                else self.press("Notifications")
        return self.press("topbar_tray_hit")

    def workspace_settings(self):
        if self.new:
            return self.tail("desktop:chrome\x1fworkspace-settings")
        return self.tail("settings.open")

    def workspace_menu(self):
        if self.new:
            return self.tail("desktop:chrome\x1fworkspace-menu")
        return self.press("topbar:workspace-settings")

    def person_card(self, person: str = ""):
        if self.new:
            if not person:
                for name in self.side.target_names():
                    if "your profile" in name.lower():
                        person = name.split(" (")[0]
                        break
            return self.tail("desktop:chrome\x1fshow-person", args={"person": person})
        return self.press("topbar:profile")

    def power_menu(self):
        if self.new:
            return self.tail("desktop:chrome\x1fshortcut", args={"action": "power_menu"})
        self.quick_settings(open=True)
        time.sleep(0.9)
        return self.tail("power-menu")

    def lock(self):
        if self.new:
            chrome = self.side.state().get("chrome") or {}
            if chrome.get("session_locked"):
                return "the screen is already locked"
            return self.tail("desktop:chrome\x1fshortcut", args={"action": "lock"})
        try:
            return self.tail("lock")
        except Exception as error:
            if "session_locked" in str(error):
                return "the screen is already locked"
            raise

    def logout(self):
        if self.new:
            raise DoorMissing("the new session lists no sign-out action")
        return self.tail("logout")

    def restart(self):
        raise DoorMissing("neither side offers restart from a headless session that owns no seat")

    def shutdown(self):
        raise DoorMissing("neither side offers shut-down from a headless session that owns no seat")

    def settings_app(self):
        """The Settings application itself, opened like any other app."""
        return self.launch("settings")

    def settings_pane(self, pane: str):
        """One pane of the workspace's own settings."""
        if self.new:
            # Settings is an app window with a sidebar; a person opens the app
            # and picks the section that names the pane.
            sections = {"general": ("personal", "allgemein"),
                        "members": ("users", "benutzer", "accounts & permissions"),
                        "privacy": ("device privacy & security", "datenschutz"),
                        "apps": ("default apps", "standard-apps")}
            self.launch("settings")
            self.side.wait_window("settings")
            time.sleep(0.6)
            for needle in sections[pane]:
                row = self.side.target(needle)
                if row is not None:
                    return self.press_at(row)
            raise DoorMissing(
                f"the Settings app shows no section named {pane!r}; it lists "
                + ", ".join(sorted({str(row.get("name")) for row in self.side.targets()
                                    if "/" not in str(row.get("name"))
                                    and "[" not in str(row.get("name"))})))
        ids = {"general": "settings.open", "members": "settings.members.open",
               "privacy": "settings.privacy.open", "apps": "settings.apps.open"}
        return self.tail(ids[pane])

    def _display_actions(self) -> str:
        titles = []
        for row in self.side.action_rows():
            text = f"{row.get('id')} {row.get('title')}".lower()
            if "display" in text or "anzeige" in text:
                titles.append(str(row.get("title") or row.get("id")))
        return ", ".join(sorted(set(titles))) or "none"

    def displays(self):
        """What displays are connected, and their scale and position."""
        if self.new:
            raise DoorMissing(f"the new session has no action that shows the connected "
                              f"displays; it lists {self._display_actions()}")
        return self.tail("display.outputs.inspect")

    def display_move(self):
        """Move the window in front to the next display."""
        if self.new:
            return self.tail("move-to-display", args={"steps": 1})
        return self.tail("group.move-to-display-next")

    def display_layout(self, layout: str):
        if self.new:
            raise DoorMissing(f"the new session has no display-layout action; it lists "
                              f"{self._display_actions()}")
        return self.tail(f"display.layout.{layout}")

    def files_shared(self):
        """The workspace's shared apps and files, from settings."""
        if self.new:
            raise DoorMissing("the new session lists no shared-files settings action")
        return self.tail("settings.apps.open")

    # What an app's own ask for a file is called, per side's language.
    PORTALS = {"open": ("open file", "open a file", "open audio file", "datei öffnen",
                        "choose a file", "import file"),
               "save": ("save as", "save a copy", "export file", "speichern unter",
                        "save to a folder")}

    def file_portal(self, kind: str = "open"):
        """The dialog an app opens when it asks which file to open or save."""
        hints = self.PORTALS.get(kind)
        if hints is None:
            raise DoorMissing(f"the audit knows no `{kind}` file dialog")
        listed = []
        for row in self.side.action_rows():
            text = f"{row.get('title')} {row.get('id')}".lower()
            if any(hint in text for hint in hints):
                return self.side.invoke(row)
            if "file" in text or "datei" in text:
                listed.append(str(row.get("title") or row.get("id")))
        raise DoorMissing(f"no app lists an action that asks which file to {kind}; the "
                          f"side lists {', '.join(sorted(set(listed))[:12]) or 'none'}")

    def theme(self, theme: str):
        if self.new:
            row = self.side.find_action("choose-theme")
            if row is None:
                raise DoorMissing("no choose-theme action")
            return self.side.invoke(row, {"theme": theme.capitalize()})
        return self.tail(f"appearance.theme.{theme}")

    def shortcut(self, action: str):
        """One of the desktop's own shortcuts, by the name both sides know it."""
        if self.new:
            # The names the catalogue tells stories with, as this session's
            # shortcut action spells them.
            names = {"previous_window": "focus_previous_window",
                     "next_window": "focus_next_window"}
            return self.tail("desktop:chrome\x1fshortcut",
                             args={"action": names.get(action, action)})
        old = {
            "search": lambda: self.search(),
            "workspace_overview": lambda: self.tail("overview.toggle"),
            "lock": lambda: self.tail("lock"),
            "quick_settings": lambda: self.quick_settings(open=True),
            "shortcut_help": lambda: self.tail("shortcut-help.toggle"),
            "previous_window": lambda: self.tail("window.focus-previous"),
            "next_window": lambda: self.tail("window.focus-next"),
            "previous_tab": lambda: self.tail("tab.previous"),
            "next_tab": lambda: self.tail("tab.next"),
            "tab_menu": lambda: self.tail("tab.menu"),
            "previous_strip": lambda: self.tail("strip.focus-previous"),
            "next_strip": lambda: self.tail("strip.focus-next"),
            "previous_workspace": lambda: self.tail("workspace.previous"),
            "next_workspace": lambda: self.tail("workspace.next"),
            "toggle_maximize": lambda: self.tail("group.toggle-maximize"),
            "reopen_closed_tab": lambda: self.tail("tab.reopen"),
            "detach_tab": lambda: self.tail("tab.move-to-new-group"),
            "clipboard_history": lambda: self.tail("quick-settings.clipboard"),
            "notifications": lambda: self.notifications(open=True),
            "emoji_picker": lambda: self.tail("emoji.picker"),
            "toggle_magnifier": lambda: self.tail("magnifier.toggle"),
            "power_menu": lambda: self.tail("power-menu"),
        }
        if action not in old:
            raise DoorMissing(f"the old shell has no `{action}` shortcut")
        return old[action]()

    def dnd(self, on: bool | None = None):
        if self.new:
            return self.tail_contains("set-do-not-disturb", args={"on": True if on is None else on})
        return self.tail("notifications.dnd.toggle")

    def wifi(self, on: bool | None = None):
        if self.new:
            self.quick_settings(open=True)
            time.sleep(0.9)
            row = self.side.find_action(contains="set-wifi") or \
                self.side.find_action(contains="toggle-wifi")
            if row is None:
                raise DoorMissing(f"the panel publishes no radio control here; the "
                                  f"session lists {self._radio_actions()}")
            return self.side.invoke(row, {"on": True if on is None else on})
        self.quick_settings(open=True)
        time.sleep(0.9)
        return self.tail("toggle-wifi")

    def bluetooth(self, on: bool | None = None):
        if self.new:
            self.quick_settings(open=True)
            time.sleep(0.9)
            row = self.side.find_action(contains="set-bluetooth") or \
                self.side.find_action(contains="toggle-bluetooth")
            if row is None:
                raise DoorMissing(f"the panel publishes no radio control here; the "
                                  f"session lists {self._radio_actions()}")
            return self.side.invoke(row, {"on": True if on is None else on})
        self.quick_settings(open=True)
        time.sleep(0.9)
        return self.tail("toggle-bluetooth")

    def _radio_actions(self) -> str:
        titles = []
        for row in self.side.action_rows():
            text = f"{row.get('id')} {row.get('title')}".lower()
            if any(word in text for word in ("wifi", "wi-fi", "bluetooth", "network")):
                titles.append(str(row.get("title") or row.get("id")))
        return ", ".join(sorted(set(titles))) or "none"

    def escape(self):
        self.side.key("Escape")
        try:
            self.side.client_key("Escape")
        except Failed:
            pass
        return "Escape"

    def inert(self):
        width, height = self.side.size
        self.side.move(width - 8, height - 8)
        time.sleep(0.2)
        return "pointer parked"

    def desk_menu(self):
        width, height = self.side.size
        self.side.right_click(width / 2, height * 0.65)
        return "right click on the desk"

    def capture(self):
        if self.new:
            for name in self.side.target_names():
                if "screenshot" in name.lower() or "bildschirmfoto" in name.lower():
                    return self.press(name)
            # The Print key is what a person presses; its effect is the capture
            # studio's own action, which no shortcut name stands for.
            row = self.side.find_action(contains="screenshot")
            if row is not None:
                return self.side.invoke(row)
            return self.side.key("Print")
        return self.tail("capture.open")

    def emoji(self):
        if self.new:
            return self.tail("desktop:chrome\x1fshortcut", args={"action": "emoji_picker"})
        return self.tail("emoji.picker")

    def clipboard_history(self):
        if self.new:
            return self.tail("desktop:chrome\x1fshortcut", args={"action": "clipboard_history"})
        return self.tail("quick-settings.clipboard")

    def shortcut_help(self):
        if self.new:
            return self.tail("desktop:chrome\x1fshortcut", args={"action": "shortcut_help"})
        return self.tail("shortcut-help.toggle")

    def tour(self):
        if self.new:
            raise DoorMissing("the new session lists no desktop tour action")
        return self.tail("desktop-tour.open")

    def collaborators(self):
        if self.new:
            return self.tail("desktop:chrome\x1ftoggle-collaborators")
        return self.tail("collaborators.open")

    def activity(self):
        if self.new:
            tree = self.side.state().get("workspace_tree") or {}
            active = tree.get("active")
            if active is None:
                raise DoorMissing("no workspace is open whose history to show")
            return self.tail("desktop:chrome\x1factivity",
                             args={"workspace": str(active), "view": {"explore": None}})
        return self.tail("room.activity.open")

    def invite(self):
        if self.new:
            return self.tail("desktop:chrome\x1fjoin-workspace")
        return self.tail("collaborators.open")

    def join_workspace(self):
        if self.new:
            return self.tail("desktop:chrome\x1fjoin-workspace")
        raise DoorMissing("the old shell has no join-workspace action")

    # -- windows ----------------------------------------------------------

    def launch(self, app: str):
        return self.side.launch_app(app)

    # The launchpad's cards name the collaboration apps; the app store's
    # packages are what a press asks for.
    CARDS = {"notes": ("notes", "notizen"), "chat": ("chat", "anruf"),
             "tasks": ("tasks", "aufgaben"), "meet": ("meet", "besprechen"),
             "activity": ("activity", "laufenden"), "invite": ("invite", "einladen")}

    def launchpad(self, app: str):
        """Press the app's card on an empty workspace."""
        if self.new:
            for label in self.CARDS.get(app, (app,)):
                row = self.side.target(label)
                if row is not None:
                    self.press_at(row)
                    return self.side.wait_window(app)
            raise DoorMissing(f"the launchpad shows no card for {app}")
        try:
            self.press(f"launchpad:{app}")
        except DoorMissing:
            raise DoorMissing(f"the old launchpad shows no card for {app}")
        try:
            return self.side.wait_window(app, timeout=8)
        except Failed:
            raise DoorMissing(
                "the old launchpad's card opens the app through the Applications "
                "provider, and this profile installs no app packages (its apps start "
                "as binaries)")

    def app_picker(self):
        """The launchpad's 'open an app' door: a palette over every app."""
        if self.new:
            # This session opens an app from its search overlay, where a name
            # is typed, or from the cards an empty desk's launchpad shows; it
            # has no separate palette over every installed app.
            return self.search()
        return self.press("open-app")

    def pick(self, app: str):
        """Type an app's name in a palette that is open and take the first row."""
        time.sleep(0.5)
        self.side.type_text(app)
        time.sleep(0.8)
        self.side.key("Enter")
        try:
            return self.side.wait_window(app, timeout=10)
        except Failed:
            raise DoorMissing(f"the picker does not offer {app} here")

    def focused_window(self) -> dict:
        rows = self.side.windows()
        if not rows:
            raise DoorMissing("no window on the desk")
        for row in rows:
            if row.get("focused") or row.get("active"):
                return row
        return rows[0]

    def focused_group(self) -> str:
        return str(self.focused_window().get("group"))

    def focused_tab(self) -> str:
        return str(self.focused_window().get("tab"))

    def window_close(self):
        if self.new:
            return self.tail("desktop:chrome\x1fclose-window", args={"group": self.focused_group()})
        return self.tail("group.close")

    def window_close_all(self):
        """Close windows until the desk holds none, letting each one go first."""
        closed = 0
        for _ in range(24):
            rows = self.side.windows()
            if not rows:
                return f"closed {closed}"
            try:
                self.window_close()
            except Failed as error:
                return f"closed {closed}, then {error}"
            closed += 1
            end = time.monotonic() + 4.0
            while time.monotonic() < end and len(self.side.windows()) >= len(rows):
                time.sleep(0.1)
        return f"closed {closed}"

    def maximize(self, on: bool):
        if self.new:
            return self.tail("desktop:chrome\x1fmaximize",
                             args={"group": self.focused_group(), "on": on})
        return self.tail("group.toggle-maximize" if on else "group.restore")

    def fullscreen(self, on: bool):
        if self.new:
            return self.tail("desktop:chrome\x1ffullscreen",
                             args={"group": self.focused_group(), "on": on})
        return self.tail("group.fullscreen")

    def minimize(self):
        if self.new:
            raise DoorMissing("the new session lists no minimize action")
        raise DoorMissing("the old shell's windows have no minimize")

    def window_menu(self):
        if self.new:
            try:
                self.click_window_body()
            except DoorMissing:
                pass
            self.side.right_click(*self.tab_point())
            return "right click on the window's tab"
        return self.tail("tab.menu")

    def tab_point(self):
        """The centre of the focused window's tab in the bar, from the targets."""
        window = self.focused_window()
        title = str(window.get("title") or "")
        for row in self.side.targets():
            name = str(row.get("name") or "")
            if title and title.lower() in name.lower() and row["rect"][3] < 60:
                rect = row["rect"]
                return rect[0] + rect[2] / 2, rect[1] + rect[3] / 2
        for row in self.side.targets():
            if row["rect"][1] < 60 and row["rect"][3] > 20 and row["rect"][3] < 60 \
                    and row["rect"][2] > 60:
                rect = row["rect"]
                return rect[0] + rect[2] / 2, rect[1] + rect[3] / 2
        raise DoorMissing("no tab in the bar to press")

    def tab_menu(self):
        if self.new:
            self.side.right_click(*self.tab_point())
            return "right click on the tab"
        return self.tail("tab.menu")

    def window_step(self, steps: int):
        if self.new:
            return self.tail("desktop:desk\x1fstep-focus", args={"steps": steps})
        return self.tail("window.focus-next" if steps > 0 else "window.focus-previous")

    def cycle_held(self):
        self.side.modifiers(alt=True)
        self.side.key("Tab")
        return "Alt held, Tab pressed"

    def cycle_release(self):
        self.side.key_up("Tab")
        self.side.modifiers()
        return "Alt released"

    def group_move(self, direction: str):
        if self.new:
            # A person carries the window group along its strip with the key
            # the desktop binds for it.
            return self.tail("desktop:chrome\x1fshortcut",
                             args={"action": f"move_group_{direction}"})
        return self.tail("group.move-left" if direction == "left" else "group.move-right")

    def group_width(self, direction: str):
        if self.new:
            return self.tail("desktop:chrome\x1fshortcut",
                             args={"action": f"resize_{'wider' if direction == 'wider' else 'narrower'}"})
        return self.tail("group.wider" if direction == "wider" else "group.narrower")

    def split_right(self):
        if self.new:
            return self.tail("desktop:chrome\x1fsplit-right", args={"tab": self.focused_tab()})
        return self.tail("tab.split-right")

    def new_group(self):
        if self.new:
            return self.tail("desktop:chrome\x1fnew-group", args={"tab": self.focused_tab()})
        return self.tail("tab.move-to-new-group")

    # -- tabs -------------------------------------------------------------

    def tab_new(self, app: str = "notes"):
        if self.new:
            raise DoorMissing("the new session opens a window from the launchpad or search; "
                              "it has no new-tab-in-this-group action")
        # The old shell's new tab asks which app to put in it, by its
        # application id through the app store.
        return self.tail("tab.open-beside", args={"app": f"so.tilo.{app}"})

    def tab_close(self):
        if self.new:
            return self.tail("desktop:chrome\x1fclose-window", args={"group": self.focused_group()})
        return self.tail("tab.close")

    def tab_step(self, steps: int):
        if self.new:
            return self.tail("desktop:desk\x1fstep-focus", args={"steps": steps})
        return self.tail("tab.next" if steps > 0 else "tab.previous")

    def tab_rename(self, name: str):
        if self.new:
            return self.tail("desktop:chrome\x1frename",
                             args={"group": self.focused_group(), "name": name})
        return self.tail("tab.rename", args={"name": name})

    def tab_detach(self):
        if self.new:
            return self.tail("desktop:chrome\x1fnew-group", args={"tab": self.focused_tab()})
        return self.tail("tab.move-to-new-group")

    def tab_reopen(self):
        if self.new:
            return self.tail("desktop:chrome\x1freopen-closed")
        return self.tail("tab.reopen")

    def tab_duplicate(self):
        if self.new:
            raise DoorMissing("the new session lists no duplicate-tab action")
        return self.tail("tab.duplicate")

    # -- clipboard and text ----------------------------------------------

    def clipboard_set(self, text: str):
        """Put text on the session's clipboard, as a person copying would."""
        address = self.side.bus_address()
        env = dict(os.environ)
        env.update(LOCALE)
        if address:
            env["DBUS_SESSION_BUS_ADDRESS"] = address
        runtime = getattr(self.side, "wayland_runtime", None)
        if runtime:
            env["XDG_RUNTIME_DIR"] = str(runtime)
            for entry in sorted(os.listdir(runtime)):
                if entry.startswith("wayland-") and not entry.endswith(".lock"):
                    env["WAYLAND_DISPLAY"] = entry
                    break
        if shutil.which("wl-copy") is None:
            raise DoorMissing("no wl-copy to put text on the clipboard")
        # A compositor with no clipboard manager keeps wl-copy serving the
        # selection in the background; that process staying up is the copy
        # having worked, not a step that never returns.
        serving = subprocess.Popen(["wl-copy", text], env=env, stdout=subprocess.DEVNULL,
                                   stderr=subprocess.PIPE, text=True, start_new_session=True)
        try:
            code = serving.wait(timeout=3)
        except subprocess.TimeoutExpired:
            return "wl-copy, serving the selection"
        if code != 0:
            raise Failed(f"wl-copy: {(serving.stderr.read() or '').strip()[:200]}")
        return "wl-copy"

    def clipboard_paste(self):
        if self.new:
            return self.side.chord("v", ctrl=True)
        self.side.modifiers(ctrl=True)
        self.side.client_key("v")
        self.side.modifiers()
        return "Ctrl+V to the focused client"

