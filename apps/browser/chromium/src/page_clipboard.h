#pragma once
// The page's clipboard where the engine has none (README, "Page clipboard").
//
// On Linux every page renders offscreen with no display server, and
// Chromium's clipboard there lives in the engine alone: what a page copies
// never leaves it, and what the person copied elsewhere never reaches it.
// The renderer runs page_clipboard.js in each document, which sends what the
// page copies to the host and asks the host for what it pastes and reads.
// The host sends a copy to the Browser as a `clipboard` event, which puts it
// on the system clipboard, and asks the Browser for the system clipboard's
// text with `clipboard_read`, answered by `clipboard_contents`.
#include <cstddef>
#include <map>
#include <optional>
#include <string>
#include "page_clipboard_script.h"

namespace tilo {

// The most a copy carries: its text, and its markup, which is dropped from
// a copy that has more and never truncated.
inline constexpr size_t kMaxClipboardText = 4 * 1024 * 1024;
inline constexpr size_t kMaxClipboardHtml = 1024 * 1024;
// How long the host waits on the Browser for the system clipboard, and on
// the renderer to keep it for a paste, before the page goes on without it.
inline constexpr int kClipboardReadMs = 3000;
inline constexpr int kClipboardPrimeMs = 500;
// The renderer process runs page_clipboard.js only with this switch, which
// the host gives it where the engine has no clipboard of its own.
inline constexpr char kPageClipboardSwitch[] = "tilo-page-clipboard";

// A key Chromium pastes on: Ctrl+V (Ctrl+Shift+V pastes the words alone)
// and Shift+Insert.
inline bool PasteKey(const std::string& key, bool ctrl, bool shift, bool alt, bool meta) {
  if (alt || meta) return false;
  if (ctrl) return key == "v" || key == "V";
  return shift && key == "Insert";
}

// Reads of the system clipboard the host asked the Browser for, by the id
// it asked with: a paste a key started in a tab, or a page's read
// (`navigator.clipboard.readText`) from one of a tab's frames.
class ClipboardReads {
 public:
  enum class Purpose { Paste, Read };
  struct Read {
    Purpose purpose;
    int tab;
    // For a read: the frame and the renderer's own id for it.
    std::string frame;
    int renderer = 0;
  };
  static constexpr size_t kMaxReads = 64;
  // The id to ask with, or 0 when too many reads are waiting.
  int Add(Read read) {
    if (reads_.size() >= kMaxReads) return 0;
    next_ = next_ % 2147483647 + 1;
    reads_[next_] = std::move(read);
    return next_;
  }
  std::optional<Read> Take(int id) {
    auto found = reads_.find(id);
    if (found == reads_.end()) return std::nullopt;
    auto read = std::move(found->second);
    reads_.erase(found);
    return read;
  }

 private:
  std::map<int, Read> reads_;
  int next_ = 0;
};

}  // namespace tilo
