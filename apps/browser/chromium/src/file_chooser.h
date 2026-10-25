#pragma once
#include "include/cef_values.h"
#include <algorithm>
#include <cctype>
#include <string>
#include <sys/stat.h>
#include <vector>

namespace tilo {
// The page's file chooser, answered by an agent instead of a panel on the
// person's desktop. These are the checks the host makes before it hands a
// path to the page: a path the chooser would never have offered must not
// arrive as a file the page believes the person picked.

inline bool ChooserDir(const std::string& path) {
  struct stat info;
  return ::stat(path.c_str(), &info) == 0 && S_ISDIR(info.st_mode);
}

inline bool ChooserFile(const std::string& path) {
  struct stat info;
  return ::stat(path.c_str(), &info) == 0 && S_ISREG(info.st_mode);
}

inline std::string ChooserLower(std::string value) {
  std::transform(value.begin(), value.end(), value.begin(), [](unsigned char c) { return static_cast<char>(std::tolower(c)); });
  return value;
}

// One `accept` entry of a chooser: `.pdf`, `image/*`, `text/plain`, or a
// platform description such as `*.pdf`. Empty means every file is offered.
inline std::vector<std::string> ChooserTokens(const std::vector<CefString>& extensions) {
  std::vector<std::string> tokens;
  for (const auto& extension : extensions) {
    const std::string group = extension.ToString();
    size_t start = 0;
    while (start <= group.size()) {
      const auto end = group.find(';', start);
      auto entry = group.substr(start, end == std::string::npos ? std::string::npos : end - start);
      entry.erase(0, entry.find_first_not_of(" \t"));
      const auto last = entry.find_last_not_of(" \t");
      entry.erase(last == std::string::npos ? 0 : last + 1);
      if (!entry.empty() && entry != "*" && entry != "*.*" && entry != "*/*")
        tokens.push_back(ChooserLower(entry));
      if (end == std::string::npos) break;
      start = end + 1;
    }
  }
  return tokens;
}

// The chooser's accepted types as the outline shows them.
inline std::string ChooserAccept(const std::vector<CefString>& extensions) {
  std::string accept;
  for (const auto& token : ChooserTokens(extensions)) {
    if (!accept.empty()) accept += ",";
    accept += token;
  }
  return accept.size() <= 200 ? accept : accept.substr(0, 200);
}

inline bool ChooserAccepts(const std::vector<std::string>& tokens, const std::string& path) {
  if (tokens.empty()) return true;
  std::string name = path;
  if (const auto slash = name.rfind('/'); slash != std::string::npos) name = name.substr(slash + 1);
  name = ChooserLower(name);
  const auto dot = name.rfind('.');
  const std::string type = dot == std::string::npos ? std::string() : name.substr(dot + 1);
  for (const auto& token : tokens) {
    if (token.size() > 1 && token[0] == '.') {
      if (ChooserLower(token.substr(1)) == type) return true;
    } else if (auto prefix = token; prefix.size() > 1 && prefix.compare(0, 2, "*.") == 0) {
      if (prefix.substr(2) == type) return true;
    } else if (type.find(token) != std::string::npos) {
      // A platform description: `PDF` covers `pdf`, `text/csv` covers `csv`.
      return true;
    }
  }
  return false;
}

// The most files one chooser answer may carry.
inline constexpr size_t kMaxChooserPaths = 16;

// Why the page would refuse this file, or empty when it accepts it.
inline std::string ChooserFileRefusal(const std::string& path, const std::vector<CefString>& extensions) {
  if (ChooserDir(path)) return "That is a folder, not a file";
  if (!ChooserFile(path)) return "No file at that path";
  const auto tokens = ChooserTokens(extensions);
  if (!ChooserAccepts(tokens, path)) {
    std::string accepted;
    for (const auto& token : tokens) {
      if (!accepted.empty()) accepted += ", ";
      accepted += token;
    }
    return "This chooser takes " + accepted;
  }
  return std::string();
}

// Why the chooser refuses these paths, or empty with `out` holding them all.
inline std::string ChooserRefusal(const std::vector<std::string>& paths, bool folder,
                                  const std::vector<CefString>& extensions, bool multiple,
                                  std::vector<std::string>& out) {
  if (paths.empty()) return "No files were given";
  if (paths.size() > kMaxChooserPaths) return "Too many files for one chooser";
  if (paths.size() > 1 && !multiple) return "This chooser takes one file";
  for (const auto& path : paths) {
    if (path.empty() || path.size() > 4096) return "Every file needs a full path";
    if (path[0] != '/') return "Give full paths from /";
    const auto error = folder ? std::string() : ChooserFileRefusal(path, extensions);
    if (!error.empty()) return error;
    out.push_back(path);
  }
  return std::string();
}
}  // namespace tilo
