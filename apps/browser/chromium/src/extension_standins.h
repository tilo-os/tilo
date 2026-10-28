#pragma once
// An ad blocker's stand-ins at its address for the session.
//
// An ad blocker puts a harmless script of its own in place of a page's ad or
// analytics script (a declarativeNetRequest redirect to one of its files):
// the page's calls to the API it stubs then do nothing rather than throw.
// uBlock Origin Lite's files are web accessible only at an address of its own
// for the session (`use_dynamic_url`), chrome-extension://<GUID>/<path>.
// Chromium answers that address with a redirect to the extension's own,
// chrome-extension://<id>/<path>, and lets the file load there because it
// remembers the address it came from. CEF starts each redirected request
// anew instead (its request proxy restarts the job to let the embedder
// intercept it), so Chromium forgets, sees the file asked for at the
// extension's own address and refuses it (net::ERR_BLOCKED_BY_CLIENT): the
// stand-in never runs.
//
// The host answers that second request itself, with the file, when it came
// from the session's address and the extension's manifest makes the file web
// accessible to the page that asked, as Chromium would have. Which extension
// the session's address names is what Chromium's own redirect says; where its
// files are is what Chromium says of it (`Extensions.getExtensions`).
#include <algorithm>
#include <cctype>
#include <cstdint>
#include <cstring>
#include <filesystem>
#include <fstream>
#include <functional>
#include <map>
#include <memory>
#include <mutex>
#include <optional>
#include <sstream>
#include <string>
#include <vector>
#include "include/base/cef_callback.h"
#include "include/cef_parser.h"
#include "include/cef_resource_handler.h"
#include "include/cef_resource_request_handler.h"
#include "include/cef_task.h"
#include "include/wrapper/cef_closure_task.h"
#include "match_pattern.h"

namespace tilo {

// Chromium's address of an extension for the session: a GUID.
inline bool SessionExtensionHost(const std::string& host) {
  if (host.size() != 36) return false;
  for (size_t i = 0; i < host.size(); ++i) {
    const bool dash = i == 8 || i == 13 || i == 18 || i == 23;
    if (dash ? host[i] != '-' : !std::isxdigit(static_cast<unsigned char>(host[i]))) return false;
  }
  return true;
}
// An extension's id, its own address: 32 letters a to p.
inline bool OwnExtensionHost(const std::string& host) {
  if (host.size() != 32) return false;
  for (char c : host) if (c < 'a' || c > 'p') return false;
  return true;
}
// The host and path of a chrome-extension:// address, or nothing.
struct ExtensionAddress { std::string host, path; };
inline std::optional<ExtensionAddress> ExtensionAddressOf(const std::string& url) {
  CefURLParts parts;
  if (!CefParseURL(url, parts) || CefString(&parts.scheme).ToString() != "chrome-extension") return std::nullopt;
  ExtensionAddress address{CefString(&parts.host).ToString(), CefString(&parts.path).ToString()};
  if (address.path.empty()) address.path = "/";
  return address;
}

// Whether the manifest makes the file at `path` (as in its address, from
// its root) web accessible to a page of `initiator` (an origin), as Chromium
// decides for a file asked for at the session's address
// (WebAccessibleResourcesInfo): the first entry whose resources name it
// says, by its `matches`. A Manifest V2 list makes it accessible to any page.
inline bool WebAccessible(CefRefPtr<CefDictionaryValue> manifest, const std::string& path, const std::string& initiator) {
  auto entries = manifest && manifest->GetType("web_accessible_resources") == VTYPE_LIST ? manifest->GetList("web_accessible_resources") : nullptr;
  if (!entries) return false;
  auto names = [&path](CefRefPtr<CefListValue> resources) {
    for (size_t i = 0; resources && i < resources->GetSize(); ++i) {
      if (resources->GetType(i) != VTYPE_STRING) continue;
      auto pattern = resources->GetString(i).ToString();
      if (pattern.empty()) continue;
      if (pattern[0] != '/') pattern.insert(0, "/");
      if (GlobMatch(pattern, path)) return true;
    }
    return false;
  };
  const std::string page = initiator + "/";
  for (size_t i = 0; i < entries->GetSize(); ++i) {
    if (entries->GetType(i) == VTYPE_STRING) {
      auto single = CefListValue::Create();
      single->SetString(0, entries->GetString(i));
      if (names(single)) return true;
      continue;
    }
    if (entries->GetType(i) != VTYPE_DICTIONARY) continue;
    auto entry = entries->GetDictionary(i);
    if (!names(entry->GetType("resources") == VTYPE_LIST ? entry->GetList("resources") : nullptr)) continue;
    auto matches = entry->GetType("matches") == VTYPE_LIST ? entry->GetList("matches") : nullptr;
    for (size_t j = 0; matches && j < matches->GetSize(); ++j)
      if (matches->GetType(j) == VTYPE_STRING && MatchPattern(matches->GetString(j).ToString(), page)) return true;
  }
  return false;
}

// The file at `path` (as in its address) under the extension's directory,
// or nothing when it is not a file there: no step out of it, by `..` or by
// a link.
inline std::optional<std::filesystem::path> ExtensionFile(const std::string& directory, const std::string& path) {
  const auto relative = CefURIDecode(path, true, static_cast<cef_uri_unescape_rule_t>(UU_SPACES | UU_URL_SPECIAL_CHARS_EXCEPT_PATH_SEPARATORS)).ToString();
  if (relative.empty() || relative[0] != '/' || relative.find('\\') != std::string::npos || relative.find('\0') != std::string::npos) return std::nullopt;
  std::error_code error;
  const auto root = std::filesystem::canonical(directory, error);
  if (error) return std::nullopt;
  const auto file = std::filesystem::canonical(root / relative.substr(1), error);
  if (error || !std::filesystem::is_regular_file(file, error)) return std::nullopt;
  const auto [end, rest] = std::mismatch(root.begin(), root.end(), file.begin(), file.end());
  if (end != root.end() || rest == file.end()) return std::nullopt;
  return file;
}

// Where each extension's files are, as Chromium says, asked again when one
// is not known (one added since) or its files are not where it said.
class ExtensionDirectories {
 public:
  using Answer = std::function<void(std::map<std::string, std::string>)>;
  // `ask` runs on the UI thread and answers with every extension's
  // directory, by id; `done` may be called on any thread.
  explicit ExtensionDirectories(std::function<void(Answer)> ask) : ask_(std::move(ask)) {}
  void Find(const std::string& id, bool fresh, std::function<void(std::string)> done) {
    if (!fresh) {
      std::lock_guard lock(state_->mutex);
      auto known = state_->directories.find(id);
      if (known != state_->directories.end()) { done(known->second); return; }
    }
    CefPostTask(TID_UI, base::BindOnce([](std::shared_ptr<State> state, std::function<void(Answer)> ask, std::string id, std::function<void(std::string)> done) {
      ask([state, id, done](std::map<std::string, std::string> directories) {
        std::string found;
        {
          std::lock_guard lock(state->mutex);
          state->directories = std::move(directories);
          auto known = state->directories.find(id);
          if (known != state->directories.end()) found = known->second;
        }
        done(found);
      });
    }, state_, ask_, id, std::move(done)));
  }

