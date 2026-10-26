#pragma once
#include "include/cef_browser.h"
#include "include/cef_devtools_message_observer.h"
#include "include/cef_parser.h"
#include "geolocation_script.h"
#include <cmath>
#include <cstdio>
#include <cstdlib>
#include <cstring>
#include <functional>
#include <map>
#include <optional>
#include <string>

namespace tilo {
// A position the engine can answer with.
struct Position {
  double latitude = 0, longitude = 0, accuracy = 0;
};
// The person's own location from the platform, when this machine will say:
// macOS answers through Location Services. A build running offscreen, headless
// or under a test account has no authorization, and the answer is nothing.
void SystemPosition(std::function<void(std::optional<Position>)> answer);
// The fixed position a development or test session asks for, from
// TILO_GEOLOCATION ("latitude,longitude[,accuracy]"): the deterministic
// answer a benchmark needs, and no answer at all in a person's browser
// unless the person's own platform supplies one.
inline std::optional<Position> FixedPosition() {
  const char* text = std::getenv("TILO_GEOLOCATION");
  if (!text || !*text) return std::nullopt;
  double values[3] = {0, 0, 0};
  int found = 0;
  const char* at = text;
  while (found < 3 && *at) {
    char* end = nullptr;
    const double value = std::strtod(at, &end);
    if (end == at) break;
    values[found++] = value;
    at = end;
    if (*at == ',') ++at;
  }
  if (found < 2 || std::fabs(values[0]) > 90 || std::fabs(values[1]) > 180) return std::nullopt;
  Position position;
  position.latitude = values[0];
  position.longitude = values[1];
  position.accuracy = found > 2 && values[2] > 0 ? values[2] : 25;
  return position;
}
// A page's geolocation calls, taken from the page and answered here.
//
// The wrapper geolocation.js installs reports each call to this binding; the
// site's question then reaches the app as a permission request, whose answer
// applies the position (an emulation override, the only grant this engine can
// give Chromium) and replies to the call the page waits on.
class Geolocation final : public CefDevToolsMessageObserver {
 public:
  // `ask` asks the app about `origin`'s request and answers once, with the
  // position to report or nothing when none is available.
  using Ask = std::function<void(const std::string& origin, int request)>;
  Geolocation(CefRefPtr<CefBrowser> browser, Ask ask)
      : browser_(browser), ask_(std::move(ask)) {}
  void Attach() {
    registration_ = browser_->GetHost()->AddDevToolsMessageObserver(this);
    auto host = browser_->GetHost();
    host->ExecuteDevToolsMethod(0, "Page.enable", nullptr);
    host->ExecuteDevToolsMethod(0, "Runtime.enable", nullptr);
    auto binding = CefDictionaryValue::Create();
    binding->SetString("name", "__tiloGeoReport");
    host->ExecuteDevToolsMethod(0, "Runtime.addBinding", binding);
    auto script = CefDictionaryValue::Create();
    script->SetString("source", kGeolocationScript);
    host->ExecuteDevToolsMethod(0, "Page.addScriptToEvaluateOnNewDocument", script);
    // The page already open when this began has the wrapper too.
    auto evaluate = CefDictionaryValue::Create();
    evaluate->SetString("expression", kGeolocationScript);
    host->ExecuteDevToolsMethod(0, "Runtime.evaluate", evaluate);
  }
  void Detach() {
    registration_ = nullptr;
    browser_ = nullptr;
  }
  // The site's question for `request` was answered: `position` when the person
  // allowed it and one is available, nothing when refused or unavailable.
  void Answer(const std::string& origin, int request, std::optional<Position> position) {
    if (!browser_) return;
    const auto waiting = calls_.find(request);
    if (waiting == calls_.end()) return;
    const int call = waiting->second;
    calls_.erase(waiting);
    if (position) Override(*position);
    Reply(call, position);
    // The site is answered as it was for the rest of this session, without
    // asking the person again.
    answered_[origin] = position;
  }
  void OnDevToolsEvent(CefRefPtr<CefBrowser>, const CefString& method, const void* params, size_t size) override {
    if (!browser_ || method.ToString() != "Runtime.bindingCalled") return;
    auto parsed = CefParseJSON(std::string(static_cast<const char*>(params), size), JSON_PARSER_RFC);
    auto event = parsed && parsed->GetType() == VTYPE_DICTIONARY ? parsed->GetDictionary() : nullptr;
    if (!event || event->GetString("name") != "__tiloGeoReport") return;
    auto payload = CefParseJSON(event->GetString("payload"), JSON_PARSER_RFC);
    auto call = payload && payload->GetType() == VTYPE_DICTIONARY ? payload->GetDictionary() : nullptr;
    if (!call) return;
    Asked(call->GetInt("id"));
  }

 private:
  // The page asked for a position: a site already answered once this session
  // is answered again with the same answer, and a new one is asked about.
  void Asked(int call) {
    const std::string origin = Origin();
    if (origin.empty()) { Reply(call, std::nullopt);return; }
    const auto known = answered_.find(origin);
    if (known != answered_.end()) {
      if (known->second) Override(*known->second);
      Reply(call, known->second);
      return;
    }
    const int request = ++sequence_;
    calls_[request] = call;
    ask_(origin, request);
  }
  std::string Origin() const {
    if (!browser_) return {};
    auto frame = browser_->GetMainFrame();
    if (!frame) return {};
    std::string url = frame->GetURL().ToString();
    const auto scheme = url.find("://");
    const auto slash = url.find('/', scheme == std::string::npos ? 0 : scheme + 3);
    return slash == std::string::npos ? url : url.substr(0, slash);
  }
  // Chromium's geolocation permission in this engine is decided before a page
  // hears anything, and no grant reaches it; an emulation override is the one
  // way a page with a person's answer receives a position.
  void Override(const Position& position) {
    auto params = CefDictionaryValue::Create();
    params->SetDouble("latitude", position.latitude);
    params->SetDouble("longitude", position.longitude);
    params->SetDouble("accuracy", position.accuracy);
    browser_->GetHost()->ExecuteDevToolsMethod(0, "Emulation.setGeolocationOverride", params);
  }
  // What the page's call waits on heard: whether it is allowed a position, and
  // the position when there is one to give.
  void Reply(int call, std::optional<Position> position) {
    if (!browser_) return;
    auto fields = CefDictionaryValue::Create();
    if (position) {
      fields->SetDouble("latitude", position->latitude);
      fields->SetDouble("longitude", position->longitude);
      fields->SetDouble("accuracy", position->accuracy);
    } else {
      fields->SetBool("unavailable", true);
    }
    auto value = CefValue::Create();
    value->SetDictionary(fields);
    auto expression = CefDictionaryValue::Create();
    expression->SetString(
        "expression",
        "__tiloGeoAnswer(" + std::to_string(call) + "," + (position ? "true" : "false") + "," +
            CefWriteJSON(value, JSON_WRITER_DEFAULT).ToString() + ")");
    browser_->GetHost()->ExecuteDevToolsMethod(0, "Runtime.evaluate", expression);
  }
  CefRefPtr<CefBrowser> browser_;
  CefRefPtr<CefRegistration> registration_;
  Ask ask_;
  int sequence_ = 0;
  std::map<int, int> calls_;
  std::map<std::string, std::optional<Position>> answered_;
  IMPLEMENT_REFCOUNTING(Geolocation);
};
}
