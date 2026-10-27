#pragma once
// Extensions' keyboard shortcuts (`commands` in a manifest).
//
// Chrome gives each command the key its manifest suggests and fires it from
// its windows' keyboard handling; every page here is an offscreen browser no
// window holds, so the host does: the keys the Browser forwards into a page
// are looked up here first (host.cc, Key). Which key each command gets
// follows Chrome:
//
// - The key suggested for this platform (`mac` or `linux`), else `default`,
//   parsed as Chrome parses it: Ctrl or Alt (on macOS Command, which "Ctrl"
//   means there, or MacCtrl), maybe Shift, and one key; never Ctrl with Alt.
// - At most four commands of an extension get a key.
// - A key the Browser keeps for itself (BrowserKeeps) is never given: the
//   Browser answers it before the page could see it.
// - Two extensions asking for one key: the one the host was told of first
//   keeps it, as in Chrome the one installed first does.
//
// Media keys and Insert are parsed but given to no command: the Browser
// forwards neither into a page, so such a command could never run.
#include <algorithm>
#include <cctype>
#include <cstddef>
#include <map>
#include <optional>
#include <string>
#include <tuple>
#include <utility>
#include <vector>

namespace tilo {

// A key with its modifiers, as the Browser forwards one: `meta` is Command
// on macOS. `key` is Chrome's name of it ("Y", "9", "Comma", "Up").
struct Shortcut {
  bool ctrl = false;
  bool alt = false;
  bool shift = false;
  bool meta = false;
  std::string key;
  bool operator<(const Shortcut& other) const {
    return std::tie(ctrl, alt, shift, meta, key) < std::tie(other.ctrl, other.alt, other.shift, other.meta, other.key);
  }
  bool operator==(const Shortcut& other) const {
    return std::tie(ctrl, alt, shift, meta, key) == std::tie(other.ctrl, other.alt, other.shift, other.meta, other.key);
  }
};

// A command as its manifest declares it: `suggested` is the key text for
// this platform, empty for none.
struct ExtensionCommand {
  std::string name;
  std::string description;
  std::string suggested;
};

// What Chrome's `commands.getAll` answers for one command.
struct ListedCommand {
  std::string name;
  std::string description;
  std::string shortcut;
};

// The commands that act as a click on the extension's toolbar button.
inline bool ActionCommand(const std::string& name) {
  return name == "_execute_action" || name == "_execute_browser_action" || name == "_execute_page_action";
}

inline bool MediaKey(const std::string& key) {
  return key == "MediaNextTrack" || key == "MediaPlayPause" || key == "MediaPrevTrack" || key == "MediaStop";
}

// `text` ("Ctrl+Shift+Y") as Chrome parses a suggested key on this
// platform; nothing for one Chrome refuses.
inline std::optional<Shortcut> ParseShortcut(const std::string& text, bool mac) {
  static const char* const named[] = {"Comma", "Period", "Space", "Home", "End", "PageUp", "PageDown", "Insert", "Delete",
                                      "Tab", "Up", "Down", "Left", "Right", "MediaNextTrack", "MediaPlayPause",
                                      "MediaPrevTrack", "MediaStop"};
  std::vector<std::string> tokens;
  size_t start = 0;
  while (true) {
    const size_t plus = text.find('+', start);
    tokens.push_back(text.substr(start, plus == std::string::npos ? std::string::npos : plus - start));
    if (plus == std::string::npos) break;
    start = plus + 1;
  }
  if (tokens.empty() || tokens.size() > 4) return std::nullopt;
  Shortcut shortcut;
  // On macOS "Ctrl" is Command and "MacCtrl" the control key.
  bool command = false;
  for (const auto& token : tokens) {
    if (token == "Ctrl") {
      if (mac) command = shortcut.meta = true; else shortcut.ctrl = true;
    } else if (token == "Command") {
      if (!mac) return std::nullopt;
      command = shortcut.meta = true;
    } else if (token == "MacCtrl") {
      if (!mac) return std::nullopt;
      shortcut.ctrl = true;
    } else if (token == "Alt") {
      shortcut.alt = true;
    } else if (token == "Shift") {
      shortcut.shift = true;
    } else {
      if (!shortcut.key.empty()) return std::nullopt;
      if (token.size() == 1 && std::isalnum(static_cast<unsigned char>(token[0]))) {
        shortcut.key = std::string(1, static_cast<char>(std::toupper(static_cast<unsigned char>(token[0]))));
      } else if (std::find(std::begin(named), std::end(named), token) != std::end(named)) {
        shortcut.key = token;
      } else {
        return std::nullopt;
      }
    }
  }
  if (shortcut.key.empty()) return std::nullopt;
  const bool modified = shortcut.ctrl || shortcut.alt || shortcut.meta;
  if (MediaKey(shortcut.key)) return modified || shortcut.shift ? std::nullopt : std::optional<Shortcut>(shortcut);
  // Ctrl with Alt is AltGr on many layouts, which types characters.
  if (!modified || (shortcut.ctrl && shortcut.alt) || (command && shortcut.alt)) return std::nullopt;
  return shortcut;
}

// The shortcut a key the Browser forwards makes, its key named as Chrome
// names it. The Browser forwards the character a key typed: with Shift,
// a digit or a comma is the US layout's symbol on it.
inline std::optional<Shortcut> KeyShortcut(const std::string& key, bool ctrl, bool alt, bool shift, bool meta) {
  static const std::pair<const char*, const char*> named[] = {
    {"ArrowUp", "Up"}, {"ArrowDown", "Down"}, {"ArrowLeft", "Left"}, {"ArrowRight", "Right"}, {"Home", "Home"},
    {"End", "End"}, {"PageUp", "PageUp"}, {"PageDown", "PageDown"}, {"Delete", "Delete"}, {"Tab", "Tab"},
  };
  Shortcut shortcut{ctrl, alt, shift, meta, {}};
  for (const auto& [from, to] : named) if (key == from) shortcut.key = to;
  if (shortcut.key.empty() && key.size() == 1) {
    const char c = key[0];
    const std::string shifted = ")!@#$%^&*(";
    if (std::isalnum(static_cast<unsigned char>(c))) shortcut.key = std::string(1, static_cast<char>(std::toupper(static_cast<unsigned char>(c))));
    else if (shifted.find(c) != std::string::npos) shortcut.key = std::string(1, static_cast<char>('0' + shifted.find(c)));
    else if (c == ',' || c == '<') shortcut.key = "Comma";
    else if (c == '.' || c == '>') shortcut.key = "Period";
    else if (c == ' ') shortcut.key = "Space";
  }
  if (shortcut.key.empty()) return std::nullopt;
  return shortcut;
}

// The Browser's own keys among those a command could ask for, which it
// answers before a page sees them (apps/browser: browser_widget.rs
// handle_browser_shortcut and handle_address_shortcut, browser_window.rs
// and ui::shell_tabs for the desktop's tab chords), and on macOS the editing
// keys the host runs itself (host.cc, Key). The list is browser_keeps.inc,
// which the Browser's own test holds against what it answers.
inline std::vector<std::string> BrowserKeptText(bool mac) {
  static const char* const kept[] = {
#include "browser_keeps.inc"
  };
  const std::string platform = mac ? "mac " : "linux ";
  const std::string host = " (host)";
  std::vector<std::string> texts;
  for (const char* entry : kept) {
    std::string text(entry);
    if (text.rfind(platform, 0) != 0) continue;
    text = text.substr(platform.size());
    if (text.size() > host.size() && text.compare(text.size() - host.size(), host.size(), host) == 0)
      text.resize(text.size() - host.size());
    texts.push_back(std::move(text));
  }
  return texts;
}
inline bool BrowserKeeps(const Shortcut& shortcut, bool mac) {
  static const auto parsed = [](bool on_mac) {
    std::vector<Shortcut> keys;
    for (const auto& text : BrowserKeptText(on_mac))
      if (auto key = ParseShortcut(text, on_mac)) keys.push_back(*key);
    return keys;
  };
  static const std::vector<Shortcut> on_mac = parsed(true), on_linux = parsed(false);
  const auto& kept = mac ? on_mac : on_linux;
  return std::find(kept.begin(), kept.end(), shortcut) != kept.end();
}

// What Chrome's `commands.getAll` shows of a key: "Ctrl+Shift+Y" on Linux,
// "⇧⌘Y" on macOS.
inline std::string ShortcutText(const Shortcut& shortcut, bool mac) {
  static const std::pair<const char*, const char*> linux_names[] = {
    {"Comma", ","}, {"Period", "."}, {"PageUp", "Page Up"}, {"PageDown", "Page Down"}, {"Up", "Up Arrow"},
    {"Down", "Down Arrow"}, {"Left", "Left Arrow"}, {"Right", "Right Arrow"},
  };
  static const std::pair<const char*, const char*> mac_names[] = {
    {"Comma", ","}, {"Period", "."}, {"PageUp", "⇞"}, {"PageDown", "⇟"}, {"Up", "↑"},
    {"Down", "↓"}, {"Left", "←"}, {"Right", "→"}, {"Home", "↖"}, {"End", "↘"},
    {"Delete", "⌦"}, {"Tab", "⇥"},
  };
  std::string key = shortcut.key;
  if (mac) {
    for (const auto& [from, to] : mac_names) if (key == from) key = to;
    std::string text;
    if (shortcut.ctrl) text += "⌃";
    if (shortcut.alt) text += "⌥";
    if (shortcut.shift) text += "⇧";
    if (shortcut.meta) text += "⌘";
    return text + key;
  }
  for (const auto& [from, to] : linux_names) if (key == from) key = to;
  std::string text;
  if (shortcut.ctrl) text += "Ctrl+";
  else if (shortcut.alt) text += "Alt+";
  if (shortcut.shift) text += "Shift+";
  return text + key;
}

class ExtensionCommands {
 public:
  explicit ExtensionCommands(bool mac) : mac_(mac) {}

