#pragma once
#include "blocker.h"
#include "extension_standins.h"
#include "site_policy.h"
#include <map>
#include <set>
#include <mutex>
#include "include/cef_request_context_handler.h"
#include "include/wrapper/cef_stream_resource_handler.h"
#include "include/wrapper/cef_byte_read_handler.h"
#include <atomic>
#include <functional>

namespace tilo {
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
struct FilterState { std::shared_ptr<Blocker> blocker; bool enabled=true; std::string rules;std::set<std::string> exceptions; };
// IO callbacks retain only this document's counter; replacing/removing an owner
// leaves late callbacks isolated from the next document and from Client lifetime.
struct SiteDocument { std::string origin;uint64_t generation;std::atomic<uint64_t> blocked{0};SiteDocument(std::string o,uint64_t g):origin(std::move(o)),generation(g) {} };
class FilterResource final : public CefResourceRequestHandler {
 public:
  FilterResource(std::shared_ptr<FilterState> state,std::string source,std::function<void()> failure,bool fail_closed,std::shared_ptr<SiteDocument> document)
    :state_(std::move(state)),source_(std::move(source)),failure_(std::move(failure)),fail_closed_(fail_closed),document_(std::move(document)) {}
  ReturnValue OnBeforeResourceLoad(CefRefPtr<CefBrowser>,CefRefPtr<CefFrame>,CefRefPtr<CefRequest> request,CefRefPtr<CefCallback>) override {
    const auto url=request->GetURL().ToString();
    if(!WebURL(url)) return RV_CONTINUE;
    auto result=state_->blocker->Match(url,source_,ResourceKind(request->GetResourceType()));
    if(!result) { failure_();return fail_closed_?RV_CANCEL:RV_CONTINUE; }
    const auto redirect=result->GetString("redirect").ToString();
    if(document_ && (result->GetBool("blocked") || !redirect.empty())) document_->blocked.fetch_add(1,std::memory_order_relaxed);
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
  std::shared_ptr<SiteDocument> document_;
  std::shared_ptr<FilterState> state_;std::string source_,mime_,bytes_;std::function<void()> failure_;bool fail_closed_;bool redirected_=false;
  IMPLEMENT_REFCOUNTING(FilterResource);
};
class RequestFilter final : public CefRequestContextHandler {
 public:
  // A filter that fails closed refuses a request its matcher could not check.
  RequestFilter(std::function<void()> failure,bool fail_closed):failure_(std::move(failure)),fail_closed_(fail_closed) {
    std::atomic_store(&state_,std::make_shared<FilterState>(FilterState{std::make_shared<Blocker>(""),true}));
  }
  bool Configure(const std::string& rules,bool enabled) {
    auto next=std::make_shared<FilterState>(*Snapshot());
    if(!rules.empty()) { auto blocker=std::make_shared<Blocker>(rules);if(!blocker->Valid()) return false;next->blocker=std::move(blocker);next->rules=rules; }
    next->enabled=enabled;std::atomic_store(&state_,next);return true;
  }
  std::shared_ptr<FilterState> Snapshot() const { return std::atomic_load(&state_); }
  void Exceptions(const std::set<std::string>& origins) {auto next=std::make_shared<FilterState>(*Snapshot());next->exceptions=origins;std::atomic_store(&state_,next);}
  void Begin(int browser,const std::string& url) {std::lock_guard<std::mutex> lock(owners_mutex_);owners_[browser]=std::make_shared<SiteDocument>(Origin(url),++generation_);}
  void Remove(int browser) {std::lock_guard<std::mutex> lock(owners_mutex_);owners_.erase(browser);}
  std::shared_ptr<SiteDocument> Document(int browser) {std::lock_guard<std::mutex> lock(owners_mutex_);auto it=owners_.find(browser);return it==owners_.end()?nullptr:it->second;}
  // Set once, before the first request (extension_standins.h).
  void ServeStandIns(std::shared_ptr<StandIns> stand_ins) {stand_ins_=std::move(stand_ins);}
  CefRefPtr<CefResourceRequestHandler> GetResourceRequestHandler(CefRefPtr<CefBrowser> browser,CefRefPtr<CefFrame> frame,CefRefPtr<CefRequest> request,bool navigation,bool,const CefString& initiator,bool&) override {
    // An ad blocker's stand-in at its address for the session, and the
    // request that becomes at its own address: the host answers it.
    if(stand_ins_ && !navigation) {
      if(auto address=ExtensionAddressOf(request->GetURL())) {
        if(SessionExtensionHost(address->host) || (OwnExtensionHost(address->host) && stand_ins_->Expects(request->GetIdentifier())))
          return new StandInRequest(stand_ins_,initiator.ToString());
        return nullptr;
      }
    }
    auto state=std::atomic_load(&state_);
    if(!state->enabled || !WebURL(request->GetURL())) return nullptr;
    auto document=browser && frame?Document(browser->GetIdentifier()):nullptr;
    auto top=document?document->origin:std::string();
    if(request->GetResourceType()==RT_MAIN_FRAME) top=Origin(request->GetURL());
    if(!top.empty() && state->exceptions.count(top)) return nullptr;
    // For subdocument navigation frame->GetURL() may already be the target.
    // The initiator identifies the embedding document and preserves its party
    // and domain context; worker requests also use this origin without a frame.
    std::string source=initiator.ToString();
    if(!WebURL(source) && frame && request->GetResourceType()==RT_SUB_FRAME) {
      auto parent=frame->GetParent();if(parent) source=parent->GetURL();
    }
    if(!WebURL(source) && frame) source=frame->GetURL();
    if(!WebURL(source)) source=request->GetURL();
    return new FilterResource(std::move(state),std::move(source),failure_,fail_closed_,std::move(document));
  }
 private:
  std::shared_ptr<FilterState> state_;std::function<void()> failure_;bool fail_closed_;
  std::shared_ptr<StandIns> stand_ins_;
  std::mutex owners_mutex_;std::map<int,std::shared_ptr<SiteDocument>> owners_;uint64_t generation_=0;
  IMPLEMENT_REFCOUNTING(RequestFilter);
};
}
