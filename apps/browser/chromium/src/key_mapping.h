#pragma once
#include <cctype>
#include <string>

namespace qos {
// The IPC currently carries logical key names, not hardware scan codes. Use the
// conventional US physical key for these names until the UI sends layout data.
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
}  // namespace qos
