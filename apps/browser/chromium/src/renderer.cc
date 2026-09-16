#include "host.h"
#include "include/cef_browser.h"
#include "include/cef_frame.h"
#include "include/cef_parser.h"
#include "include/cef_process_message.h"
#include "include/cef_render_process_handler.h"
#include "include/cef_v8.h"
#include <map>
#include <vector>

namespace {
// Deliberately require a direct web document. Inherited about:blank/srcdoc and
// blob documents are excluded: a URL alone cannot prove their creator origin.
// This also excludes chrome:, chrome-extension:, devtools:, file: and data:.
bool IsWebDocument(CefRefPtr<CefFrame> frame) {
  CefURLParts parts;
  if (!frame || !CefParseURL(frame->GetURL(), parts)) return false;
  const auto scheme=CefString(&parts.scheme).ToString();
  return scheme=="http" || scheme=="https";
}
}

class Renderer final : public CefApp, public CefRenderProcessHandler {
 public:
  CefRefPtr<CefRenderProcessHandler> GetRenderProcessHandler() override { return this; }

  void OnContextReleased(CefRefPtr<CefBrowser>, CefRefPtr<CefFrame> frame,
                         CefRefPtr<CefV8Context> context) override {
    auto found=applied_.find(frame->GetIdentifier().ToString());
    if(found!=applied_.end() && found->second.context->IsSame(context)) applied_.erase(found);
  }

  bool OnProcessMessageReceived(CefRefPtr<CefBrowser>, CefRefPtr<CefFrame> frame,
                                CefProcessId source,
                                CefRefPtr<CefProcessMessage> message) override {
    if(source!=PID_BROWSER || message->GetName()!="qos.scripts") return false;
    if(!IsWebDocument(frame)) return true;
    auto context=frame->GetV8Context();
    if(!context || !context->IsValid()) return true;
    std::vector<CefString> scripts;
    auto args=message->GetArgumentList();
    for(size_t i=0;i<args->GetSize();++i) scripts.push_back(args->GetString(i));

    auto& applied=applied_[frame->GetIdentifier().ToString()];
    if(applied.context && applied.context->IsSame(context) && applied.scripts==scripts) return true;
    applied={context,scripts};
    // The browser synchronizes configuration on load completion and on explicit
    // updates. The V8 context identifies the document, so reload/site-isolation
    // creates a fresh execution while repeated synchronization is harmless.
    for(const auto& script:scripts) {
      if(!IsWebDocument(frame)) break;
      frame->ExecuteJavaScript(script,frame->GetURL(),0);
    }
    return true;
  }
 private:
  struct Applied { CefRefPtr<CefV8Context> context; std::vector<CefString> scripts; };
  std::map<std::string,Applied> applied_;
  IMPLEMENT_REFCOUNTING(Renderer);
};
CefRefPtr<CefApp> MakeRendererApp() { return new Renderer; }
