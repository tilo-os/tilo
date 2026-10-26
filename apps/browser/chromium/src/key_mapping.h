#pragma once
#include <algorithm>
#include <cctype>
#include <string>

namespace tilo {
// The IPC carries logical key names and, when the sender knows it, the
// physical key's DOM code. Without a code the conventional US key for the
// name stands in.
struct KeyMapping {
  int windows = 0;
  int mac = -1;
  int x11 = 0;
  char16_t character = 0;
  char16_t mac_character = 0;
};
inline KeyMapping MapKey(const std::string& key) {
  struct NamedKey { const char* name; KeyMapping mapping; };
  static const NamedKey named[] = {
    {"Enter", {13,36,36,'\r','\r'}}, {"Tab", {9,48,23,'\t','\t'}},
    {"Backspace", {8,51,22,'\b',0x7f}}, {"Delete", {46,117,119,0,0xf728}},
    {"Escape", {27,53,9,27,27}}, {"ArrowLeft", {37,123,113,0,0xf702}},
    {"ArrowUp", {38,126,111,0,0xf700}}, {"ArrowRight", {39,124,114,0,0xf703}},
    {"ArrowDown", {40,125,116,0,0xf701}}, {"Home", {36,115,110,0,0xf729}},
    {"End", {35,119,115,0,0xf72b}}, {"PageUp", {33,116,112,0,0xf72c}},
    {"PageDown", {34,121,117,0,0xf72d}},
  };
  for (const auto& item : named) if (key == item.name) return item.mapping;
  if (key.size() != 1) return {};
  const unsigned char character = key[0];
  const char lower = static_cast<char>(std::tolower(character));
  const std::string letters = "abcdefghijklmnopqrstuvwxyz";
  const int mac_letters[] = {0,11,8,2,14,3,5,4,34,38,40,37,46,45,31,35,12,15,1,17,32,9,13,7,16,6};
  const int x11_letters[] = {38,56,54,40,26,41,42,43,31,44,45,46,58,57,32,33,24,27,39,28,30,55,25,53,29,52};
  auto index = letters.find(lower);
  if (index != std::string::npos) return {std::toupper(character),mac_letters[index],x11_letters[index],character,character};
  const std::string digits = "0123456789";
  const int mac_digits[] = {29,18,19,20,21,23,22,26,28,25};
  const int x11_digits[] = {19,10,11,12,13,14,15,16,17,18};
  index = digits.find(lower);
  if (index != std::string::npos) return {character,mac_digits[index],x11_digits[index],character,character};
  struct Symbol { char value; int windows; int mac; int x11; };
  static const Symbol symbols[] = {
    {' ',32,49,65},{'-',189,27,20},{'=',187,24,21},{'[',219,33,34},
    {']',221,30,35},{'\\',220,42,51},{';',186,41,47},{'\'',222,39,48},
    {',',188,43,59},{'.',190,47,60},{'/',191,44,61},{'`',192,50,49},
  };
  for (const auto& symbol : symbols)
    if (character == symbol.value) return {symbol.windows,symbol.mac,symbol.x11,character,character};
  // Character events still carry text for characters without a known US key.
  return {0,-1,0,character,character};
}

// The US key at a place on the keyboard, named as a DOM KeyboardEvent.code
// ("KeyY", "Digit1", "IntlBackslash"), or 0 for a place not named here.
inline char UsCharacterAt(const std::string& code) {
  if (code.size()==4 && code.compare(0,3,"Key")==0 && code[3]>='A' && code[3]<='Z')
    return static_cast<char>(code[3]-'A'+'a');
  if (code.size()==6 && code.compare(0,5,"Digit")==0 && code[5]>='0' && code[5]<='9') return code[5];
  struct Place { const char* code; char character; };
  static const Place places[] = {
    {"Minus",'-'},{"Equal",'='},{"BracketLeft",'['},{"BracketRight",']'},
    {"Backslash",'\\'},{"Semicolon",';'},{"Quote",'\''},{"Comma",','},
    {"Period",'.'},{"Slash",'/'},{"Backquote",'`'},{"Space",' '},
  };
  for (const auto& place : places) if (code == place.code) return place.character;
  return 0;
}

// Whether the protocol knows `code` as a place on the keyboard.
inline bool KnownKeyCode(const std::string& code) {
  return code=="IntlBackslash" || UsCharacterAt(code)!=0;
}

// Whether `code` reads as a DOM code at all ("ArrowLeft", "KeyA"). A code
// the protocol does not place (a named key's) is taken and left to its name.
inline bool KeyCodeShaped(const std::string& code) {
  return !code.empty() && code.size()<=32 &&
    std::all_of(code.begin(),code.end(),[](char c){return std::isalnum(static_cast<unsigned char>(c));});
}

// A key as the page should see it: what the layout makes of it (its text
// and, for Latin letters and digits, its key code, as Chromium gives them on
// each layout), on the physical key that was pressed. A key whose text has
// no US key (a Cyrillic or Greek letter, a German umlaut) takes the key code
// of the US key in its place, so Ctrl+С on a Russian layout is the page's
// Ctrl+C, as in Chrome.
inline KeyMapping MapKey(const std::string& key, const std::string& code) {
  KeyMapping mapping = MapKey(key);
  if (code.empty()) return mapping;
  KeyMapping place;
  if (code=="IntlBackslash") place = {226,10,94,0,0};
  else if (const char at = UsCharacterAt(code)) place = MapKey(std::string(1,at));
  else return mapping;
  mapping.mac = place.mac;
  mapping.x11 = place.x11;
  if (mapping.windows==0) mapping.windows = place.windows;
  return mapping;
}

// The Latin letter a shortcut names: the key's own when it is one, else the
// US letter on the key pressed (Command+С on a Russian layout copies). A key
// whose text is on a US key names that key, whatever its place: Command+;
// on Dvorak, typed on the US Z key, is not Command+Z.
inline char ShortcutLetter(const std::string& key, const std::string& code) {
  if (key.size()==1)
    return std::isalpha(static_cast<unsigned char>(key[0]))
      ? static_cast<char>(std::tolower(static_cast<unsigned char>(key[0]))) : 0;
  const char at = UsCharacterAt(code);
  return at>='a' && at<='z' ? at : 0;
}
}  // namespace tilo