 private:
  struct State { std::mutex mutex; std::map<std::string, std::string> directories; };
  std::shared_ptr<State> state_ = std::make_shared<State>();
  std::function<void(Answer)> ask_;
};

// The stand-in file, read off the IO thread once Chromium said where the
// extension's files are, and refused as Chromium refuses one that is not
// web accessible to the page.
class StandInFile final : public CefResourceHandler {
 public:
  StandInFile(std::shared_ptr<ExtensionDirectories> directories, std::string id, std::string path, std::string initiator)
      : directories_(std::move(directories)), id_(std::move(id)), path_(std::move(path)), initiator_(std::move(initiator)) {}
  bool Open(CefRefPtr<CefRequest>, bool& handle_request, CefRefPtr<CefCallback> callback) override {
    handle_request = false;
    Look(false, callback);
    return true;
  }
  void GetResponseHeaders(CefRefPtr<CefResponse> response, int64_t& length, CefString&) override {
    if (!found_) { response->SetError(ERR_BLOCKED_BY_CLIENT); length = 0; return; }
    response->SetStatus(200);
    response->SetStatusText("OK");
    response->SetMimeType(mime_);
    CefResponse::HeaderMap headers;
    // As Chromium answers for a web accessible file.
    headers.insert({"Access-Control-Allow-Origin", "*"});
    headers.insert({"Cache-Control", "no-cache"});
    response->SetHeaderMap(headers);
    length = static_cast<int64_t>(bytes_.size());
  }
  bool Read(void* out, int wanted, int& read, CefRefPtr<CefResourceReadCallback>) override {
    read = 0;
    if (offset_ >= bytes_.size()) return false;
    read = static_cast<int>(std::min<size_t>(static_cast<size_t>(wanted), bytes_.size() - offset_));
    std::memcpy(out, bytes_.data() + offset_, static_cast<size_t>(read));
    offset_ += static_cast<size_t>(read);
    return true;
  }
  void Cancel() override {}

