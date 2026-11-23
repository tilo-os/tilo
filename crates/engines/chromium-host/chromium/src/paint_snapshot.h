#pragma once
#include "include/cef_browser.h"
#include "include/cef_devtools_message_observer.h"
#include "include/cef_parser.h"
#include "include/base/cef_callback.h"
#include "include/wrapper/cef_closure_task.h"
#include <cctype>
#include <functional>
#include <map>
#include <string>
#include <utility>

namespace tilo {
// The page as layout rather than pixels, for the paint stream: a snapshot of
// every box and text line the engine laid out, and the bytes of a resource the
// page already loaded. Both come from the browser process over DevTools, so
// the page can neither observe nor answer them, and a resource is read from
// the engine's cache instead of being fetched again.
class PaintSnapshot final : public CefDevToolsMessageObserver {
 public:
  using Emit=std::function<void(const std::string&,CefRefPtr<CefDictionaryValue>)>;
  PaintSnapshot(CefRefPtr<CefBrowser> browser,Emit emit):browser_(browser),emit_(std::move(emit)) {}
  void Attach() { registration_=browser_->GetHost()->AddDevToolsMessageObserver(this); }
  void Detach() { registration_=nullptr;browser_=nullptr;calls_.clear();shown_.clear(); }
  // {request_id, styles: [css property...]} -> paint_snapshot {request_id, ok, snapshot}
  // A capture that names fields (`shown`) asks what they show instead.
  void Capture(CefRefPtr<CefDictionaryValue> d) {
    if(d->HasKey("shown")) { Shown(d);return; }
    auto params=CefDictionaryValue::Create();
    params->SetList("computedStyles",d->GetList("styles")->Copy());
    params->SetBool("includePaintOrder",true);params->SetBool("includeDOMRects",true);
    Call(Pending{Kind::Snapshot,d->GetInt("request_id"),{}},"DOMSnapshot.captureSnapshot",params);
  }
  // {request_id, shown: [backend node id...]} -> paint_shown {request_id, ok, fields: [{node, text}]}
  // What a field shows where that is not what it holds: a date, a time or a
  // number as the engine words it for whoever reads the page, which stands in
  // the engine's own tree inside the field and in no snapshot of the layout.
  // A field that could not be read is left out of the answer. A password
  // field is never read: what it holds is no text it shows.
  //
  // `faces` asks beside it what class of face the engine sets each family
  // list in, on this machine: `faces` in the answer says for each, in order,
  // the generic family it is set like, or nothing where it is like none or
  // like several, and `settled` whether every face the page loads was there.
  // That is measured in a world of the page's frame the page's own scripts
  // do not share, on a canvas that is in no document.
  void Shown(CefRefPtr<CefDictionaryValue> d) {
    const int request=d->GetInt("request_id");auto nodes=d->GetList("shown");
    const int count=static_cast<int>(nodes->GetSize());
    std::string measure;
    if(d->GetType("faces")==VTYPE_LIST && d->GetList("faces")->GetSize()) {
      auto faces=CefValue::Create();faces->SetList(d->GetList("faces")->Copy());
      measure=std::string(kFaces)+"("+CefWriteJSON(faces,JSON_WRITER_DEFAULT).ToString()+")";
    }
    auto& asked=shown_[request];asked.fields=CefListValue::Create();asked.faces=CefListValue::Create();
    asked.waiting=count+(measure.empty()?0:1);
    // A call that cannot be made counts one answer off at once, and the last
    // one answers the ask: nothing of `asked` is read after the calls.
    for(int i=0;i<count;++i) {
      auto params=CefDictionaryValue::Create();
      params->SetInt("backendNodeId",nodes->GetInt(i));params->SetInt("depth",-1);params->SetBool("pierce",true);
      Call(Pending{Kind::Shown,request,{},nodes->GetInt(i)},"DOM.describeNode",params);
    }
    if(!measure.empty()) Call(Pending{Kind::FacesFrame,request,std::move(measure)},"Page.getFrameTree",nullptr);
  }
  // {request_id, url} -> paint_resource {request_id, ok, content, base64}
  void Resource(CefRefPtr<CefDictionaryValue> d) {
    Call(Pending{Kind::FrameTree,d->GetInt("request_id"),d->GetString("url").ToString()},"Page.getFrameTree",nullptr);
  }
  void OnDevToolsMethodResult(CefRefPtr<CefBrowser>,int id,bool success,const void* result,size_t size) override {
    // CEF may answer before ExecuteDevToolsMethod returns the id, and every
    // observer sees every result: copy, then match ids on the next UI turn.
    CefRefPtr<PaintSnapshot> self(this);
    std::string copy(static_cast<const char*>(result),size);
    CefPostTask(TID_UI,base::BindOnce([](CefRefPtr<PaintSnapshot> self,int id,bool success,std::string copy) {
      self->Result(id,success,copy);
    },self,id,success,std::move(copy)));
  }
  void OnDevToolsAgentDetached(CefRefPtr<CefBrowser>) override {
    auto calls=std::move(calls_);calls_.clear();
    for(auto& [id,pending]:calls) Fail(pending);
  }

