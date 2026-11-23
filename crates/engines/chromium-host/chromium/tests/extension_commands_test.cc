#include "extension_commands.h"
#include <cassert>
#include <string>
#include <vector>

using tilo::ExtensionCommand;
using tilo::ExtensionCommands;
using tilo::KeyShortcut;
using tilo::ParseShortcut;
using tilo::Shortcut;
using tilo::ShortcutText;

static Shortcut Key(const std::string& key, bool ctrl, bool alt, bool shift, bool meta = false) {
  return *KeyShortcut(key, ctrl, alt, shift, meta);
}

int main() {
  // Chrome's suggested keys: Ctrl or Alt, maybe Shift, and one key.
  assert(ParseShortcut("Ctrl+Shift+Y", false) == Key("Y", true, false, true));
  assert(ParseShortcut("Alt+Shift+Y", false) == Key("y", false, true, true));
  assert(ParseShortcut("Ctrl+Comma", false) == Key(",", true, false, false));
  assert(ParseShortcut("Alt+Up", false) == Key("ArrowUp", false, true, false));
  // Shift alone, Ctrl with Alt (AltGr), no key, two keys, an unknown key,
  // or macOS's own modifiers on Linux: refused, as Chrome refuses them.
  for (const char* refused : {"Shift+Y", "Y", "Ctrl+Alt+Y", "Ctrl+Shift", "Ctrl+Y+U", "Ctrl+F13", "Command+Y",
                              "MacCtrl+Y", "Ctrl+Shift+Alt+Y+U", "", "Ctrl+Shift+MediaStop"}) {
    assert(!ParseShortcut(refused, false));
  }
  // A media key takes no modifier.
  assert(ParseShortcut("MediaPlayPause", false));
  // On macOS "Ctrl" is Command, which the Browser forwards as meta, and
  // MacCtrl the control key.
  assert(ParseShortcut("Ctrl+Shift+Y", true) == Key("Y", false, false, true, true));
  assert(ParseShortcut("Command+Shift+Y", true) == Key("Y", false, false, true, true));
  assert(ParseShortcut("MacCtrl+Shift+Y", true) == Key("Y", true, false, true));
  assert(!ParseShortcut("Command+Alt+Y", true));

  // A key forwarded with Shift reads as the US layout's symbol on it.
  assert(Key("(", true, false, true).key == "9");
  assert(Key("<", false, true, true).key == "Comma");
  assert(!KeyShortcut("Escape", true, false, false, false));

  // What commands.getAll shows, as Chrome writes it.
  assert(ShortcutText(*ParseShortcut("Ctrl+Shift+Y", false), false) == "Ctrl+Shift+Y");
  assert(ShortcutText(*ParseShortcut("Alt+Shift+Up", false), false) == "Alt+Shift+Up Arrow");
  assert(ShortcutText(*ParseShortcut("Ctrl+Shift+Y", true), true) == "⇧⌘Y");

  // The Browser's own keys: each entry of the list parses, and they are the
  // keys the Browser answers (its own test holds the list to that).
  for (bool mac : {false, true}) {
    const auto texts = tilo::BrowserKeptText(mac);
    assert(texts.size() > 10);
    for (const auto& text : texts) assert(ParseShortcut(text, mac) && tilo::BrowserKeeps(*ParseShortcut(text, mac), mac));
  }
  assert(tilo::BrowserKeeps(*ParseShortcut("Ctrl+F", false), false));
  assert(tilo::BrowserKeeps(*ParseShortcut("Alt+Shift+Left", false), false));
  // A password manager's key for filling a login is not the address's.
  assert(!tilo::BrowserKeeps(*ParseShortcut("Ctrl+Shift+L", false), false));
  assert(!tilo::BrowserKeeps(*ParseShortcut("Ctrl+Shift+0", false), false));
  // On macOS the tab keys take Control as well as Command.
  assert(tilo::BrowserKeeps(*ParseShortcut("MacCtrl+T", true), true));
  assert(tilo::BrowserKeeps(*ParseShortcut("Ctrl+C", true), true));
  assert(!tilo::BrowserKeeps(*ParseShortcut("MacCtrl+C", true), true));

  ExtensionCommands commands(false);
  commands.Set("first", {
    {"_execute_action", "", "Ctrl+Shift+Y"},
    {"toggle", "Toggle", "Alt+Shift+T"},
    // The Browser's own: find, and a new tab.
    {"find", "Find", "Ctrl+Shift+F"},
    {"tab", "Tab", "Ctrl+T"},
    {"none", "No key", ""},
    {"broken", "Broken", "Shift+Q"},
  });
  assert((commands.Find(Key("Y", true, false, true)) == std::pair<std::string, std::string>{"first", "_execute_action"}));
  assert((commands.Find(Key("T", false, true, true)) == std::pair<std::string, std::string>{"first", "toggle"}));
  assert(!commands.Find(Key("F", true, false, true)));
  assert(!commands.Find(Key("t", true, false, false)));
  assert(!commands.Find(Key("Y", true, false, false)));
  // The button's command first, then the others by name.
  const auto all = commands.All("first");
  assert(all.size() == 6);
  assert(all[0].name == "_execute_action" && all[0].shortcut == "Ctrl+Shift+Y");
  assert(all[1].name == "broken" && all[1].shortcut.empty());
  assert(all[2].name == "find" && all[2].shortcut.empty());
  assert(all[5].name == "toggle" && all[5].shortcut == "Alt+Shift+T" && all[5].description == "Toggle");

  // A key two extensions ask for stays with the one told of first, also
  // when the first is told of again.
  commands.Set("second", {{"grab", "Grab", "Alt+Shift+T"}, {"own", "Own", "Alt+Shift+O"}});
  commands.Set("first", {{"toggle", "Toggle", "Alt+Shift+T"}});
  assert(commands.Find(Key("T", false, true, true))->first == "first");
  assert(commands.All("second")[0].shortcut.empty());
  assert(commands.Find(Key("O", false, true, true))->second == "own");
  // Gone, it lets go of its keys.
  commands.Forget("first");
  assert(commands.Find(Key("T", false, true, true))->first == "second");
  assert(!commands.Find(Key("Y", true, false, true)));

  // At most four commands of an extension get a key.
  commands.Set("many", {{"a", "", "Alt+Shift+1"}, {"b", "", "Alt+Shift+2"}, {"c", "", "Alt+Shift+3"},
                        {"d", "", "Alt+Shift+4"}, {"e", "", "Alt+Shift+5"}});
  assert(commands.Find(Key("$", false, true, true)));
  assert(!commands.Find(Key("%", false, true, true)));
  return 0;
}
