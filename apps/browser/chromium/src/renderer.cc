#include "host.h"
#include "cosmetics.h"
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

  void OnContextCreated(CefRefPtr<CefBrowser> browser,CefRefPtr<CefFrame> frame,CefRefPtr<CefV8Context>) override {
    if(!IsWebDocument(frame)) return;
    auto& config=configs_[browser->GetIdentifier()];
    if(config.filters) ApplyFilters(frame,config.filters);
    frame->SendProcessMessage(PID_BROWSER,CefProcessMessage::Create("qos.config"));
  }
  void OnContextReleased(CefRefPtr<CefBrowser>, CefRefPtr<CefFrame> frame,
                         CefRefPtr<CefV8Context> context) override {
    auto key=frame->GetIdentifier().ToString();
    auto cosmetic=cosmetics_.find(key);
    if(cosmetic!=cosmetics_.end() && cosmetic->second.context->IsSame(context)) cosmetics_.erase(cosmetic);
    auto found=applied_.find(key);
    if(found!=applied_.end() && found->second.context->IsSame(context)) applied_.erase(found);
  }

  bool OnProcessMessageReceived(CefRefPtr<CefBrowser> browser,CefRefPtr<CefFrame> frame,
                                CefProcessId source,CefRefPtr<CefProcessMessage> message) override {
    if(source!=PID_BROWSER) return false;
    auto& config=configs_[browser->GetIdentifier()];
    if(message->GetName()=="qos.filters") {
      config.filters=message->GetArgumentList()->Copy();ApplyFilters(frame,config.filters);return true;
    }
    if(message->GetName()!="qos.scripts") return false;
    config.scripts.clear();auto args=message->GetArgumentList();
    for(size_t i=0;i<args->GetSize();++i) config.scripts.push_back(args->GetString(i));
    ApplyScripts(frame,config.scripts);return true;
  }
 private:
  void ApplyScripts(CefRefPtr<CefFrame> frame,const std::vector<CefString>& scripts) {
    if(!IsWebDocument(frame)) return;
    auto context=frame->GetV8Context();if(!context || !context->IsValid()) return;
    auto& applied=applied_[frame->GetIdentifier().ToString()];
    if(applied.context && applied.context->IsSame(context) && applied.scripts==scripts) return;
    applied={context,scripts};
    // Eval is synchronous in OnContextCreated: cached configuration runs before
    // parser scripts. Queued frame configuration covers new renderer processes.
    context->Enter();
    for(const auto& script:scripts) {
      if(!IsWebDocument(frame)) break;
      CefRefPtr<CefV8Value> result;CefRefPtr<CefV8Exception> exception;
      context->Eval(script,frame->GetURL(),0,result,exception);
    }
    context->Exit();
  }
  struct Config { CefRefPtr<CefListValue> filters;std::vector<CefString> scripts; };
  std::map<int,Config> configs_;
  void ApplyFilters(CefRefPtr<CefFrame> frame,CefRefPtr<CefListValue> args) {
    if(!IsWebDocument(frame)) return;
    auto context=frame->GetV8Context();if(!context || !context->IsValid()) return;
    const auto rules=args->GetString(0).ToString();const bool enabled=args->GetBool(1);
    auto& applied=cosmetics_[frame->GetIdentifier().ToString()];
    if(applied.context && applied.context->IsSame(context) && applied.rules==rules && applied.enabled==enabled) return;
    context->Enter();
    if(applied.context && applied.context->IsSame(context) && applied.cleanup) applied.cleanup->ExecuteFunction(nullptr,{});
    applied={context,nullptr,rules,enabled};
    if(enabled) {
      if(!blocker_ || rules!=rules_) { blocker_=std::make_shared<qos::Blocker>(rules);rules_=rules; }
      if(blocker_->Valid()) {
        CefRefPtr<CefV8Value> factory;CefRefPtr<CefV8Exception> exception;
        auto resources=blocker_->Cosmetics(frame->GetURL(),"[]","[]");
        if(resources) {
          const auto script=resources->GetString("injected_script").ToString();
          if(!script.empty()) context->Eval("(function(){const scriptletGlobals={};\n"+script+"\n})()",frame->GetURL(),0,factory,exception);
        }
        if(context->Eval(qos::kCosmetics,frame->GetURL(),0,factory,exception) && factory && factory->IsFunction()) {
          auto query=CefV8Value::CreateFunction("query",new qos::CosmeticQuery(blocker_,frame->GetURL()));
          applied.cleanup=factory->ExecuteFunction(nullptr,{query});
        }
      }
    }
    context->Exit();
  }
  struct CosmeticApplied { CefRefPtr<CefV8Context> context;CefRefPtr<CefV8Value> cleanup;std::string rules;bool enabled; };
  std::map<std::string,CosmeticApplied> cosmetics_;
  std::shared_ptr<qos::Blocker> blocker_;std::string rules_;
  struct Applied { CefRefPtr<CefV8Context> context; std::vector<CefString> scripts; };
  std::map<std::string,Applied> applied_;
  IMPLEMENT_REFCOUNTING(Renderer);
};
CefRefPtr<CefApp> MakeRendererApp() { return new Renderer; }
