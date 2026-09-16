#include "host.h"
#include "include/cef_browser.h"
#include "include/cef_frame.h"
#include "include/cef_render_process_handler.h"
#include "include/cef_process_message.h"
#include "include/cef_v8.h"
#include <vector>
class Renderer final : public CefApp, public CefRenderProcessHandler {
 public:
  CefRefPtr<CefRenderProcessHandler> GetRenderProcessHandler() override { return this; }
  void OnContextCreated(CefRefPtr<CefBrowser>, CefRefPtr<CefFrame> frame, CefRefPtr<CefV8Context>) override {
    for(const auto& script:scripts_) frame->ExecuteJavaScript(script,frame->GetURL(),0);
  }
  bool OnProcessMessageReceived(CefRefPtr<CefBrowser> browser,CefRefPtr<CefFrame>,CefProcessId source,CefRefPtr<CefProcessMessage> message) override {
    if(source!=PID_BROWSER || message->GetName()!="qos.scripts") return false;
    scripts_.clear(); auto args=message->GetArgumentList();
    for(size_t i=0;i<args->GetSize();++i) scripts_.push_back(args->GetString(i));
    for(const auto& script:scripts_) browser->GetMainFrame()->ExecuteJavaScript(script,browser->GetMainFrame()->GetURL(),0);
    return true;
  }
 private:
  std::vector<CefString> scripts_;
  IMPLEMENT_REFCOUNTING(Renderer);
};
CefRefPtr<CefApp> MakeRendererApp() { return new Renderer; }
