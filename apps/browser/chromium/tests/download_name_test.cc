#include "download_name.h"
#include <cassert>
#include <string>

int main() {
  using tilo::DownloadFileName;
  // Only the last path part, without control characters.
  assert(DownloadFileName("../../fixture.txt") == "fixture.txt");
  assert(DownloadFileName("..\\..\\fixture.txt") == "fixture.txt");
  assert(DownloadFileName("a\nb.txt") == "a_b.txt");
  assert(DownloadFileName("") == "download");
  assert(DownloadFileName("..") == "download");
  // A long name is shortened before its extension: the file keeps its type.
  const std::string title(300, 'x');
  const auto shortened = DownloadFileName(title + ".pdf");
  assert(shortened.size() == tilo::kDownloadNameBytes);
  assert(shortened == std::string(tilo::kDownloadNameBytes - 4, 'x') + ".pdf");
  // A dot far from the end is part of the name, not its type.
  const auto no_type = DownloadFileName("v1." + title);
  assert(no_type.size() == tilo::kDownloadNameBytes && no_type.rfind("v1.x", 0) == 0);
  // Shortening never splits a character.
  std::string umlauts;
  for (int i = 0; i < 120; ++i) umlauts += "\xC3\xA4";  // 240 bytes of "ä"
  const auto cut = DownloadFileName(umlauts + ".txt");
  assert(cut.size() <= tilo::kDownloadNameBytes);
  assert(cut.size() % 2 == 0 && cut.substr(cut.size() - 4) == ".txt");
  assert(cut.substr(0, 2) == "\xC3\xA4" && cut[cut.size() - 6] == '\xC3');
  // A name that fits is kept as it is.
  assert(DownloadFileName("Quarterly report.pdf") == "Quarterly report.pdf");
}
