#include "key_mapping.h"
#include <cassert>

int main() {
  // A US layout: the name and the place agree.
  auto a = tilo::MapKey("a", "KeyA");
  assert(a.windows == 'A' && a.mac == 0 && a.x11 == 38 && a.character == 'a');
  // German: the key a US layout prints Y on types z. The page hears z (key
  // code Z, as Chromium gives on that layout) on the key KeyY.
  auto z = tilo::MapKey("z", "KeyY");
  assert(z.windows == 'Z' && z.character == 'z');
  assert(z.mac == tilo::MapKey("y").mac && z.x11 == tilo::MapKey("y").x11);
  // French: A sits where a US layout has Q.
  auto french = tilo::MapKey("a", "KeyQ");
  assert(french.windows == 'A' && french.x11 == tilo::MapKey("q").x11);
  // Russian: a Cyrillic letter has no US key, so the key's place gives the
  // code Ctrl+С needs to copy.
  auto cyrillic = tilo::MapKey("\xd1\x81", "KeyC");
  assert(cyrillic.windows == 'C' && cyrillic.x11 == tilo::MapKey("c").x11 && cyrillic.mac == tilo::MapKey("c").mac);
  // French digits row: & on the 1 key is the page's key code 1.
  assert(tilo::MapKey("&", "Digit1").windows == '1');
  // The key between left Shift and Z on ISO keyboards.
  auto iso = tilo::MapKey("<", "IntlBackslash");
  assert(iso.windows == 226 && iso.x11 == 94 && iso.mac == 10);
  // Without a place, or with one the protocol does not name, the name decides.
  assert(tilo::MapKey("z", "").x11 == tilo::MapKey("z").x11);
  assert(tilo::MapKey("Enter", "Enter").windows == 13);
  assert(tilo::KnownKeyCode("KeyZ") && tilo::KnownKeyCode("Digit0") && tilo::KnownKeyCode("Space") && tilo::KnownKeyCode("IntlBackslash"));
  assert(!tilo::KnownKeyCode("Keyz") && !tilo::KnownKeyCode("Enter") && !tilo::KnownKeyCode(""));
  // A named key's code is taken, and its name decides.
  assert(tilo::KeyCodeShaped("ArrowLeft") && tilo::KeyCodeShaped("KeyA"));
  assert(!tilo::KeyCodeShaped("") && !tilo::KeyCodeShaped("Key A") && !tilo::KeyCodeShaped(std::string(33,'a')));
  assert(tilo::MapKey("ArrowLeft", "ArrowLeft").windows == 37);
  // Shortcuts are named by Latin letters whatever the layout.
  assert(tilo::ShortcutLetter("C", "KeyC") == 'c');
  assert(tilo::ShortcutLetter("\xd1\x81", "KeyC") == 'c');
  assert(tilo::ShortcutLetter("a", "KeyQ") == 'a');
  assert(tilo::ShortcutLetter("&", "Digit1") == 0);
}
