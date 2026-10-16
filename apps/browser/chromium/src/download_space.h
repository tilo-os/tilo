#pragma once
// Whether a download fits on the disk it is saved to.
//
// A download must never fill the disk: the person's other work, and the
// desktop itself, need room too. Before a download starts, and again as its
// size becomes known, the free space where it is saved is checked; a
// download that would not leave kDownloadHeadroom free is refused with a
// plain-language reason instead.
#include <cstdint>
#include <optional>
#include <string>
#include <sys/statvfs.h>

namespace tilo {

inline constexpr uint64_t kDownloadHeadroom = 512ull * 1024 * 1024;

// Bytes an unprivileged writer can still store where `dir` lives, or
// nothing when the filesystem can't say.
inline std::optional<uint64_t> AvailableBytes(const std::string& dir) {
  struct statvfs stat {};
  if (statvfs(dir.c_str(), &stat) != 0) return std::nullopt;
  return static_cast<uint64_t>(stat.f_bavail) * static_cast<uint64_t>(stat.f_frsize);
}

inline std::string ReadableSize(uint64_t bytes) {
  constexpr uint64_t mb = 1000ull * 1000;
  constexpr uint64_t gb = 1000 * mb;
  if (bytes >= gb) {
    const uint64_t tenths = bytes / (gb / 10);
    return std::to_string(tenths / 10) + "." + std::to_string(tenths % 10) + " GB";
  }
  return std::to_string((bytes + mb - 1) / mb) + " MB";
}

// Why `remaining` more bytes (0 when unknown) can't be saved where only
// `available` are free, or an empty string when they fit.
inline std::string SpaceRefusal(uint64_t remaining, uint64_t available) {
  if (available < kDownloadHeadroom)
    return "Not enough space: only " + ReadableSize(available) + " is free";
  if (remaining > 0 && available - kDownloadHeadroom < remaining)
    return "Not enough space: it needs " + ReadableSize(remaining) + ", " +
           ReadableSize(available) + " is free";
  return "";
}

}  // namespace tilo
