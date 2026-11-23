#pragma once
#include "include/cef_cookie.h"
#include "include/cef_browser.h"
#include "include/cef_devtools_message_observer.h"
#include "include/cef_parser.h"
#include "include/cef_scheme.h"
#include "include/cef_stream.h"
#include "include/wrapper/cef_stream_resource_handler.h"
#include "include/wrapper/cef_closure_task.h"
#include "include/base/cef_callback.h"
#include "include/cef_values.h"
#include "include/internal/cef_time.h"
#include <algorithm>
#include <cmath>
#include <functional>
#include <cstdio>
#include <set>
#include <string>
#include <tuple>

namespace tilo {
inline void WipeServiceTransport(std::string& value) {
  volatile char* bytes=value.data();for(size_t i=0;i<value.size();++i)bytes[i]=0;
}
inline void ForgetServiceCookies(CefRefPtr<CefDictionaryValue> command) {
  for(const char* key:{"cookies","origin_storage"})if(command->GetType(key)==VTYPE_LIST) {
    auto entries=command->GetList(key);
    for(size_t i=0;i<entries->GetSize();++i)if(entries->GetType(i)==VTYPE_DICTIONARY)entries->GetDictionary(i)->SetString("value","");
  }
}
inline bool ServiceSessionId(const std::string& id) {
  return !id.empty() && id.size()<=96 && std::all_of(id.begin(),id.end(),[](unsigned char c){
    return (c>='a' && c<='z') || (c>='A' && c<='Z') || (c>='0' && c<='9') || c=='-' || c=='_';
  });
}
inline bool ServiceCookieHost(const std::string& host,const std::string& service) {
  if(host.empty() || host.size()>253)return false;
  size_t start=0;
  while(start<host.size()) {
    const auto end=host.find('.',start);const auto length=(end==std::string::npos?host.size():end)-start;
    if(!length || length>63 || host[start]=='-' || host[start+length-1]=='-')return false;
    for(size_t i=start;i<start+length;++i)if(!((host[i]>='a' && host[i]<='z') || (host[i]>='0' && host[i]<='9') || host[i]=='-'))return false;
    if(end==std::string::npos)break;start=end+1;if(start==host.size())return false;
  }
  const std::string base=service=="slack"?"slack.com":service=="discord"?"discord.com":"";
  return !base.empty() && (host==base || (host.size()>base.size() && host.compare(host.size()-base.size(),base.size(),base)==0 && host[host.size()-base.size()-1]=='.'));
}
// The private command has no extension fields: source-specific or partitioned
// storage must be normalized explicitly before it can reach this boundary.
inline bool ServiceSessionCommandValid(CefRefPtr<CefDictionaryValue> d) {
  if(d->GetType("request_id")!=VTYPE_INT || d->GetInt("request_id")<=0 ||
     d->GetType("session_id")!=VTYPE_STRING || !ServiceSessionId(d->GetString("session_id").ToString()) ||
     d->GetType("service")!=VTYPE_STRING || d->GetType("cookies")!=VTYPE_LIST)return false;
  const auto service=d->GetString("service").ToString();if(service!="slack" && service!="discord")return false;
  CefDictionaryValue::KeyList fields;d->GetKeys(fields);
  for(const auto& field:fields)if(field!="op" && field!="tab" && field!="request_id" && field!="session_id" && field!="service" && field!="cookies" && field!="origin_storage" && field!="verify_only")return false;
  if(d->HasKey("verify_only") && d->GetType("verify_only")!=VTYPE_BOOL)return false;
  size_t origin_count=0,origin_bytes=0;
  if(d->HasKey("origin_storage")) {
    if(d->GetType("origin_storage")!=VTYPE_LIST)return false;auto origins=d->GetList("origin_storage");origin_count=origins->GetSize();
    if(origin_count>1 || (origin_count && service!="discord"))return false;
    if(origin_count) {
      if(origins->GetType(0)!=VTYPE_DICTIONARY)return false;auto entry=origins->GetDictionary(0);
      CefDictionaryValue::KeyList names;entry->GetKeys(names);if(names.size()!=3)return false;
      for(const auto& key:names)if(key!="origin" && key!="key" && key!="value")return false;
      for(const char* key:{"origin","key","value"})if(entry->GetType(key)!=VTYPE_STRING)return false;
      if(entry->GetString("origin")!="https://discord.com" || entry->GetString("key")!="token")return false;
      auto secret=entry->GetString("value").ToString();const bool valid=!secret.empty() && secret.size()<=16384;
      origin_bytes=secret.size();WipeServiceTransport(secret);if(!valid)return false;
    }
  }
  auto cookies=d->GetList("cookies");if((!cookies->GetSize() && !origin_count) || cookies->GetSize()>1024)return false;
  std::set<std::tuple<std::string,bool,std::string,std::string>> keys;size_t bytes=origin_bytes;
  const double now=static_cast<double>(cef_basetime_now().val)/1000000.0-11644473600.0;
  for(size_t i=0;i<cookies->GetSize();++i) {
    if(cookies->GetType(i)!=VTYPE_DICTIONARY)return false;auto c=cookies->GetDictionary(i);
    CefDictionaryValue::KeyList names;c->GetKeys(names);if(names.size()!=10)return false;
    for(const auto& field:names)if(field!="host" && field!="domain" && field!="host_only" && field!="name" && field!="value" && field!="path" && field!="expires_unix_seconds" && field!="secure" && field!="http_only" && field!="same_site")return false;
    for(const char* key:{"host","domain","name","value","path","same_site"})if(c->GetType(key)!=VTYPE_STRING)return false;
    for(const char* key:{"host_only","secure","http_only"})if(c->GetType(key)!=VTYPE_BOOL)return false;
    const auto host=c->GetString("host").ToString(),domain=c->GetString("domain").ToString();
    const auto name=c->GetString("name").ToString(),path=c->GetString("path").ToString();
    auto value=c->GetString("value").ToString();
    const auto same=c->GetString("same_site").ToString();
    const bool host_only=c->GetBool("host_only"),secure=c->GetBool("secure");
    bool valid=ServiceCookieHost(host,service) && ServiceCookieHost(domain,service) &&
      (host==domain || (host.size()>domain.size() && host.compare(host.size()-domain.size(),domain.size(),domain)==0 && host[host.size()-domain.size()-1]=='.')) &&
      (!host_only || host==domain) && !name.empty() && name.size()<=256 && value.size()<=16384 &&
      !path.empty() && path[0]=='/' && path.size()<=2048 && (same=="unspecified" || same=="none" || same=="lax" || same=="strict") &&
      (same!="none" || secure) && (name.rfind("__Secure-",0)!=0 || secure) &&
      (name.rfind("__Host-",0)!=0 || (secure && host_only && path=="/"));
    for(unsigned char b:name)valid=valid && b>=0x21 && b<=0x7e && std::string("()<>@,;:\\\"/[]?={}").find(b)==std::string::npos;
    for(unsigned char b:value)valid=valid && (b==0x21 || (b>=0x23 && b<=0x2b) || (b>=0x2d && b<=0x3a) || (b>=0x3c && b<=0x5b) || (b>=0x5d && b<=0x7e));
    for(wchar_t b:c->GetString("path").ToWString())valid=valid && b>=0x20 && !(b>=0x7f && b<=0x9f) && b!=';';
    bytes+=host.size()+domain.size()+name.size()+path.size()+value.size();
    // Wipe our temporary copy. CEF owns separate strings until the command is
    // consumed; this does not claim to erase CEF/Chromium allocator copies.
    WipeServiceTransport(value);
    if(!valid || bytes>2*1024*1024 || !keys.emplace(domain,host_only,path,name).second)return false;
    const auto expires_type=c->GetType("expires_unix_seconds");
    if(expires_type!=VTYPE_NULL) {
      if(expires_type!=VTYPE_INT && expires_type!=VTYPE_DOUBLE)return false;
      const double expires=expires_type==VTYPE_INT?c->GetInt("expires_unix_seconds"):c->GetDouble("expires_unix_seconds");
      cef_time_t utc{};cef_basetime_t base{};
      if(!std::isfinite(expires) || std::floor(expires)!=expires || expires<=now || expires>253402300799.0 ||
         !cef_time_from_doublet(expires,&utc) || !cef_time_to_basetime(&utc,&base))return false;
    }
  }
  return true;
}

// The bootstrap is a local, script-free document at the exact admitted HTTPS
// origin. It never loads provider content or puts private values in a URL.
class ServiceOriginBootstrap final : public CefSchemeHandlerFactory {
 public:
  ServiceOriginBootstrap(int browser,std::string url):browser_(browser),url_(std::move(url)) {}
  CefRefPtr<CefResourceHandler> Create(CefRefPtr<CefBrowser> browser,CefRefPtr<CefFrame> frame,
                                      const CefString&,CefRefPtr<CefRequest> request) override {
    if(!browser || browser->GetIdentifier()!=browser_ || !frame || !frame->IsMain() || request->GetURL()!=url_)return nullptr;
    static const char body[]="<!doctype html><meta http-equiv='Content-Security-Policy' content=\"default-src 'none'\"><title></title>";
    return new CefStreamResourceHandler("text/html",CefStreamReader::CreateForData(const_cast<char*>(body),sizeof(body)-1));
  }
 private:
  int browser_;std::string url_;
  IMPLEMENT_REFCOUNTING(ServiceOriginBootstrap);
};

// The first receipt means SetCookie + cookie FlushStore and exact origin
// readback. localStorage has no public durable flush callback in CEF. The owner
// must gracefully shut down and restart this isolated host, then verify_only
// must match the persisted values before recording a durable profile receipt.
class ServiceSessionImport final : public CefSetCookieCallback,public CefCompletionCallback,
                                   public CefDevToolsMessageObserver {
 public:
  using Reply=std::function<void(CefRefPtr<CefDictionaryValue>)>;
  ServiceSessionImport(CefRefPtr<CefBrowser> browser,CefRefPtr<CefDictionaryValue> command,Reply reply)
      :browser_(browser),manager_(browser->GetHost()->GetRequestContext()->GetCookieManager(nullptr)),command_(command),
       reply_(std::move(reply)),count_(command->GetList("cookies")->GetSize()),
       origins_(command->HasKey("origin_storage")?command->GetList("origin_storage")->GetSize():0),verify_(command->GetBool("verify_only")) {}
  void Start() {
    registration_=browser_->GetHost()->AddDevToolsMessageObserver(this);
    CefPostDelayedTask(TID_UI,base::BindOnce([](CefRefPtr<ServiceSessionImport> self){if(!self->finished_)self->Finish(false);},CefRefPtr<ServiceSessionImport>(this)),30000);
    if(!manager_)Finish(false);else if(verify_)Call(Stage::Cookies,"Storage.getCookies",nullptr);else Next();
  }
  void Detach() {if(!finished_)Finish(false);registration_=nullptr;browser_=nullptr;}
  void Loaded(CefRefPtr<CefFrame> frame,int status) {
    if(finished_ || stage_!=Stage::Bootstrap || !frame->IsMain())return;
    // OnLoadEnd for the pre-import about:blank can arrive after Start. Only
    // the locally admitted bootstrap document can advance this operation.
    if(frame->GetURL()!=bootstrap_)return;
    bootstrap_status_=status;if(status!=200) {Finish(false);return;}
    auto params=OriginParameters();
    if(!verify_) {
      auto source=command_->GetList("origin_storage")->GetDictionary(0);
      params->SetString("key",source->GetString("key"));params->SetString("value",source->GetString("value"));
      Call(Stage::SetOrigin,"DOMStorage.setDOMStorageItem",params);params->SetString("value","");
    } else Call(Stage::GetOrigin,"DOMStorage.getDOMStorageItems",params);
  }
  void OnComplete(bool success) override {if(!success)Finish(false);else {++index_;Next();}}
  void OnComplete() override {BeginOrigin();}
  void OnDevToolsMethodResult(CefRefPtr<CefBrowser>,int id,bool success,const void* result,size_t size) override {
    if(finished_ || size>4*1024*1024)return;
    // The return may precede ExecuteDevToolsMethod's assigned ID. Queue the
    // bounded private reply and match on the next UI turn; never publish it.
    std::string copy(static_cast<const char*>(result),size);
    CefPostTask(TID_UI,base::BindOnce([](CefRefPtr<ServiceSessionImport> self,int id,bool success,std::string copy){
      self->Result(id,success,copy);WipeServiceTransport(copy);
    },CefRefPtr<ServiceSessionImport>(this),id,success,std::move(copy)));
  }
  void OnDevToolsAgentDetached(CefRefPtr<CefBrowser>) override {if(!finished_)Finish(false);}
 private:
  enum class Stage { None,Cookies,Bootstrap,SetOrigin,GetOrigin };
  ~ServiceSessionImport() override {if(command_)ForgetServiceCookies(command_);}
  CefRefPtr<CefDictionaryValue> OriginParameters() {
    auto storage=CefDictionaryValue::Create();storage->SetString("securityOrigin","https://discord.com");storage->SetBool("isLocalStorage",true);
    auto params=CefDictionaryValue::Create();params->SetDictionary("storageId",storage);return params;
  }
  void Call(Stage stage,const char* method,CefRefPtr<CefDictionaryValue> params) {
    stage_=stage;pending_=browser_->GetHost()->ExecuteDevToolsMethod(0,method,params);if(!pending_)Finish(false);
  }
  bool CookiesMatch(CefRefPtr<CefDictionaryValue> result) {
    if(result->GetType("cookies")!=VTYPE_LIST)return false;auto actual=result->GetList("cookies");
    if(actual->GetSize()>1024)return false;
    auto expected=command_->GetList("cookies");
    for(size_t i=0;i<expected->GetSize();++i) {
      auto e=expected->GetDictionary(i);bool found=false;
      for(size_t j=0;j<actual->GetSize();++j) {
        if(actual->GetType(j)!=VTYPE_DICTIONARY)return false;auto c=actual->GetDictionary(j);
        const auto domain=e->GetBool("host_only")?e->GetString("domain").ToString():"."+e->GetString("domain").ToString();
        if(c->GetString("domain")!=domain || c->GetString("name")!=e->GetString("name") || c->GetString("path")!=e->GetString("path"))continue;
        if(c->GetString("value")!=e->GetString("value") || c->GetBool("secure")!=e->GetBool("secure") || c->GetBool("httpOnly")!=e->GetBool("http_only"))return false;
        const auto same=e->GetString("same_site");
        if((same=="unspecified" && c->HasKey("sameSite")) || (same=="none" && c->GetString("sameSite")!="None") ||
           (same=="lax" && c->GetString("sameSite")!="Lax") || (same=="strict" && c->GetString("sameSite")!="Strict"))return false;
        const bool session=e->GetType("expires_unix_seconds")==VTYPE_NULL;
        if(c->GetBool("session")!=session)return false;
        if(!session) {
          const double expiry=e->GetType("expires_unix_seconds")==VTYPE_INT?e->GetInt("expires_unix_seconds"):e->GetDouble("expires_unix_seconds");
          const double observed=c->GetType("expires")==VTYPE_INT?c->GetInt("expires"):c->GetDouble("expires");
          if(std::abs(expiry-observed)>0.001)return false;
        }
        found=true;break;
      }
      if(!found)return false;
    }
    return true;
  }
  void Result(int id,bool success,const std::string& json) {
    if(finished_ || id!=pending_)return;pending_=0;if(!success) {Finish(false);return;}
    auto parsed=CefParseJSON(json,JSON_PARSER_RFC);
    auto result=parsed && parsed->GetType()==VTYPE_DICTIONARY?parsed->GetDictionary():nullptr;
    if(!result) {Finish(false);return;}
    if(stage_==Stage::Cookies) {const bool matches=CookiesMatch(result);ForgetServiceCookies(result);if(matches)BeginOrigin();else Finish(false);return;}
    if(stage_==Stage::SetOrigin) {Call(Stage::GetOrigin,"DOMStorage.getDOMStorageItems",OriginParameters());return;}
    if(stage_!=Stage::GetOrigin || result->GetType("entries")!=VTYPE_LIST) {Finish(false);return;}
    auto entries=result->GetList("entries");if(entries->GetSize()>1024) {Finish(false);return;}
    auto expected=command_->GetList("origin_storage")->GetDictionary(0);bool matches=false;
    for(size_t i=0;i<entries->GetSize();++i) {
      if(entries->GetType(i)!=VTYPE_LIST)continue;auto entry=entries->GetList(i);
      if(entry->GetSize()==2 && entry->GetType(0)==VTYPE_STRING && entry->GetType(1)==VTYPE_STRING && entry->GetString(0)=="token")matches=entry->GetString(1)==expected->GetString("value");
      if(entry->GetSize()>1)entry->SetString(1,"");
    }
    Finish(matches);
  }
  void BeginOrigin() {
    if(!origins_) {Finish(true);return;}
    bootstrap_="https://discord.com/.tilo-session/"+command_->GetString("session_id").ToString();
    if(!browser_->GetHost()->GetRequestContext()->RegisterSchemeHandlerFactory("https","discord.com",new ServiceOriginBootstrap(browser_->GetIdentifier(),bootstrap_))) {Finish(false);return;}
    stage_=Stage::Bootstrap;browser_->GetMainFrame()->LoadURL(bootstrap_);
  }
  void Next() {
    if(finished_)return;
    if(index_==count_) {if(!manager_->FlushStore(this))Finish(false);return;}
    auto source=command_->GetList("cookies")->GetDictionary(index_);CefCookie cookie;
    CefString(&cookie.name)=source->GetString("name");
    if(!source->GetBool("host_only"))CefString(&cookie.domain)=source->GetString("domain");
    CefString(&cookie.path)=source->GetString("path");cookie.secure=source->GetBool("secure");cookie.httponly=source->GetBool("http_only");
    const auto same=source->GetString("same_site");
    cookie.same_site=same=="none"?CEF_COOKIE_SAME_SITE_NO_RESTRICTION:same=="lax"?CEF_COOKIE_SAME_SITE_LAX_MODE:same=="strict"?CEF_COOKIE_SAME_SITE_STRICT_MODE:CEF_COOKIE_SAME_SITE_UNSPECIFIED;
    if(source->GetType("expires_unix_seconds")!=VTYPE_NULL) {
      cef_time_t utc{};const double seconds=source->GetType("expires_unix_seconds")==VTYPE_INT?source->GetInt("expires_unix_seconds"):source->GetDouble("expires_unix_seconds");
      if(!cef_time_from_doublet(seconds,&utc) || !cef_time_to_basetime(&utc,&cookie.expires) || cookie.expires.val<=cef_basetime_now().val) {Finish(false);return;}
      cookie.has_expires=true;
    }
    const auto url="https://"+source->GetString("host").ToString()+source->GetString("path").ToString();
    CefString(&cookie.value)=source->GetString("value");
    const bool admitted=manager_->SetCookie(url,cookie,this);source->SetString("value","");
    volatile auto* secret=cookie.value.str;for(size_t i=0;i<cookie.value.length;++i)secret[i]=0;
    if(!admitted)Finish(false);
  }
  void Finish(bool imported) {
    if(finished_)return;finished_=true;
    if(!imported)std::fprintf(stderr,"[service-session] refused stage=%d bootstrap_status=%d verify=%d\n",static_cast<int>(stage_),bootstrap_status_,verify_?1:0);
    auto fields=CefDictionaryValue::Create();fields->SetInt("request_id",command_->GetInt("request_id"));
    fields->SetString("session_id",command_->GetString("session_id"));fields->SetString("service",command_->GetString("service"));
    fields->SetBool("imported",imported);fields->SetInt("cookie_count",imported?static_cast<int>(count_):0);
    fields->SetInt("origin_storage_count",imported?static_cast<int>(origins_):0);
    ForgetServiceCookies(command_);command_=nullptr;manager_=nullptr;registration_=nullptr;
    auto reply=std::move(reply_);reply(fields);
  }
  CefRefPtr<CefBrowser> browser_;CefRefPtr<CefCookieManager> manager_;CefRefPtr<CefDictionaryValue> command_;
  CefRefPtr<CefRegistration> registration_;Reply reply_;const size_t count_,origins_;const bool verify_;
  size_t index_=0;bool finished_=false;Stage stage_=Stage::None;int pending_=0,bootstrap_status_=0;std::string bootstrap_;
  IMPLEMENT_REFCOUNTING(ServiceSessionImport);
};
} // namespace tilo
