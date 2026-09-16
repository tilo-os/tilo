#pragma once
#include "blocker.h"
#include "include/cef_request_context_handler.h"
#include "include/wrapper/cef_stream_resource_handler.h"
#include "include/wrapper/cef_byte_read_handler.h"
#include <atomic>
#include <functional>

namespace qos {
inline std::string ResourceKind(cef_resource_type_t kind) {
  switch(kind) {
    case RT_MAIN_FRAME: return "document";
    case RT_SUB_FRAME: return "subdocument";
    case RT_STYLESHEET: return "stylesheet";
    case RT_SCRIPT: case RT_WORKER: case RT_SHARED_WORKER: case RT_SERVICE_WORKER: return "script";
    case RT_IMAGE: case RT_FAVICON: return "image";
    case RT_FONT_RESOURCE: return "font";
    case RT_MEDIA: return "media";
    case RT_XHR: return "xmlhttprequest";
    case RT_PING: return "ping";
    case RT_CSP_REPORT: return "csp_report";
    default: return "other";
  }
}
// Each in-flight request owns its compiled snapshot. Publishing a new snapshot
// never destroys state still being read on CEF's IO thread.
struct FilterState { std::shared_ptr<Blocker> blocker; bool enabled=true; };
class FilterResource final : public CefResourceRequestHandler {
 public:
  FilterResource(std::shared_ptr<FilterState> state,std::string source,std::function<void()> failure)
    :state_(std::move(state)),source_(std::move(source)),failure_(std::move(failure)) {}
  ReturnValue OnBeforeResourceLoad(CefRefPtr<CefBrowser>,CefRefPtr<CefFrame>,CefRefPtr<CefRequest> request,CefRefPtr<CefCallback>) override {
    const auto url=request->GetURL().ToString();
    if(!WebURL(url)) return RV_CONTINUE;
    auto result=state_->blocker->Match(url,source_,ResourceKind(request->GetResourceType()));
    if(!result) { failure_();return RV_CONTINUE; }
    const auto redirect=result->GetString("redirect").ToString();
    if(!redirect.empty()) {
      // adblock-rust redirect resources are data URLs. Serve the replacement
      // bytes on the original URL, preserving Chromium's normal origin checks.
      const auto comma=redirect.find(',');const auto marker=redirect.find(";base64,");
      if(redirect.starts_with("data:") && comma!=std::string::npos && marker!=std::string::npos) {
        auto encoded=redirect.substr(comma+1);auto data=CefBase64Decode(encoded);mime_=redirect.substr(5,marker-5);
        if(data || encoded.empty()) {
          bytes_.resize(data ? data->GetSize() : 0);
          if(data) data->GetData(bytes_.data(),bytes_.size(),0);
          redirected_=true;return RV_CONTINUE;
        }
      }
      if(WebURL(redirect)) { request->SetURL(redirect);return RV_CONTINUE; }
      failure_();return RV_CANCEL;
    }
    if(result->GetBool("blocked")) return RV_CANCEL;
    auto rewritten=result->GetString("rewritten_url").ToString();
    if(!rewritten.empty() && rewritten!=url && WebURL(rewritten)) request->SetURL(rewritten);
    return RV_CONTINUE;
  }
  CefRefPtr<CefResourceHandler> GetResourceHandler(CefRefPtr<CefBrowser>,CefRefPtr<CefFrame>,CefRefPtr<CefRequest>) override {
    if(!redirected_) return nullptr;
    // Retain the immutable byte owner for the lifetime of the stream.
    auto reader=new CefByteReadHandler(reinterpret_cast<const unsigned char*>(bytes_.data()),bytes_.size(),this);
    auto stream=CefStreamReader::CreateForHandler(reader);
    return new CefStreamResourceHandler(mime_,stream);
  }
 private:
  std::shared_ptr<FilterState> state_;std::string source_,mime_,bytes_;std::function<void()> failure_;bool redirected_=false;
  IMPLEMENT_REFCOUNTING(FilterResource);
};
class RequestFilter final : public CefRequestContextHandler {
 public:
  explicit RequestFilter(std::function<void()> failure):failure_(std::move(failure)) {
    std::atomic_store(&state_,std::make_shared<FilterState>(FilterState{std::make_shared<Blocker>(""),true}));
  }
  bool Configure(const std::string& rules,bool enabled) {
    auto blocker=std::make_shared<Blocker>(rules);if(!blocker->Valid()) return false;
    std::atomic_store(&state_,std::make_shared<FilterState>(FilterState{std::move(blocker),enabled}));return true;
  }
  CefRefPtr<CefResourceRequestHandler> GetResourceRequestHandler(CefRefPtr<CefBrowser>,CefRefPtr<CefFrame> frame,CefRefPtr<CefRequest> request,bool,bool,const CefString& initiator,bool&) override {
    auto state=std::atomic_load(&state_);
    if(!state->enabled || !WebURL(request->GetURL())) return nullptr;
    std::string source=frame ? frame->GetURL().ToString() : initiator.ToString();
    if(!WebURL(source)) source=initiator.ToString();
    if(!WebURL(source)) source=request->GetURL();
    return new FilterResource(std::move(state),std::move(source),failure_);
  }
 private:
  std::shared_ptr<FilterState> state_;std::function<void()> failure_;
  IMPLEMENT_REFCOUNTING(RequestFilter);
};
}