 private:
  // A stand-in is small; this bounds what a page can make the host read.
  static constexpr std::uintmax_t kMaxFile = 16u << 20;
  void Look(bool fresh, CefRefPtr<CefCallback> callback) {
    CefRefPtr<StandInFile> self(this);
    directories_->Find(id_, fresh, [self, fresh, callback](std::string directory) {
      CefPostTask(TID_FILE_USER_VISIBLE, base::BindOnce([](CefRefPtr<StandInFile> self, bool fresh, std::string directory, CefRefPtr<CefCallback> callback) {
        if (self->Load(directory) || fresh || directory.empty()) { callback->Continue(); return; }
        // Not where Chromium said: it may have moved (an update).
        self->Look(true, callback);
      }, self, fresh, std::move(directory), callback));
    });
  }
  bool Load(const std::string& directory) {
    if (directory.empty()) return false;
    auto manifest = ReadFile(std::filesystem::path(directory) / "manifest.json");
    auto parsed = manifest ? CefParseJSON(*manifest, JSON_PARSER_ALLOW_TRAILING_COMMAS) : nullptr;
    if (!parsed || parsed->GetType() != VTYPE_DICTIONARY) return false;
    // Found: whether it may load is settled now, as Chromium settles it.
    if (!WebAccessible(parsed->GetDictionary(), path_, initiator_)) return true;
    auto file = ExtensionFile(directory, path_);
    auto bytes = file ? ReadFile(*file) : std::nullopt;
    if (!bytes) return false;
    bytes_ = std::move(*bytes);
    auto extension = file->extension().string();
    if (!extension.empty()) extension.erase(0, 1);
    mime_ = extension == "js" || extension == "mjs" ? "text/javascript" : CefGetMimeType(extension).ToString();
    if (mime_.empty()) mime_ = "application/octet-stream";
    found_ = true;
    return true;
  }
  static std::optional<std::string> ReadFile(const std::filesystem::path& file) {
    std::error_code error;
    const auto size = std::filesystem::file_size(file, error);
    if (error || size > kMaxFile) return std::nullopt;
    std::ifstream stream(file, std::ios::binary);
    if (!stream) return std::nullopt;
    std::ostringstream bytes;
    bytes << stream.rdbuf();
    return std::move(bytes).str();
  }
  std::shared_ptr<ExtensionDirectories> directories_;
  std::string id_, path_, initiator_, mime_, bytes_;
  size_t offset_ = 0;
  bool found_ = false;
  IMPLEMENT_REFCOUNTING(StandInFile);
};

// Requests to an extension's address for the session, and the request each
// becomes at the extension's own address, which the host answers.
class StandIns {
 public:
  explicit StandIns(std::shared_ptr<ExtensionDirectories> directories) : directories_(std::move(directories)) {}
  // The redirect Chromium answered request `request` at the session's
  // address with, from a page of `initiator`.
  void Redirected(uint64_t request, const std::string& to, const std::string& initiator) {
    auto address = ExtensionAddressOf(to);
    if (!address || !OwnExtensionHost(address->host)) return;
    std::lock_guard lock(mutex_);
    // One the page left before it went on is dropped with the oldest.
    if (redirected_.size() >= 256) redirected_.erase(redirected_.begin());
    redirected_[request] = Redirect{address->host, address->path, initiator};
  }
  // Whether request `request` is one a request to the session's address
  // became.
  bool Expects(uint64_t request) {
    std::lock_guard lock(mutex_);
    return redirected_.count(request) > 0;
  }
  // The file for request `request` at `url`, when it is the one a request to
  // the session's address became; nothing otherwise, and Chromium answers.
  CefRefPtr<CefResourceHandler> Answer(uint64_t request, const std::string& url) {
    auto address = ExtensionAddressOf(url);
    if (!address) return nullptr;
    Redirect redirect;
    {
      std::lock_guard lock(mutex_);
      auto found = redirected_.find(request);
      if (found == redirected_.end()) return nullptr;
      redirect = std::move(found->second);
      redirected_.erase(found);
    }
    if (redirect.id != address->host || redirect.path != address->path) return nullptr;
    return new StandInFile(directories_, redirect.id, redirect.path, redirect.initiator);
  }

 private:
  struct Redirect { std::string id, path, initiator; };
  std::shared_ptr<ExtensionDirectories> directories_;
  std::mutex mutex_;
  std::map<uint64_t, Redirect> redirected_;
};

// A request to an extension's address for the session: Chromium's redirect
// is noted, and the request it becomes answered.
class StandInRequest final : public CefResourceRequestHandler {
 public:
  StandInRequest(std::shared_ptr<StandIns> stand_ins, std::string initiator) : stand_ins_(std::move(stand_ins)), initiator_(std::move(initiator)) {}
  void OnResourceRedirect(CefRefPtr<CefBrowser>, CefRefPtr<CefFrame>, CefRefPtr<CefRequest> request, CefRefPtr<CefResponse>, CefString& new_url) override {
    stand_ins_->Redirected(request->GetIdentifier(), new_url.ToString(), initiator_);
  }
  CefRefPtr<CefResourceHandler> GetResourceHandler(CefRefPtr<CefBrowser>, CefRefPtr<CefFrame>, CefRefPtr<CefRequest> request) override {
    return stand_ins_->Answer(request->GetIdentifier(), request->GetURL().ToString());
  }

 private:
  std::shared_ptr<StandIns> stand_ins_;
  std::string initiator_;
  IMPLEMENT_REFCOUNTING(StandInRequest);
};

}  // namespace tilo
