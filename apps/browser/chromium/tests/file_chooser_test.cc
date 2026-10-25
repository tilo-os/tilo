#include "file_chooser.h"
#include <cassert>
#include <cstdio>
#include <cstdlib>
#include <fstream>
#include <string>
#include <unistd.h>
#include <vector>

namespace {
std::string TempDirectory() {
  char pattern[] = "/tmp/tilo-file-chooser-XXXXXX";
  const auto* made = mkdtemp(pattern);
  assert(made);
  return made;
}

std::string Write(const std::string& directory, const std::string& name, const std::string& body = "pdf") {
  const auto path = directory + "/" + name;
  std::ofstream(path) << body;
  return path;
}
}  // namespace

int main() {
  const auto directory = TempDirectory();
  const auto pdf = Write(directory, "scan.pdf");
  const auto png = Write(directory, "photo.png");

  // The page's own accept list decides what it is offered.
  assert(tilo::ChooserAccept({CefString(".pdf")}) == ".pdf");
  assert(tilo::ChooserAccept({CefString(".pdf;.doc"), CefString(".txt")}) == ".pdf,.doc,.txt");
  assert(tilo::ChooserAccept({CefString("*")}).empty());
  assert(tilo::ChooserAccept({CefString("image/*")}) == "image/*");

  std::vector<std::string> out;
  assert(tilo::ChooserRefusal({pdf}, false, {CefString(".pdf")}, false, out).empty());
  assert(out.size() == 1 && out[0] == pdf);
  out.clear();

  // Types the chooser does not accept, missing files, folders and relative paths.
  assert(tilo::ChooserRefusal({png}, false, {CefString(".pdf")}, false, out) == "This chooser takes .pdf");
  assert(out.empty());
  assert(tilo::ChooserRefusal({directory + "/absent.pdf"}, false, {CefString(".pdf")}, false, out)
         == "No file at that path");
  assert(tilo::ChooserRefusal({directory}, false, {CefString(".pdf")}, false, out)
         == "That is a folder, not a file");
  assert(tilo::ChooserRefusal({"scan.pdf"}, false, {CefString(".pdf")}, false, out) == "Give full paths from /");
  assert(tilo::ChooserRefusal({"pdf/scan.pdf"}, false, {CefString(".pdf")}, false, out)
         == "Give full paths from /");
  assert(out.empty());

  // A single-file chooser takes one path, and one answer carries at most 16.
  assert(tilo::ChooserRefusal({pdf, png}, false, {}, false, out) == "This chooser takes one file");
  std::vector<std::string> many;
  for (int i = 0; i < 17; ++i) many.push_back(Write(directory, "many" + std::to_string(i) + ".pdf"));
  assert(tilo::ChooserRefusal(many, false, {CefString(".pdf")}, true, out) == "Too many files for one chooser");
  many.resize(16);
  out.clear();
  assert(tilo::ChooserRefusal(many, false, {CefString(".pdf")}, true, out).empty());
  assert(out.size() == 16);
  out.clear();

  // A folder chooser takes the folder, not its files.
  assert(tilo::ChooserRefusal({directory}, true, {}, false, out).empty());
  assert(out.size() == 1 && out[0] == directory);

  // A chooser with no accepted types offers every file.
  out.clear();
  assert(tilo::ChooserRefusal({png}, false, {CefString("*")}, false, out).empty());
  assert(out.size() == 1);

  std::system(("rm -rf '" + directory + "'").c_str());
  return 0;
}