 private:
  enum class Kind { Snapshot, FrameTree, Resource, Shown, FacesFrame, FacesWorld, Faces };
  // Which generic family the engine sets each family list like: the width of
  // one line of text set in the list, beside its width in each generic family.
  static constexpr const char* kFaces=R"js((lists => {
  const pen = document.createElement('canvas').getContext('2d');
  const line = 'The quick brown fox, iIl1 mmmWWW 0123456789';
  const wide = list => { pen.font = '40px serif'; pen.font = '40px ' + list; return pen.measureText(line).width; };
  const generic = ['serif', 'sans-serif', 'monospace'];
  const widths = generic.map(wide);
  return {
    faces: lists.map(list => {
      const own = wide(list);
      const like = generic.filter((_, at) => Math.abs(widths[at] - own) < 0.01);
      return like.length === 1 ? like[0] : '';
    }),
    settled: !document.fonts || document.fonts.status === 'loaded',
  };
}))js";
  struct Pending { Kind kind;int request;std::string url;int node=0; };
  // The fields of one ask the engine has still to describe, and what it said of the others.
  struct Asked { int waiting=0;CefRefPtr<CefListValue> fields,faces;bool settled=true; };
  // Whether a described node is a password field: its attributes come as name, value, name, value.
  static bool Password(CefRefPtr<CefDictionaryValue> node) {
    if(node->GetType("attributes")!=VTYPE_LIST) return false;
    auto attributes=node->GetList("attributes");
    const auto lower=[](std::string text){for(auto& c:text)c=static_cast<char>(std::tolower(static_cast<unsigned char>(c)));return text;};
    for(size_t i=0;i+1<attributes->GetSize();i+=2) {
      if(attributes->GetType(i)!=VTYPE_STRING || attributes->GetType(i+1)!=VTYPE_STRING) continue;
      if(lower(attributes->GetString(i).ToString())=="type" && lower(attributes->GetString(i+1).ToString())=="password") return true;
    }
    return false;
  }
  // The text of a described node and of everything under it, the engine's own tree included, in the order it is laid out.
  static void ShownText(CefRefPtr<CefDictionaryValue> node,std::string& out,int depth) {
    if(!node || depth>24 || out.size()>1024) return;
    if(node->GetType("nodeType")==VTYPE_INT && node->GetInt("nodeType")==3 && node->GetType("nodeValue")==VTYPE_STRING) out+=node->GetString("nodeValue").ToString();
    for(const char* key:{"shadowRoots","children"}) {
      if(node->GetType(key)!=VTYPE_LIST) continue;
      auto list=node->GetList(key);
      for(size_t i=0;i<list->GetSize();++i) if(list->GetType(i)==VTYPE_DICTIONARY) ShownText(list->GetDictionary(i),out,depth+1);
    }
  }
  // One field of an ask was described or could not be: the ask is answered when all were.
  void Told(int request) {
    auto found=shown_.find(request);
    if(found==shown_.end() || --found->second.waiting>0) return;
    auto fields=CefDictionaryValue::Create();fields->SetBool("ok",true);fields->SetList("fields",found->second.fields);
    fields->SetList("faces",found->second.faces);fields->SetBool("settled",found->second.settled);
    shown_.erase(found);Reply("paint_shown",request,fields);
  }
  void Call(Pending pending,const char* method,CefRefPtr<CefDictionaryValue> params) {
    if(!browser_) return;
    const int id=browser_->GetHost()->ExecuteDevToolsMethod(0,method,params);
    if(!id) { Fail(pending);return; }
    calls_.emplace(id,std::move(pending));
  }
  void Result(int id,bool success,const std::string& json) {
    auto found=calls_.find(id);
    if(!browser_ || found==calls_.end()) return;
    Pending pending=std::move(found->second);calls_.erase(found);
    if(!success) { Fail(pending);return; }
    if(pending.kind==Kind::Snapshot) {
      auto fields=CefDictionaryValue::Create();fields->SetBool("ok",true);fields->SetString("snapshot",json);
      Reply("paint_snapshot",pending.request,fields);return;
    }
    auto parsed=CefParseJSON(json,JSON_PARSER_RFC);
    auto result=parsed && parsed->GetType()==VTYPE_DICTIONARY?parsed->GetDictionary():nullptr;
    if(pending.kind==Kind::Shown) {
      auto node=result && result->GetType("node")==VTYPE_DICTIONARY?result->GetDictionary("node"):nullptr;
      auto found=shown_.find(pending.request);
      if(node && found!=shown_.end() && !Password(node)) {
        std::string text;ShownText(node,text,0);
        auto field=CefDictionaryValue::Create();field->SetInt("node",pending.node);field->SetString("text",text);
        found->second.fields->SetDictionary(found->second.fields->GetSize(),field);
      }
      Told(pending.request);return;
    }
    if(pending.kind==Kind::FacesFrame) {
      auto tree=result && result->GetType("frameTree")==VTYPE_DICTIONARY?result->GetDictionary("frameTree"):nullptr;
      auto frame=tree && tree->GetType("frame")==VTYPE_DICTIONARY?tree->GetDictionary("frame"):nullptr;
      if(!frame || frame->GetType("id")!=VTYPE_STRING) { Fail(pending);return; }
      auto params=CefDictionaryValue::Create();
      params->SetString("frameId",frame->GetString("id"));params->SetString("worldName","tilo paint");
      pending.kind=Kind::FacesWorld;Call(std::move(pending),"Page.createIsolatedWorld",params);return;
    }
    if(pending.kind==Kind::FacesWorld) {
      if(!result || result->GetType("executionContextId")!=VTYPE_INT) { Fail(pending);return; }
      auto params=CefDictionaryValue::Create();
      params->SetString("expression",pending.url);params->SetInt("contextId",result->GetInt("executionContextId"));
      params->SetBool("returnByValue",true);
      pending.kind=Kind::Faces;pending.url.clear();Call(std::move(pending),"Runtime.evaluate",params);return;
    }
    if(pending.kind==Kind::Faces) {
      auto said=result && result->GetType("result")==VTYPE_DICTIONARY?result->GetDictionary("result"):nullptr;
      auto value=said && said->GetType("value")==VTYPE_DICTIONARY?said->GetDictionary("value"):nullptr;
      auto found=shown_.find(pending.request);
      if(value && value->GetType("faces")==VTYPE_LIST && found!=shown_.end()) {
        found->second.faces=value->GetList("faces")->Copy();
        found->second.settled=value->GetType("settled")!=VTYPE_BOOL || value->GetBool("settled");
      }
      Told(pending.request);return;
    }
    if(pending.kind==Kind::FrameTree) {
      auto tree=result?result->GetDictionary("frameTree"):nullptr;
      auto frame=tree?tree->GetDictionary("frame"):nullptr;
      if(!frame || frame->GetType("id")!=VTYPE_STRING) { Fail(pending);return; }
      auto params=CefDictionaryValue::Create();
      params->SetString("frameId",frame->GetString("id"));params->SetString("url",pending.url);
      pending.kind=Kind::Resource;Call(std::move(pending),"Page.getResourceContent",params);return;
    }
    if(!result || result->GetType("content")!=VTYPE_STRING) { Fail(pending);return; }
    auto fields=CefDictionaryValue::Create();fields->SetBool("ok",true);
    fields->SetString("content",result->GetString("content"));
    fields->SetBool("base64",result->GetType("base64Encoded")==VTYPE_BOOL && result->GetBool("base64Encoded"));
    Reply("paint_resource",pending.request,fields);
  }
  void Fail(const Pending& pending) {
    if(pending.kind==Kind::Shown || pending.kind==Kind::FacesFrame || pending.kind==Kind::FacesWorld || pending.kind==Kind::Faces) { Told(pending.request);return; }
    auto fields=CefDictionaryValue::Create();fields->SetBool("ok",false);
    Reply(pending.kind==Kind::Snapshot?"paint_snapshot":"paint_resource",pending.request,fields);
  }
  void Reply(const char* event,int request,CefRefPtr<CefDictionaryValue> fields) {
    fields->SetInt("request_id",request);emit_(event,fields);
  }
  CefRefPtr<CefBrowser> browser_;CefRefPtr<CefRegistration> registration_;Emit emit_;
  std::map<int,Pending> calls_;
  std::map<int,Asked> shown_;
  IMPLEMENT_REFCOUNTING(PaintSnapshot);
};
}
