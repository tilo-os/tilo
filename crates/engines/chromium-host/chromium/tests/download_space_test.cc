#include "download_space.h"
#include <cassert>

int main() {
  constexpr uint64_t gb = 1000ull * 1000 * 1000;
  // Plenty of room: nothing to say.
  assert(tilo::SpaceRefusal(0, 50 * gb).empty());
  assert(tilo::SpaceRefusal(4 * gb, 50 * gb).empty());
  // A known size that would not leave the headroom is refused.
  assert(tilo::SpaceRefusal(3900 * 1000 * 1000ull, 2500 * 1000 * 1000ull) ==
         "Not enough space: it needs 3.9 GB, 2.5 GB is free");
  // A nearly full disk refuses even a download of unknown size.
  assert(tilo::SpaceRefusal(0, 20 * 1000 * 1000ull) == "Not enough space: only 20 MB is free");
  // Never underflows at the boundary.
  assert(!tilo::SpaceRefusal(1, tilo::kDownloadHeadroom).empty());
  assert(tilo::SpaceRefusal(0, tilo::kDownloadHeadroom).empty());
  assert(tilo::AvailableBytes("/").has_value());
  assert(!tilo::AvailableBytes("/nonexistent/path/for/space/test").has_value());
}