  // An extension's commands, as its manifest declares them. One told of
  // again keeps its place: the keys it holds stay its.
  void Set(const std::string& extension, std::vector<ExtensionCommand> commands) {
    auto found = std::find_if(extensions_.begin(), extensions_.end(), [&](const auto& entry) { return entry.first == extension; });
    if (found != extensions_.end()) found->second = std::move(commands);
    else extensions_.emplace_back(extension, std::move(commands));
    Assign();
  }
  void Forget(const std::string& extension) {
    extensions_.erase(std::remove_if(extensions_.begin(), extensions_.end(), [&](const auto& entry) { return entry.first == extension; }),
                      extensions_.end());
    Assign();
  }

  // The extension and command a key runs, if any.
  std::optional<std::pair<std::string, std::string>> Find(const Shortcut& shortcut) const {
    auto found = assigned_.find(shortcut);
    if (found == assigned_.end()) return std::nullopt;
    return found->second;
  }

  // Every key a command holds now, in a stable order.
  std::vector<Shortcut> Assigned() const {
    std::vector<Shortcut> keys;
    for (const auto& [key, owner] : assigned_) keys.push_back(key);
    return keys;
  }

  // The extension's commands with the key each was given, empty for none:
  // the toolbar button's first, then the rest by name, as Chrome lists them.
  std::vector<ListedCommand> All(const std::string& extension) const {
    std::vector<ListedCommand> listed;
    for (const auto& [id, commands] : extensions_) {
      if (id != extension) continue;
      for (const auto& command : commands) {
        std::string shortcut;
        for (const auto& [key, owner] : assigned_)
          if (owner.first == extension && owner.second == command.name) shortcut = ShortcutText(key, mac_);
        listed.push_back(ListedCommand{command.name, command.description, shortcut});
      }
    }
    std::stable_sort(listed.begin(), listed.end(), [](const ListedCommand& a, const ListedCommand& b) {
      return std::make_pair(!ActionCommand(a.name), a.name) < std::make_pair(!ActionCommand(b.name), b.name);
    });
    return listed;
  }

 private:
  void Assign() {
    assigned_.clear();
    for (const auto& [extension, commands] : extensions_) {
      std::vector<const ExtensionCommand*> ordered;
      for (const auto& command : commands) ordered.push_back(&command);
      std::stable_sort(ordered.begin(), ordered.end(), [](const ExtensionCommand* a, const ExtensionCommand* b) {
        return std::make_pair(!ActionCommand(a->name), a->name) < std::make_pair(!ActionCommand(b->name), b->name);
      });
      int given = 0;
      for (const auto* command : ordered) {
        if (given >= 4 || command->suggested.empty()) continue;
        auto shortcut = ParseShortcut(command->suggested, mac_);
        if (!shortcut || MediaKey(shortcut->key) || shortcut->key == "Insert" || BrowserKeeps(*shortcut, mac_)) continue;
        if (assigned_.count(*shortcut)) continue;
        assigned_[*shortcut] = {extension, command->name};
        ++given;
      }
    }
  }

  bool mac_;
  // In the order the host was told of them.
  std::vector<std::pair<std::string, std::vector<ExtensionCommand>>> extensions_;
  std::map<Shortcut, std::pair<std::string, std::string>> assigned_;
};

}  // namespace tilo
