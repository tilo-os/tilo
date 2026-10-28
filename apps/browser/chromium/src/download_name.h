#pragma once
// The name a download is saved under, from the name a page suggests.
//
// The suggestion is untrusted: it may carry a path, control characters, or
// be far longer than a file name may be. What is saved is its last path
// part, with control characters replaced, at most kDownloadNameBytes long.
// A long name is shortened before its extension, so the file keeps its type
// and still opens in the app for it.
#include <cstddef>
#include <filesystem>
#include <string>

namespace tilo {

inline constexpr size_t kDownloadNameBytes = 180;
// Longer than this after the dot is no file type, only part of the name.
inline constexpr size_t kDownloadExtensionBytes = 16;

// The longest start of `text` at most `bytes` long that ends between
// UTF-8 characters.
inline std::string Utf8Prefix(const std::string& text, size_t bytes) {
  if (text.size() <= bytes) return text;
  size_t end = bytes;
  while (end > 0 && (static_cast<unsigned char>(text[end]) & 0xC0) == 0x80) --end;
  return text.substr(0, end);
}

inline std::string DownloadFileName(std::string name) {
  for (auto& c : name) if (c == '\\') c = '/';
  name = std::filesystem::path(name).filename().string();
  for (auto& c : name) if (static_cast<unsigned char>(c) < 32 || c == 127) c = '_';
  if (name.empty() || name == "." || name == "..") return "download";
  if (name.size() <= kDownloadNameBytes) return name;
  const auto dot = name.rfind('.');
  std::string extension;
  if (dot != std::string::npos && dot > 0 && name.size() - dot - 1 <= kDownloadExtensionBytes)
    extension = name.substr(dot);
  const auto stem = Utf8Prefix(name.substr(0, name.size() - extension.size()),
                               kDownloadNameBytes - extension.size());
  return stem.empty() ? "download" + extension : stem + extension;
}

}  // namespace tilo
