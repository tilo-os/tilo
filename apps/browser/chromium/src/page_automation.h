#pragma once
#include "page_automation_script.h"
#include "include/cef_browser.h"
#include "include/cef_drag_data.h"
#include "include/cef_devtools_message_observer.h"
#include "include/cef_dialog_handler.h"
#include "include/cef_parser.h"
#include "include/base/cef_callback.h"
#include "include/wrapper/cef_closure_task.h"
#include <map>
#include <functional>
#include <chrono>
#include <optional>
#include <string>
#include <utility>
#include <vector>

namespace tilo {
// One frame of the page: its document, the id its parent's outline printed,
// and the hit-testing offset that turns a point in its document into a point
// in the top one.
struct PageFrame { std::string id,loader,address,number;double x=0,y=0; };
// A point in a frame's own document, hit-tested in the top one.
struct Clicked { double x=0,y=0; };

// Documents are numbered across every tab of this host, so a ref read in one
// tab never resolves in another.
inline int page_documents=0;

// Browser-process page reading and operation for the browser's page actions.
// Script runs in an isolated world of one frame — the main frame, or the frame
// an action names with the id its parent's outline printed — created through
// DevTools: it shares that document's DOM, but page script can neither see nor
// call it. Clicks and keys go through the same CEF input path as a person's,
// so the page receives trusted events with user activation; a click inside a
// frame is offset by the frame's place in the page.
//
// A page's file chooser is the one native surface an agent cannot drive. A
// click a page action caused therefore holds the chooser's callback here
// instead of showing a panel, and the click's reply says so; `fill_files`
// then answers that same callback with the files the app checked, so the page
// receives the chooser's own trusted input and change events. A person's own
// click still opens the platform panel.
class PageAutomation final : public CefDevToolsMessageObserver {
 public:
  using Emit=std::function<void(const std::string&,CefRefPtr<CefDictionaryValue>)>;
  // CSS pixels in the main frame's viewport.
  using Click=std::function<void(double,double)>;
  using Key=std::function<void(CefRefPtr<CefDictionaryValue>)>;
  // One step of a pointer gesture, in the same CSS pixels.
  using Pointer=std::function<void(CefRefPtr<CefDictionaryValue>)>;
  PageAutomation(CefRefPtr<CefBrowser> browser,Emit emit,Click click,Key key,Pointer pointer)
      :browser_(browser),emit_(std::move(emit)),click_(std::move(click)),key_(std::move(key)),pointer_(std::move(pointer)) {}
  void Attach() { registration_=browser_->GetHost()->AddDevToolsMessageObserver(this); }
  void Detach() { registration_=nullptr;browser_=nullptr;pending_.reset();CancelFileDialog(); }
  // A page asked for files. A click this automation is driving holds the
  // chooser for `fill_files`; anything else keeps the platform panel.
  bool HoldFileDialog(CefRefPtr<CefFileDialogCallback> callback) {
    if(!pending_ || Action()!="click") return false;
    // One page, one chooser: a newer request supersedes an unanswered one.
    CancelFileDialog();
    file_dialog_=callback;file_dialog_request_=pending_->request;
    // A page whose chooser is never answered holds the browser's own file
    // work open forever, so the hold ends on its own as well.
    CefPostDelayedTask(TID_UI,base::BindOnce([](CefRefPtr<PageAutomation> self) {
      if(self->browser_) self->CancelFileDialog();
    },CefRefPtr<PageAutomation>(this)),kFileDialogWaitMs);
    return true;
  }
  // A main-frame navigation began. An action that caused one answers once it
  // loads, so the caller's next read sees the page the action opened. The
  // document a held file chooser belonged to is gone, so that chooser is
  // cancelled rather than answered for a page nobody is looking at.
  void NavigationStarted() {
    ++navigations_;CancelFileDialog();
    if(pending_ && pending_->awaiting_start) { pending_->awaiting_start=false;pending_->awaiting_load=true; }
  }
  void NavigationFinished() {
    if(pending_ && pending_->awaiting_load) Succeed(pending_->message,std::nullopt);
  }
  void Request(CefRefPtr<CefDictionaryValue> command) {
    const int request=command->GetInt("request_id");
    if(!browser_) return;
    if(pending_) { Reply(request,Failure("busy","Another page action is still running in this tab"),command->GetString("op")=="text_input");return; }
    const bool private_text=command->GetString("op")=="text_input";
    auto owned=command->Copy(false);
    if(private_text) {
      const double now=std::chrono::duration<double,std::milli>(std::chrono::system_clock::now().time_since_epoch()).count();
      if(!owned->HasKey("expires_ms")) owned->SetDouble("expires_ms",now+5000);
      if(Number(owned,"expires_ms")<=now || Number(owned,"expires_ms")>now+10000) {
        Reply(request,Failure("text_input_expired","The original text request expired"),true);return;
      }
    }
    native_attempted_=false;native_object_.clear();
    // The document generation this action runs against (see host.cc's fences).
    const uint64_t generation=command->GetType("generation")==VTYPE_INT?static_cast<uint64_t>(command->GetInt("generation")):0;
    pending_=Pending(request,++token_,navigations_,false,owned,std::string{},generation);
    // A call still in flight belongs to the request it was sent for: the new
    // request's answers are matched against calls of its own only.
    call_=0;
    CefPostDelayedTask(TID_UI,base::BindOnce([](CefRefPtr<PageAutomation> self,uint64_t token) {
      if(self->pending_ && self->pending_->token==token) { self->Fail("timeout","The page did not answer in time"); }
    },CefRefPtr<PageAutomation>(this),token_),private_text?5000:kTimeoutMs);
    Begin();
  }
  // The page started a drag from the press the pointer just made. Offscreen
  // CEF offers no drag loop, so this host is the drag's target too: it drags
  // over the points the gesture asks for and drops at its end, which the
  // gesture's own step loop drives. A page drag nothing is driving is refused,
  // which is how such a drag ends.
  // Whether this automation's own gesture is the thing dragging the page.
  bool DrivesDrag() const { return pending_ && Action()=="drag"; }
  void OnDevToolsMethodResult(CefRefPtr<CefBrowser>,int id,bool success,const void* result,size_t size) override {
    // The body belongs to this callback alone, so it is copied now; the answer
    // is applied on this same turn, where a call made from it is the answer's
    // own call: a turn of its own would let the next call be sent first, and
    // the two answers would arrive under each other's stage.
    ApplyAnswer(id,success,std::string(size>0?static_cast<const char*>(result):"",size>0?size:0));
  }
  void OnDevToolsAgentDetached(CefRefPtr<CefBrowser>) override {
    world_=0;contexts_.clear();loader_.clear();world_loader_.clear();text_owner_.reset();text_lease_.clear();
    frame_world_.reset();world_request_.clear();main_world_=0;
    tree_=nullptr;++generation_;
    if(pending_ && call_) Fail("unavailable","The page's inspector connection closed");
  }

 private:
  // The answer of a call is applied on a turn of its own, and the call it
  // chains is sent from there: a call is only ever sent after the previous
  // answer has been read, so two answers never arrive under each other's stage.
  void ApplyAnswer(int id,bool success,const std::string& json) {
    if(!browser_ || !pending_ || !call_ || id!=call_) return;
    // The answer is its call's own turn: the next call is sent only after this
    // answer has read it, so a second answer is never taken for it.
    call_=0;stage_=stage_at_call_;
    CefPostTask(TID_UI,base::BindOnce([](CefRefPtr<PageAutomation> self,bool success,std::string json) {
      if(self->pending_) self->AdvanceStage(success,json);
    },CefRefPtr<PageAutomation>(this),success,json));
  }
  static constexpr int64_t kTimeoutMs=30000;
  // A hover's styles have run by the next frame; a release's handlers by
  // shortly after the frame that carried it.
  static constexpr int64_t kHoverFrameMs=80;
  static constexpr int64_t kReleaseFrameMs=80;
  // A drawing path's points are one per frame: pointer moves sent within a
  // single frame are coalesced, and the page would see only the last.
  static constexpr int64_t kPathPointMs=16;
  // A started navigation is awaited this long; a slower one is reported as
  // still loading and the caller waits for it.
  static constexpr int64_t kLoadWaitMs=10000;
  // A held file chooser waits this long for `fill_files`, then is cancelled so
  // the page is never left waiting on input that will not come.
  static constexpr int64_t kFileDialogWaitMs=60000;
  enum class Stage { FrameTree, World, FrameOffset, TextWorld, TextFocus, TextFailureFrameTree, NativeElement, NativeDOM, NativeDocument, NativeNode, NativeEditors, NativeResolve, NativeConfigure, FileTarget, FileSet, Evaluate, Fence, GestureBegin, Path, Hover, Pointer, GestureEnd, FrameSettle };
  // A gesture's own stages are numbered above every page stage, so one kind of
  // stage never stands for the other.
  enum class GestureStage { Tree, World, Locate, Place, PlaceWorld };
  static constexpr int kGestureBase=1000;
  static constexpr int Gesture(GestureStage stage) { return kGestureBase+static_cast<int>(stage)+1; }
  // Long enough for hover, the press and the drop to paint what they set.
  static constexpr int64_t kPointerFrameMs=60;
  struct TextFrame { std::string id,loader;int world=0,document=0;bool same_origin=true; };
  struct Pending {
    // Named, so an aggregate initializer names each field once: a member added
    // to the middle of the list cannot silently shift the ones after it.
    int request=0;uint64_t token=0;uint64_t navigations=0;bool retried=false;
    CefRefPtr<CefDictionaryValue> command;std::string message="";
    // "none": the action ran no page code. "started": the page began leaving
    // the document while it handled the action's input. "possible": it had
    // not, and may still navigate later on its own. "frame": what it did, it
    // did inside a frame, which is not a way for the page to leave.
    std::string navigation="none";bool awaiting_start=false,awaiting_load=false;
    // Where a gesture's two ends are, in the view's space, once it began.
    int from_x=0,from_y=0,to_x=0,to_y=0;int steps=0,step=0;
    // What the page resolved when a drag began, so a drop nothing took can be
    // replayed through the page's own drag and drop handlers.
    CefRefPtr<CefDictionaryValue> drag_fallback;
    // Whether the page's own CSS reveals something on hover, so the hover is
    // mirrored where the pointer rests.
    bool hover_styles=false;
    // A drawing path's points in the view's space, and the one being sent.
    std::vector<std::pair<double,double>> points;size_t point_index=0;bool drawing=false;
    uint64_t generation=0;
    Pending()=default;
    Pending(int request,uint64_t token,uint64_t navigations,bool retried,CefRefPtr<CefDictionaryValue> command,
      std::string message,uint64_t generation)
      :request(request),token(token),navigations(navigations),retried(retried),command(std::move(command)),
       message(std::move(message)),generation(generation) {}
  };
  // Gestures an agent asks for, which start from the page's own layout.
  bool Dragging() const {
    const auto action=Action();
    return action=="hover" || action=="drag" || action=="path";
  }
  // A hover at a viewport point travels as `at` with `hover` set, so the point
  // reads like every other hover: the page resolves it, the pointer rests
  // there, and the reply says what the hover revealed.
  bool HoverAtPoint() const { return Action()=="at" && pending_->command->GetBool("hover"); }
  bool Gesturing() const { return Dragging() || HoverAtPoint(); }
  std::string Action() const { return pending_->command->GetString("action").ToString(); }
  bool PrivateText() const { return pending_->command->GetString("op")=="text_input"; }
  static int FramePart(const std::string& part) {
    if(part.empty() || part.size()>3) return -1;
    int value=0;
    for(const char c:part) { if(c<'0'||c>'9') return -1;value=value*10+(c-'0'); }
    return value>0?value:-1;
  }
  // Frames are numbered from one in document order, both here and in the page
  // script, so `2.1` names the same frame to both.
  static bool ResolveFrame(CefRefPtr<CefDictionaryValue> tree,const std::vector<int>& path,size_t depth,PageFrame& out,const Clicked& inherited={},const std::string& prefix={}) {
    if(!tree || depth>path.size()) return false;
    auto frame=tree->GetDictionary("frame");
    if(!frame || frame->GetType("id")!=VTYPE_STRING || frame->GetType("loaderId")!=VTYPE_STRING) return false;
    if(depth==path.size()) {
      out.id=frame->GetString("id").ToString();out.loader=frame->GetString("loaderId").ToString();
      out.number=prefix;out.x=inherited.x;out.y=inherited.y;
      out.address=frame->GetType("url")==VTYPE_STRING?frame->GetString("url").ToString():std::string();
      return true;
    }
    auto children=tree->GetList("childFrames");
    if(!children || children->GetSize()>4096) return false;
    const auto ordinal=path[depth];
    if(ordinal<1 || static_cast<size_t>(ordinal)>children->GetSize()) return false;
    // Frames of one document are numbered in the order the page script walks
    // them, which is this list's order.
    auto child=children->GetDictionary(static_cast<size_t>(ordinal)-1);
    const auto offset=FrameOffset(child?child->GetDictionary("offset"):nullptr);
    const auto child_prefix=prefix.empty()?std::to_string(ordinal):prefix+"."+std::to_string(ordinal);
    return ResolveFrame(child,path,depth+1,out,{inherited.x+offset.x,inherited.y+offset.y},child_prefix);
  }
  // The frame element's border box inside its parent, as a point in the
  // parent's document: the page script measures it from the shared DOM, never
  // from the frame's own document.
  static Clicked FrameOffset(CefRefPtr<CefDictionaryValue> offset) {
    Clicked out;
    if(!offset) return out;
    for(const char* key:{"x","y"}) {
      const auto type=offset->GetType(key);
      if(type==VTYPE_DOUBLE || type==VTYPE_INT) (key[0]=='x'?out.x:out.y)=Number(offset,key);
    }
    return out;
  }
  bool CollectTextFrames(CefRefPtr<CefDictionaryValue> tree,std::vector<TextFrame>& frames,const std::string& root_origin={},bool observed_ancestors=true) {
    if(!tree || frames.size()>=128)return false;
    auto frame=tree->GetDictionary("frame");
    if(!frame || frame->GetType("id")!=VTYPE_STRING || frame->GetType("loaderId")!=VTYPE_STRING || frame->GetType("securityOrigin")!=VTYPE_STRING)return false;
    const auto origin=frame->GetString("securityOrigin").ToString();
    const auto root=root_origin.empty()?origin:root_origin;
    const bool observed=observed_ancestors && origin==root;
    frames.push_back({frame->GetString("id").ToString(),frame->GetString("loaderId").ToString(),0,0,observed});
    auto children=tree->GetList("childFrames");
    if(children)for(size_t i=0;i<children->GetSize();++i)
      if(!CollectTextFrames(children->GetDictionary(i),frames,root,observed))return false;
    return true;
  }
  void ProbeTextFrame() {
    // Cross-origin ancestors cannot be observed without widening DOM authority.
    while(!text_candidates_.empty() && !text_candidates_.front().same_origin)text_candidates_.erase(text_candidates_.begin());
    if(text_candidates_.empty()) { Fail("unavailable","The focused frame has no isolated text context");return; }
    text_frame_=text_candidates_.front();text_candidates_.erase(text_candidates_.begin());
    auto params=CefDictionaryValue::Create();
    params->SetString("frameId",text_frame_.id);params->SetString("worldName","tilo-private-text-input");
    // No universal access, origin bypass or debugger pause is requested.
    Call(Stage::TextWorld,"Page.createIsolatedWorld",params);
  }
  void Begin() {
    // The document generation (see host.cc's fences): a page action runs
    // against the document the caller saw. A click inside a frame is the
    // frame's own business, but the page around it is still the one the
    // caller saw.
    generation_=pending_->generation;
    // A press with no element arms a key: nothing on the page is read first.
    if(Action()=="press" && !pending_->command->HasKey("element")) { Act(nullptr);return; }
    // A chooser this page is waiting on is answered with the page's own
    // completion path, so its input and change events are the picker's, and
    // no DOM node has to be reached while that dialog is open.
    if(Action()=="fill_files" && file_dialog_) { AnswerFileDialog(pending_->command->GetList("paths"));return; }
    // Choosing files resolves the ref exactly as reading the page did, in the
    // document the outline named.
    // A gesture is never a read of a tree an earlier action left: it asks the
    // page for what only the page can say, whatever it already knows.
    // A pointer gesture is placed step by step, in the tree it reads itself.
    if(Action()=="pointer") { BeginGesture();return; }
    // A gesture the page runs itself starts from what the page shows now.
    if(Dragging() || HoverAtPoint()) { BeginPageGesture();return; }
    if(tree_ && tree_generation_==generation_) { AfterTree();return; }
    Call(Stage::FrameTree,"Page.getFrameTree",nullptr);
  }
  // Answer the held chooser with these paths. The page then runs exactly what
  // a person's pick runs.
  void AnswerFileDialog(CefRefPtr<CefListValue> paths) {
    auto chosen=CefRefPtr<CefFileDialogCallback>(file_dialog_);
    file_dialog_=nullptr;file_dialog_request_=0;
    std::vector<CefString> files;
    if(paths) for(size_t i=0;i<paths->GetSize();++i) files.push_back(paths->GetString(i));
    chosen->Continue(files);
    auto data=CefDictionaryValue::Create();data->SetList("files",FileNames());data->SetString("mode","chooser");
    Succeed(pending_->message,std::nullopt,data);
  }
  // The names of the files this action chose, as the page shows them: a path's
  // own last component, never the folder it came from.
  CefRefPtr<CefListValue> FileNames() {
    auto names=CefListValue::Create();
    auto paths=pending_?pending_->command->GetList("paths"):nullptr;
    for(size_t i=0;paths && i<paths->GetSize();++i) {
      const auto path=paths->GetString(i).ToString();
      const auto slash=path.find_last_of("/\\");
      names->SetString(i,slash==std::string::npos?path:path.substr(slash+1));
    }
    return names;
  }
  // A chooser the page opened and nothing answered is cancelled, so the page
  // is not left waiting on input that will never come.
  void CancelFileDialog() {
    if(!file_dialog_) return;
    auto held=CefRefPtr<CefFileDialogCallback>(file_dialog_);file_dialog_=nullptr;file_dialog_request_=0;
    held->Cancel();
  }
  // The isolated world resolved this step's element and accepted the paths, so
  // what remains is the DOM agent's own work: the files are not writable from
  // script. Resolve the element object again by ref, turn it into a node, and
  // let the input dispatch the picker's events.
  void FileTarget(CefRefPtr<CefDictionaryValue> value) {
    pending_->message=Text(value,"message");
    auto params=CefDictionaryValue::Create();
    params->SetString("expression","globalThis.__tiloPage.fillFilesElement("+CefWriteJSON(pending_->command->GetValue("element"),JSON_WRITER_DEFAULT).ToString()+")");
    params->SetString("objectGroup","tilo-page-file-input");
    params->SetInt("contextId",world_);params->SetBool("silent",true);
    Call(Stage::FileTarget,"Runtime.evaluate",params);
  }
  // A gesture's steps are resolved one at a time: a step that names an
  // element is placed in that element's box in this document, every other
  // step is already a viewport position.
  void BeginGesture() {
    pointer_index_=0;gesture_=true;SetGesture(0,0);gesture_boxes_.clear();step_located_=false;
    frame_place_world_=0;place_frame_=PageFrame{};
    auto steps=pending_->command->GetType("steps")==VTYPE_LIST?pending_->command->GetList("steps"):nullptr;
    if(!steps || steps->GetSize()==0) { Fail("unavailable","A gesture has no steps");return; }
    AdvancePointer();
  }
  // One page action in the world this gesture already shares with the page.
  CefRefPtr<CefDictionaryValue> PointerEvaluate(CefRefPtr<CefDictionaryValue> command,int context=-1) {
    auto wrapped=CefValue::Create();wrapped->SetDictionary(command);
    auto params=CefDictionaryValue::Create();
    params->SetString("expression",std::string(kPageAutomation)+"\n;globalThis.__tiloPage.run("+CefWriteJSON(wrapped,JSON_WRITER_DEFAULT).ToString()+")");
    params->SetInt("contextId",context>=0?context:world_);params->SetBool("returnByValue",true);params->SetBool("silent",true);
    params->SetBool("awaitPromise",true);
    return params;
  }
  CefRefPtr<CefDictionaryValue> PointerStep() const {
    auto steps=pending_ && pending_->command->GetType("steps")==VTYPE_LIST?pending_->command->GetList("steps"):nullptr;
    return steps && pointer_index_<steps->GetSize()?steps->GetDictionary(pointer_index_):CefRefPtr<CefDictionaryValue>();
  }
  // The frame a pointer step names, which is the one frame of the page holding
  // its element: a step never names a frame by a path.
  static std::string NamedFrame(CefRefPtr<CefDictionaryValue> step) {
    return step && step->GetType("frame")==VTYPE_STRING?step->GetString("frame").ToString():std::string();
  }
  // The frame this step names, as the page's own tree lists it: a path follows
  // the ancestors, `1.2` being the second frame of the first.
  bool PointerFramePlace(PageFrame& out,const std::string& named) {
    std::vector<int> path;
    size_t start=0;
    while(start<=named.size() && path.size()<=8) {
      const auto dot=named.find('.',start);
      const auto part=named.substr(start,dot==std::string::npos?std::string::npos:dot-start);
      const int ordinal=FramePart(part);
      if(ordinal<0) return false;
      path.push_back(ordinal);
      if(dot==std::string::npos) break;
      start=dot+1;
    }
    PageFrame target;
    const bool ok=!path.empty() && path.size()<=8 && ResolveFrame(tree_,path,0,target);
    if(!ok) return false;
    out=target;
    return true;
  }
  void AdvancePointer() {
    if(!pending_) return;
    // Only a gesture has steps: a step still queued when the gesture settled
    // finds the read its settlement left, and has nothing left to place.
    auto steps=pending_->command->GetType("steps")==VTYPE_LIST?pending_->command->GetList("steps"):nullptr;
    if(!steps) return;
    place_frame_=PageFrame{};
    // A gesture reads the page's tree as an action does: a step naming a frame
    // is placed by that frame's place in the page.
    if(!tree_ || tree_generation_!=generation_) { CallInt(Gesture(GestureStage::Tree),"Page.getFrameTree",CefRefPtr<CefDictionaryValue>());return; }
    if(!world_) { BeginGestureWorld();return; }
    if(pointer_index_>=steps->GetSize()) { SettlePointer();return; }
    auto step=steps->GetDictionary(pointer_index_);
    if(!step) { Fail("invalid_request","A gesture step is not an object");return; }
    // A step naming a frame is placed by that frame's place in the page, read
    // from the page's own tree as a click in a frame is. The step names only
    // its own frame, so the place is the one frame's, never a nested id.
    const auto named=step->GetType("frame")==VTYPE_STRING?step->GetString("frame").ToString():std::string();
    if(!named.empty() && named.find('.')!=std::string::npos)
      { Fail("no_frame","A pointer step names the frame whose outline lists its element: that frame's own id, without a dot");return; }
    // A gesture runs in the page's own document, where a frame's ref is not
    // held: the place of the frame is all this world needs of it.
    if(!named.empty() && !PointerFramePlace(place_frame_,named)) {
      Fail("no_frame","No frame of this page has that id; read the page again for the frames it lists");return;
    }
    step_located_=false;step_box_={0,0};
    if(!step->HasKey("element")) { PointerLocated();return; }
    // A step that keeps to the box a hand is already holding steps there: the
    // page does not re-place it, which would move the box mid-gesture.
    // A held box is a point of the page, so the place that moved it is not
    // added again.
    if(const auto held=HeldBox(step->GetString("element").ToString())) { gesture_x_=0;gesture_y_=0;step_box_=*held;step_located_=true;PointerLocated();return; }
    // A step of a frame names an element of that frame, which the page's own
    // world does not hold: the ref lives in the frame's document, so the page
    // says where the frame sits first and the box is read in the frame's
    // world once it is made, as a click in a frame resolves its ref there.
    if(!named.empty()) { PlaceFrameWorld();return; }
    auto command=CefDictionaryValue::Create();
    command->SetString("action","locate_box");command->SetInt("doc",document_);
    command->SetString("element",step->GetString("element"));
    CallInt(Gesture(GestureStage::Locate),"Runtime.evaluate",PointerEvaluate(command,world_));
  }
  // Whether the step being placed names a frame, whose element's box is
  // measured in that frame's own document: the page's world holds the page's
  // refs only.
  bool InFrameGesture() const { return !NamedFrame(PointerStep()).empty(); }
  // The world the gesture's own steps run in, made from the page's document.
  void BeginGestureWorld() {
    auto params=CefDictionaryValue::Create();
    params->SetString("frameId",main_id_);params->SetString("worldName","tilo-page-actions");
    CallInt(Gesture(GestureStage::World),"Page.createIsolatedWorld",params);
  }
  void BeginFramePlaceWorld() {
    // A frame's document is read in a world of its own, numbered separately
    // from the page's so a cached box is never read in the wrong document.
    loader_=place_frame_.loader;frame_place_world_=0;
    auto params=CefDictionaryValue::Create();
    params->SetString("frameId",place_frame_.id);params->SetString("worldName","tilo-page-actions");
    CallInt(Gesture(GestureStage::PlaceWorld),"Page.createIsolatedWorld",params);
  }
  bool PlaceFramePending() const { return !place_frame_.id.empty(); }
  // Where the frame the step names sits in the page, read in the page's own
  // world: a frame's world holds its frame's document, never the page's.
  void PlaceFrameRun() {
    auto command=CefDictionaryValue::Create();
    command->SetString("action","frame_offset");command->SetString("frame",place_frame_.number);command->SetBool("anchored",true);
    auto wrapped=CefValue::Create();wrapped->SetDictionary(command);
    auto params=CefDictionaryValue::Create();
    params->SetString("expression",std::string(kPageAutomation)+"\n;globalThis.__tiloPage.run("+CefWriteJSON(wrapped,JSON_WRITER_DEFAULT).ToString()+")");
    params->SetInt("contextId",main_world_);params->SetBool("returnByValue",true);params->SetBool("silent",true);
    // The place answers for the step that asked for it, which may be an
    // earlier one than the step now being placed.
    pointer_place_=place_frame_;
    CallInt(Gesture(GestureStage::Place),"Runtime.evaluate",params);
  }
  // A step naming a frame waits for the page's own world, which only it can
  // read the frame's place in, and then for the place itself.
  bool PlaceFrameWorld() {
    // A frame's place is measured in the page's own document, never in the
    // frame's world: a frame's world holds its own document only.
    if(!main_world_) {
      // The page's own world is made from the page's frame tree, read here
      // rather than at the start of the gesture: the world and the tree it
      // was made for are made together, and the step waits for them.
      auto params=CefDictionaryValue::Create();
      params->SetString("frameId",main_id_);params->SetString("worldName","tilo-page-actions");
      CallInt(Gesture(GestureStage::World),"Page.createIsolatedWorld",params);return false;
    }
    PlaceFrameRun();return true;
  }
  // A frame's own place in the page, once the page said where it sits. The
  // step's box lies in the frame's document, so the frame's world is asked for
  // that box next, and the two are added into one point of the page.
  void FramePlaceRead(CefRefPtr<CefDictionaryValue> value) {
    // The world was made for the step that asked for the place, so the place
    // is that step's: a step placed after it named another frame, and the
    // frame it named is the one whose box is read next.
    if(place_frame_.id!=pointer_place_.id) { AdvancePointer();return; }
    const auto number=[&](const char* key){return Number(value,key);};
    // The place is kept per frame, not per world: the frame's world answers
    // after it, and asks for it again, so a place read while that world was
    // still being made is the one the box is moved by.
    frame_place_read_=value->Copy(true);frame_place_id_=place_frame_.id;
    SetGesture(number("x"),number("y"));
    // The frame's own world is only worth asking for when the step's element
    // has a box in it; a step naming no element is placed by the place alone.
    if(!PointerStep()->HasKey("element")) { step_box_={0,0};PointerLocated();return; }
    BeginFramePlaceWorld();
  }
  // The box a step resolved to, kept for the rest of its gesture.
  void RememberBox(const std::string& ref) {
    if(gesture_boxes_.size()>=kGestureBoxes) return;
    if(std::any_of(gesture_boxes_.begin(),gesture_boxes_.end(),[&](const HeldBoxEntry& held){return held.ref==ref;})) return;
    // A ref's box is remembered with the point it lies at in the page.
    gesture_boxes_.push_back({ref,step_box_.x+gesture_x_,step_box_.y+gesture_y_});
  }
  std::optional<Clicked> HeldBox(const std::string& ref) const {
    for(const auto& held:gesture_boxes_) {
      if(held.ref!=ref) continue;
      // The box was placed against the frame the step named, so it is held
      // where the page placed it, never moved twice.
      return Clicked{held.x,held.y};
    }
    return std::nullopt;
  }
  // A step naming an element is placed in that element's box; one that names
  // none is already a viewport position. A step naming a frame is placed in
  // that frame's place, the box measured inside the frame and moved by where
  // the frame sits: the two halves only the page's own processes can give.
  void PointerLocated() {
    auto step=PointerStep();
    double x=0,y=0;
    if(step->HasKey("element")) {
      if(!step_located_) { Fail("stale_element","That element is not on the page ("+step->GetString("element").ToString()+(step->HasKey("frame")?" frame "+step->GetString("frame").ToString():"")+")");return; }
      x=step_box_.x+gesture_x_+Number(step,"x");y=step_box_.y+gesture_y_+Number(step,"y");
      RememberBox(step->GetString("element").ToString());
    } else { x=Number(step,"x");y=Number(step,"y"); }
    step_located_=false;
    auto event=CefDictionaryValue::Create();
    event->SetString("kind",step->GetString("kind"));event->SetDouble("x",x);event->SetDouble("y",y);
    event->SetString("button",pending_->command->GetString("button"));
    pointer_(event);
    ++pointer_index_;
    // A gesture's steps are its own frames: the page sees each position it
    // would have seen from a hand moving across it.
    CefPostDelayedTask(TID_UI,base::BindOnce([](CefRefPtr<PageAutomation> self,uint64_t token) {
      if(self->pending_ && self->pending_->token==token) self->AdvancePointer();
    },CefRefPtr<PageAutomation>(this),pending_->token),kPointerFrameMs);
  }
  // What the gesture did to the page is what the caller reads back: the same
  // read of the page any other action answers with, once the last step has
  // painted.
  void SettlePointer() {
    pointer_index_=0;gesture_=false;gesture_x_=gesture_y_=0;gesture_boxes_.clear();step_located_=false;
    // What the gesture did to the page is read like any other read: no page
    // code runs for it, and a gesture in a frame is not the page leaving.
    // The read is a `changes` command of its own: the gesture's own keys are
    // not part of it, so nothing the gesture carried is read as a read.
    auto read=CefDictionaryValue::Create();
    read->SetString("action","changes");
    read->SetString("op",pending_->command->GetString("op"));
    read->SetInt("request_id",pending_->request);
    read->SetInt("doc",document_);
    pending_->command=read;
    pending_->navigation="none";
    // A world asked for the gesture's own steps is not one an action's read
    // waits on, and a world still to be made for a frame's place is put off
    // until the next step needs it.
    world_request_.clear();place_frame_=PageFrame{};
    // The gesture's own world is the page's own document, which is where an
    // action's read runs: the read is asked for there, in the world the
    // gesture left the page in.
    if(!main_world_ && !world_) {
      world_request_=main_id_;
      auto params=CefDictionaryValue::Create();
      params->SetString("frameId",main_id_);params->SetString("worldName","tilo-page-actions");
      Call(Stage::World,"Page.createIsolatedWorld",params);return;
    }
    if(!main_world_) main_world_=world_;
    Evaluate();
  }
  // The step of a gesture the page is asked for: what the page holds before
  // the pointer moves, or a point for it to resolve.
  enum class PageGesture { Begin, Landing, Path, End };
  // A gesture starts from what the page shows now, so its answer can say
  // what the pointer changed.
  void BeginPageGesture() {
    pending_->steps=std::max(1,drag_steps());
    // The page's own CSS decides whether a hover has to be mirrored: a page
    // whose hover reveals nothing is driven by the browser's pointer alone.
    auto params=CefDictionaryValue::Create();
    params->SetString("expression",std::string(kPageAutomation)+"\n;globalThis.__tiloPage.run({action:'need_styles'})");
    params->SetInt("contextId",world_);params->SetBool("returnByValue",true);params->SetBool("userGesture",false);params->SetBool("silent",true);
    Call(Stage::Hover,"Runtime.evaluate",params);
  }
  int drag_steps() const {
    return pending_ && pending_->command->HasKey("steps")?static_cast<int>(pending_->command->GetInt("steps")):1;
  }
  // CSS pixels to the view's zoomed DIPs, where CEF's mouse events live.
  CefPoint view_point(double x,double y) const {
    const double zoom=std::pow(1.2,browser_->GetHost()->GetZoomLevel());
    return CefPoint(static_cast<int>(std::lround(x*zoom)),static_cast<int>(std::lround(y*zoom)));
  }
  bool gesture_page(CefRefPtr<CefDictionaryValue> value) const {
    return value && value->GetType("gesture_page")==VTYPE_BOOL && value->GetBool("gesture_page");
  }
  void GesturePage(Stage stage,PageGesture kind) {
    auto command=CefDictionaryValue::Create();
    const auto& source=pending_->command;
    command->SetInt("doc",document_);
    command->SetBool("gesture_page",true);
    // The page mirrors a hover through its own CSS only when that CSS opens
    // something, which it said before the gesture began.
    command->SetBool("mirror",pending_->hover_styles);
    // The gesture the page is asked for: a hover at a point travels as `at`
    // with `hover` set, so it is named here rather than taken from the action.
    const std::string gesture=HoverAtPoint()?"hover":Action();
    const std::string action=kind==PageGesture::Begin?"gesture_begin":kind==PageGesture::End?"gesture_end"
      :kind==PageGesture::Path?"path":gesture;
    command->SetString("action",action);
    command->SetString("kind",gesture);
    for(const char* key:{"element","to"}) if(source->HasKey(key)) command->SetString(key,source->GetString(key));
    for(const char* key:{"x","y","to_x","to_y"}) if(source->HasKey(key)) command->SetDouble(key,Number(source,key));
    // A path's answer says how many points it drew, which is what a canvas
    // holds and the outline cannot show.
    if(gesture=="path" && pending_->drawing) command->SetInt("drew",static_cast<int>(pending_->points.size()));
    // A hover's answer is read where the pointer was rested, which the page
    // only learns once its element has a centre.
    if(kind==PageGesture::End && gesture=="hover") {
      const double zoom=std::pow(1.2,browser_->GetHost()->GetZoomLevel());
      command->SetDouble("x",pending_->from_x/zoom);command->SetDouble("y",pending_->from_y/zoom);
    }
    // A drawing path carries its points; the page resolves them from the
    // element's own box, in viewport pixels for the engine's pointer.
    if((kind==PageGesture::Begin || kind==PageGesture::Path) && source->HasKey("points")) command->SetList("points",source->GetList("points")->Copy());
    auto wrapped=CefValue::Create();wrapped->SetDictionary(command);
    auto params=CefDictionaryValue::Create();
    params->SetString("expression",std::string(kPageAutomation)+"\n;globalThis.__tiloPage.run("+CefWriteJSON(wrapped,JSON_WRITER_DEFAULT).ToString()+")");
    params->SetInt("contextId",world_);params->SetBool("returnByValue",true);params->SetBool("userGesture",false);params->SetBool("silent",true);
    params->SetBool("awaitPromise",true);
    Call(stage,"Runtime.evaluate",params);
  }
  // Where the frame tree leads: the document this action runs in. The tree is
  // read once per page generation, so repeated actions on one page do not each
  // pay for it and none of them acts on a tree an older document described.
  void AfterTree() {
    auto frame=tree_?tree_->GetDictionary("frame"):nullptr;
    if(!frame) { Fail("unavailable","The page has no document to read");return; }
    if(PrivateText()) {
      std::vector<TextFrame> frames;
      if(!CollectTextFrames(tree_,frames)) { Fail("unavailable","The frame tree cannot be bounded and observed");return; }
      if(Action()=="prepare_text_input") {
        text_candidates_=std::move(frames);ProbeTextFrame();return;
      }
      const auto lease=pending_->command->GetString("lease").ToString();
      if(!text_owner_ || lease!=text_lease_) { Fail("text_input_retired","The original frame lease is no longer active");return; }
      const auto owner=*text_owner_;
      const bool live=std::any_of(frames.begin(),frames.end(),[&](const TextFrame& candidate) {
        return candidate.id==owner.id && candidate.loader==owner.loader;
      });
      if(!live) { text_owner_.reset();text_lease_.clear();Fail("text_input_retired","The original frame document was replaced");return; }
      text_frame_=owner;Evaluate();return;
    }
    // The frame the caller named, or the main document. A frame id the
    // page outline never printed is refused rather than read as the main
    // document, so a caller never reads the wrong document believing it
    // read the frame.
    PageFrame target;
    const auto named=pending_->command->GetType("frame")==VTYPE_STRING?pending_->command->GetString("frame").ToString():std::string();
    if(named.empty()) {
      target.id=frame->GetString("id").ToString();target.loader=frame->GetString("loaderId").ToString();
      target.address=frame->GetType("url")==VTYPE_STRING?frame->GetString("url").ToString():std::string();
    } else {
      std::vector<int> path;
      size_t start=0;
      while(start<=named.size() && path.size()<=8) {
        const auto dot=named.find('.',start);
        const auto part=named.substr(start,dot==std::string::npos?std::string::npos:dot-start);
        const int ordinal=FramePart(part);
        if(ordinal<0) { path.clear();break; }
        path.push_back(ordinal);
        if(dot==std::string::npos) break;
        start=dot+1;
      }
      if(path.empty() || path.size()>8) { Fail("no_frame","No frame of this page has that id; read the page again for the frames it lists");return; }
      if(named.size()>16 || !ResolveFrame(tree_,path,0,target)) {
        Fail("no_frame","No frame of this page has that id; read the page again for the frames it lists");return;
      }
    }
    // An action names its frame: the world left by an earlier action in another
    // frame never stands in for it.
    if(world_id_==target.id && world_ && target.loader==loader_) {
      if(named.empty() || world_frame_.number==named) { Evaluate();return; }
      world_frame_.x=0;world_frame_.y=0;
    }
    loader_=target.loader;world_=0;world_frame_=target;
    auto params=CefDictionaryValue::Create();
    params->SetString("frameId",target.id);params->SetString("worldName","tilo-page-actions");
    Call(Stage::World,"Page.createIsolatedWorld",params);
  }
  void Call(Stage stage,const char* method,CefRefPtr<CefDictionaryValue> params) {
    CallInt(static_cast<int>(stage),method,params);
  }
  bool At(Stage stage) const { return stage_==static_cast<int>(stage); }
  void CallInt(int stage,const char* method,CefRefPtr<CefDictionaryValue> params) {
    stage_=stage;
    const int id=browser_->GetHost()->ExecuteDevToolsMethod(0,method,params);
    call_=id;stage_at_call_=stage;
    if(!call_) Fail("unavailable","The page's inspector connection is unavailable");
  }
  void AdvanceStage(bool success,const std::string& json) {
    // A frame's settle answers from the frame's own document, which the click
    // may have replaced: that is the frame leaving, not the page.
    if(At(Stage::FrameSettle)) { FrameSettled(success,json);return; }
    auto parsed=CefParseJSON(json,JSON_PARSER_RFC);
    auto result=parsed && parsed->GetType()==VTYPE_DICTIONARY?parsed->GetDictionary():nullptr;
    // A script context that answers with neither a result nor an exception has
    // gone with its document, which happens when a document is replaced while
    // it is being read. The request is read again in a fresh context, once per
    // request; a page that moved on still fails as changed.
    const bool scripted=At(Stage::Evaluate) || At(Stage::GestureBegin) || At(Stage::Path) ||
      At(Stage::Hover) || At(Stage::Pointer) || At(Stage::GestureEnd);
    if(scripted && world_ && (!success || !result || (result->GetType("result")!=VTYPE_DICTIONARY && result->GetType("exceptionDetails")!=VTYPE_DICTIONARY))) {
      const auto document=contexts_.count(world_)?contexts_[world_]:0;
      contexts_.erase(world_);world_=0;
      if(document!=document_ && !pending_->retried) { pending_->retried=true;Begin();return; }
    }
    // The answer is handled where CEF delivered it, never from inside this
    // call's own dispatch: every method below is sent after the handler runs.
    if(At(Stage::NativeElement) || At(Stage::NativeDOM) || At(Stage::NativeDocument) ||
        At(Stage::NativeNode) || At(Stage::NativeEditors) || At(Stage::NativeResolve) || At(Stage::NativeConfigure) ||
        At(Stage::FileTarget) || At(Stage::FileSet)) {
      NativeResult(success,result);return;
    }
    if(At(Stage::TextFailureFrameTree)) {
      std::vector<TextFrame> frames;
      auto tree=success && result?result->GetDictionary("frameTree"):nullptr;
      if(text_owner_ && CollectTextFrames(tree,frames)) {
        const auto owner=*text_owner_;
        const bool live=std::any_of(frames.begin(),frames.end(),[&](const TextFrame& candidate) {
          return candidate.id==owner.id && candidate.loader==owner.loader;
        });
        if(!live) { Fail("text_input_retired","The original checked frame was replaced");return; }
      }
      Fail("unavailable","The original text request was not acknowledged");return;
    }
    if(stage_>=kGestureBase) {
      if(stage_==Gesture(GestureStage::Tree)) {
        // A frame tree names the document a fresh world belongs to, and the
        // frames the gesture's steps can name by their place in it.
        auto tree=success && result?result->GetDictionary("frameTree"):nullptr;
        auto frame=tree?tree->GetDictionary("frame"):nullptr;
        if(!frame || frame->GetType("id")!=VTYPE_STRING || frame->GetType("loaderId")!=VTYPE_STRING) { Fail("unavailable","The page has no document to read");return; }
        tree_=tree->Copy(true);tree_generation_=generation_;
        // The page's own document is the one a step is placed in, and its
        // world is the one a frame's place is measured in: both come from the
        // tree read here, and a place read for another document is dropped.
        main_id_=frame->GetString("id").ToString();
        const auto loader=frame->GetString("loaderId").ToString();
        if(world_ && loader==world_loader_) { AdvancePointer();return; }
        main_world_=0;
        loader_=loader;world_=0;
        // A gesture that aims into a frame measures the frame's place in the
        // page's own world, which is made with the frame tree it comes from.
        BeginGestureWorld();return;
      }
      if(stage_==Gesture(GestureStage::World)) {
        if(!success || !result || result->GetType("executionContextId")!=VTYPE_INT) { Fail("unavailable","The page refused a script context");return; }
        // The world belongs to the document the tree named: another document
        // is another world, however the ids of the two read.
        world_=result->GetInt("executionContextId");world_id_=main_id_;world_loader_=loader_;
        main_world_=world_;
        document_=page_documents=page_documents%2147483647+1;
        AdvancePointer();return;
      }
      if(stage_==Gesture(GestureStage::PlaceWorld)) {
        if(!success || !result || result->GetType("executionContextId")!=VTYPE_INT) { Fail("unavailable","A frame of the page refused a script context");return; }
        frame_place_world_=result->GetInt("executionContextId");
        frame_place_document_=page_documents=page_documents%2147483647+1;
        // A place the page gave before this world was made places the step
        // now, in the frame's own document, where its element has a box.
        if(!frame_place_read_ || frame_place_id_!=place_frame_.id) { AdvancePointer();return; }
        auto command=CefDictionaryValue::Create();
        command->SetString("action","locate_box");command->SetInt("doc",frame_place_document_);
        command->SetString("element",PointerStep()->GetString("element"));
        CallInt(Gesture(GestureStage::Locate),"Runtime.evaluate",PointerEvaluate(command,frame_place_world_));
        return;
      }
      if(stage_==Gesture(GestureStage::Place)) {
        // The evaluate result differs from a page action's: its value is the
        // answer's own body, one level in from the action's result.
        auto remote=success && result?result->GetDictionary("result"):nullptr;
        auto value=remote && remote->GetType("value")==VTYPE_DICTIONARY?remote->GetDictionary("value"):nullptr;
        FrameOffsetValue(value);return;
      }
      auto remote=success && result?result->GetDictionary("result"):nullptr;
      auto value=remote && remote->GetType("value")==VTYPE_DICTIONARY?remote->GetDictionary("value"):nullptr;
      // A box read in a frame's document is the second half of the step's
      // placement: the frame's place was read first, and it is what moves the
      // box into the page, so the box is never taken for a point of the page.
      if(stage_==Gesture(GestureStage::Locate) && pointer_place_.id==place_frame_.id && !pointer_place_.id.empty()) {
        if(!value || !value->GetBool("ok")) {
          const auto code=value && value->GetType("code")==VTYPE_STRING?value->GetString("code").ToString():"stale_element";
          Fail(code.size()<=64?code:"stale_element",value?Text(value,"message"):std::string("That element is not on the page"));return;
        }
        auto data=value->GetType("data")==VTYPE_DICTIONARY?value->GetDictionary("data"):nullptr;
        if(!data) { Fail("script","The page gave no position for that element");return; }
        step_box_={Number(data,"x"),Number(data,"y")};step_located_=true;
        // The box is a point of the frame's own document; the step's place in
        // the page is that box moved by where the frame sits, which the place
        // read before this world was made already said.
        PointerLocated();return;
      }
      if(!value) {
        // The document this step named is gone: place the step in the page
        // that replaced it, once, before answering that the ref is stale.
        if(pending_->retried) { Fail("page_changed","The page changed during the gesture");return; }
        pending_->retried=true;world_=0;
        CallInt(Gesture(GestureStage::Tree),"Page.getFrameTree",CefRefPtr<CefDictionaryValue>());return;
      }
      if(!value->GetBool("ok")) {
        const auto code=value->GetType("code")==VTYPE_STRING?value->GetString("code").ToString():"script";
        Fail(code.size()<=64?code:"script",Text(value,"message"));return;
      }
      auto data=value->GetType("data")==VTYPE_DICTIONARY?value->GetDictionary("data"):nullptr;
      if(!data) { Fail("script","The page gave no position for that element");return; }
      // The box a step names is the input to its own placement: it is carried
      // to the point the step resolves to, not kept for a later one.
      step_box_={Number(data,"x"),Number(data,"y")};step_located_=true;
      PointerLocated();return;
    }
    if(At(Stage::FrameTree)) {
      auto frame=success && result?result->GetDictionary("frameTree"):nullptr;
      frame=frame?frame->GetDictionary("frame"):nullptr;
      if(!frame || frame->GetType("id")!=VTYPE_STRING || frame->GetType("loaderId")!=VTYPE_STRING) {
        Fail("unavailable","The page has no document to read");return;
      }
      main_id_=frame->GetString("id").ToString();
      // CEF owns a result value only for the callback that received it, so the
      // tree is copied before it is kept for the next action. The copy keeps
      // the whole tree, not only the node asked for, so a later action still
      // finds the page's own document in it.
      auto tree=result->GetDictionary("frameTree");
      tree_=tree->Copy(true);tree_generation_=generation_;
      // The tree leads straight on: which document this action runs in is the
      // next step, never the end of the action.
      AfterTree();return;
    } else if(At(Stage::TextWorld)) {
      if(!success || !result || result->GetType("executionContextId")!=VTYPE_INT) { ProbeTextFrame();return; }
      text_frame_.world=result->GetInt("executionContextId");
      text_frame_.document=page_documents=page_documents%2147483647+1;
      auto params=CefDictionaryValue::Create();
      // Observe focus only. No transcript or field content is extracted here.
      params->SetString("expression","document.hasFocus() && (()=>{let e=document.activeElement;while(e?.shadowRoot?.activeElement)e=e.shadowRoot.activeElement;return !!e && e.localName!=='iframe' && e.localName!=='frame';})()");
      params->SetInt("contextId",text_frame_.world);params->SetBool("returnByValue",true);params->SetBool("silent",true);
      Call(Stage::TextFocus,"Runtime.evaluate",params);
    } else if(At(Stage::TextFocus)) {
      auto remote=success && result?result->GetDictionary("result"):nullptr;
      if(!remote || remote->GetType("value")!=VTYPE_BOOL || !remote->GetBool("value")) { ProbeTextFrame();return; }
      Evaluate();
    } else if(At(Stage::World)) {
      if(!success || !result || result->GetType("executionContextId")!=VTYPE_INT) {
        Fail("unavailable","The page refused a script context");return;
      }
      // Which world this result is depends on the frame it was asked for, so
      // the page's world and the frame's never stand for each other.
      const int context=result->GetInt("executionContextId");
      const bool asked_for_page=world_request_==main_id_;
      world_request_.clear();
      const bool in_frame=world_frame_.id!=main_id_ && !PrivateText();
      if(in_frame) {
        if(asked_for_page) main_world_=context; else frame_world_=context;
        // A click inside a frame needs the frame's place in the page, which
        // only the page's own world can measure. Asking for it stays a second
        // step: it never retargets the action away from the frame.
        if(!main_world_) {
          world_request_=main_id_;
          auto params=CefDictionaryValue::Create();
          params->SetString("frameId",main_id_);params->SetString("worldName","tilo-page-actions");
          Call(Stage::World,"Page.createIsolatedWorld",params);return;
        }
        if(!frame_world_) { Fail("unavailable","The page refused a script context in this frame");return; }
        world_=*frame_world_;
      } else {
        world_=context;main_world_=context;
      }
      world_id_=world_frame_.id;
      document_=page_documents=page_documents%2147483647+1;
      if(!PrivateText()) contexts_[world_]=document_;
      // A gesture asks the page for a world of its own, and the step it was
      // placing waits on the place of its frame, never on the page's read.
      if(gesture_ && PlaceFramePending()) { PlaceFrameRun();return; }
      if(Gesturing()) { BeginGesture();return; }
      Evaluate();
    } else if(At(Stage::Fence)) {
      auto remote=success && result?result->GetDictionary("result"):nullptr;
      auto value=remote && remote->GetType("value")==VTYPE_DICTIONARY?remote->GetDictionary("value"):nullptr;
      // No answer means the document went away under the fence: a navigation
      // that began during the action has committed.
      Fenced(value?Text(value,"navigation"):navigations_!=pending_->navigations?"started":"possible");
    } else if(At(Stage::Pointer)) {
      // Where the page's own pointer rests is told around the gesture's own
      // step, which drives what happens next; its answer ends nothing.
      return;
    } else if(At(Stage::GestureBegin) || At(Stage::Hover) || At(Stage::GestureEnd) || At(Stage::Path)) {
      auto remote=success && result?result->GetDictionary("result"):nullptr;
      auto value=remote?remote->GetDictionary("value"):nullptr;
      if(!value || value->GetType("ok")!=VTYPE_BOOL) { Fail("script","The page did not return a result");return; }
      if(!value->GetBool("ok")) {
        const auto code=value->GetType("code")==VTYPE_STRING?value->GetString("code").ToString():"script";
        Fail(code.size()<=64?code:std::string("script"),Text(value,"message"));return;
      }
      if(At(Stage::Hover)) {
        // The stylesheet answer decides whether this page's hover has to be
        // mirrored, and the gesture resolves its ends next.
        pending_->hover_styles=value->GetType("need_styles")==VTYPE_BOOL && value->GetBool("need_styles");
        GesturePage(Stage::GestureBegin,PageGesture::Begin);return;
      }
      if(At(Stage::GestureBegin)) { GestureLanded(value);return; }
      if(At(Stage::Path)) { StartPath(value);return; }
      GestureFinished(value);
    } else {
      if(!success) {
        if(PrivateText()) {
          // A failed non-editing check may observe retirement, never replay an
          // evaluation or retarget. An ambiguous commit stays unknown.
          if(Action()=="check_text_input" || Action()=="start_text_input" || Action()=="cancel_text_input") {
            Call(Stage::TextFailureFrameTree,"Page.getFrameTree",nullptr);return;
          }
          Fail("unavailable","The original text request was not acknowledged");return;
        }
        // The document changed under a cached context. Retry once in the new
        // document; a ref from the old one then fails as stale.
        world_=0;world_id_.clear();loader_.clear();world_loader_.clear();frame_world_.reset();world_request_.clear();main_world_=0;
        if(gesture_) { Fail("page_changed","The page changed during the gesture");return; }
        if(pending_->retried) { Fail("page_changed","The page changed while it was being read");return; }
        pending_->retried=true;Begin();return;
      }
      if(At(Stage::FrameOffset)) { FrameOffsetResult(success,result);return; }

      auto remote=result?result->GetDictionary("result"):nullptr;
      auto value=remote && remote->GetType("value")==VTYPE_DICTIONARY?remote->GetDictionary("value"):nullptr;
      if(!value || value->GetType("ok")!=VTYPE_BOOL) { Fail("script","The page did not return a result");return; }
      if(!value->GetBool("ok")) {
        const auto code=value->GetType("code")==VTYPE_STRING?value->GetString("code").ToString():"script";
        if(code=="text_input_native_required" && PrivateText() && Action()=="prepare_text_input" &&
            pending_->command->GetBool("native_caret") && !native_attempted_) {
          native_attempted_=true;
          auto params=CefDictionaryValue::Create();
          params->SetString("expression","globalThis.__tiloPage.nativeTextInputElement()");
          params->SetString("objectGroup","tilo-private-native-caret");
          params->SetInt("contextId",text_frame_.world);params->SetBool("silent",true);
          Call(Stage::NativeElement,"Runtime.evaluate",params);return;
        }
        Fail(code.size()<=64?code:"script",Text(value,"message"));return;
      }
      Act(value);
    }
  }
  // Resolve only node IDs and an editor object in the existing private world.
  // No field contents, attributes or caret offsets are exported to the client; no
  // remote-debugging port or universal-access world is opened.
  void NativeResult(bool success,CefRefPtr<CefDictionaryValue> result) {
    if(Action()=="fill_files") { FileResult(success,result);return; }
    if(!success || !result) { Fail("not_editable","The original native caret cannot be observed");return; }
    auto params=CefDictionaryValue::Create();
    if(At(Stage::NativeElement)) {
      auto remote=result->GetDictionary("result");
      if(!remote || remote->GetType("objectId")!=VTYPE_STRING) { Fail("not_editable","The original native field was lost");return; }
      native_object_=remote->GetString("objectId").ToString();
      Call(Stage::NativeDOM,"DOM.enable",nullptr);
    } else if(At(Stage::NativeDOM)) {
      params->SetInt("depth",0);params->SetBool("pierce",false);
      Call(Stage::NativeDocument,"DOM.getDocument",params);
    } else if(At(Stage::NativeDocument)) {
      params->SetString("objectId",native_object_);
      Call(Stage::NativeNode,"DOM.requestNode",params);
    } else if(At(Stage::NativeNode)) {
      if(result->GetType("nodeId")!=VTYPE_INT || result->GetInt("nodeId")<=0) { Fail("not_editable","The original native field was lost");return; }
      params->SetInt("nodeId",result->GetInt("nodeId"));params->SetBool("pierce",true);
      auto styles=CefListValue::Create();auto style=CefDictionaryValue::Create();
      style->SetString("name","-webkit-user-modify");style->SetString("value","read-write-plaintext-only");
      styles->SetDictionary(0,style);params->SetList("computedStyles",styles);
      Call(Stage::NativeEditors,"DOM.getNodesForSubtreeByStyle",params);
    } else if(At(Stage::NativeEditors)) {
      auto nodes=result->GetList("nodeIds");
      if(!nodes || nodes->GetSize()!=1 || nodes->GetType(0)!=VTYPE_INT) { Fail("not_editable","The native editor cannot be uniquely observed");return; }
      params->SetInt("nodeId",nodes->GetInt(0));params->SetInt("executionContextId",text_frame_.world);
      params->SetString("objectGroup","tilo-private-native-caret");
      Call(Stage::NativeResolve,"DOM.resolveNode",params);
    } else if(At(Stage::NativeResolve)) {
      auto object=result->GetDictionary("object");
      if(!object || object->GetType("objectId")!=VTYPE_STRING) { Fail("not_editable","The original native editor was lost");return; }
      params->SetString("objectId",object->GetString("objectId"));
      params->SetString("functionDeclaration","function(){return globalThis.__tiloPage.configureNativeTextEditor(this)}");
      params->SetBool("returnByValue",true);params->SetBool("silent",true);
      Call(Stage::NativeConfigure,"Runtime.callFunctionOn",params);
    } else {
      auto remote=result->GetDictionary("result");
      if(!remote || remote->GetType("value")!=VTYPE_BOOL || !remote->GetBool("value")) { Fail("text_input_changed","The original native field changed");return; }
      Evaluate();
    }
  }
  // The file input's files, in two steps: the isolated world's object for the
  // element (already validated by `fillFilesTarget`), then the files
  // themselves. `DOM.setFileInputFiles` is the browser's own
  // Enter-on-a-chooser path: the page receives trusted input and change
  // events, and scripts it runs see the files a person would have chosen. The
  // element is its object, never a node id: a node id belongs to the
  // inspected document, and no agent is attached for this world's document.
  void FileResult(bool success,CefRefPtr<CefDictionaryValue> result) {
    auto params=CefDictionaryValue::Create();
    if(At(Stage::FileTarget)) {
      auto remote=success && result?result->GetDictionary("result"):nullptr;
      // The element arrives as the object itself: an object is sent with its
      // own properties, so a wrapper would arrive without the element in it.
      if(!remote || remote->GetType("objectId")!=VTYPE_STRING) { Fail("page_changed","The file input is no longer on the page");return; }
      file_object_=remote->GetString("objectId").ToString();
      params->SetString("objectId",file_object_);
      params->SetList("files",pending_->command->GetList("paths")->Copy());
      Call(Stage::FileSet,"DOM.setFileInputFiles",params);
    } else {
      if(!success) { Fail("not_set","The page refused the chosen files");return; }
      auto data=CefDictionaryValue::Create();data->SetList("files",FileNames());data->SetString("mode","dom");
      Succeed(pending_->message,std::nullopt,data);return;
    }
  }
  // Both ends of the gesture, in the view's own space, as the page resolved
  // them: the element's visible centre, or the point the caller gave.
  bool GesturePoints(CefRefPtr<CefDictionaryValue> value,CefPoint& from,CefPoint& to) const {
    const auto read=[&](const char* x,const char* y,CefPoint& point) {
      if(value->GetType(x)!=VTYPE_DOUBLE && value->GetType(x)!=VTYPE_INT) return false;
      point=CefPoint(static_cast<int>(std::lround(Number(value,x))),static_cast<int>(std::lround(Number(value,y))));
      return true;
    };
    return read("x","y",from) && (Action()!="drag" || read("to_x","to_y",to));
  }
  void GestureLanded(CefRefPtr<CefDictionaryValue> value) {
    // A drawing path resolves its points against the element's box before the
    // pointer moves at all, so it answers with the path and not two ends.
    if(Action()=="path") {
      // A contact path names an element and points, never two ends.
      if(!gesture_page(value)) {Fail("script","The page did not say where the gesture goes");return; }
      GesturePage(Stage::Path,PageGesture::Path);return;
    }
    CefPoint from(0,0),to(0,0);
    if(!GesturePoints(value,from,to)) { Fail("script","The page did not say where the gesture goes");return; }
    pending_->from_x=from.x;pending_->from_y=from.y;pending_->to_x=to.x;pending_->to_y=to.y;
    if(Action()=="hover") {
      // Rest the pointer on the element, then let a frame pass so the page's
      // hover styles and handlers have run before the page is read again.
      MovePointer(from,false);
      CefPostDelayedTask(TID_UI,base::BindOnce([](CefRefPtr<PageAutomation> self,uint64_t token) {
        if(self->pending_ && self->pending_->token==token) self->GesturePage(Stage::GestureEnd,PageGesture::End);
      },CefRefPtr<PageAutomation>(this),pending_->token),kHoverFrameMs);
      return;
    }
    PressPointer(from,true);
    // The page's own two ends for a drag it replays itself, and the element
    // it left, so that replay starts from the element the agent named.
    pending_->drag_fallback=value->Copy(true);
    pending_->step=0;
    // The press starts the page's own drag, which the engine opens a turn later
    // (`StartDragging`). The moves have to wait for it, or the release goes out
    // before the drag exists and the page is left holding one nobody ends.
    CefPostDelayedTask(TID_UI,base::BindOnce([](CefRefPtr<PageAutomation> self) { self->DragStep(); },CefRefPtr<PageAutomation>(this)),kPathPointMs);
  }
  // One move of a drag, evenly spaced between its ends, then the release.
  void DragStep() {
    if(!pending_) return;
    const int steps=std::max(1,pending_->steps);
    if(pending_->step<steps) {
      ++pending_->step;
      const double at=static_cast<double>(pending_->step)/steps;
      const CefPoint point(
        static_cast<int>(std::lround(pending_->from_x+(pending_->to_x-pending_->from_x)*at)),
        static_cast<int>(std::lround(pending_->from_y+(pending_->to_y-pending_->from_y)*at)));
      MovePointer(point,true);
      CefPostTask(TID_UI,base::BindOnce([](CefRefPtr<PageAutomation> self) { self->DragStep(); },CefRefPtr<PageAutomation>(this)));
      return;
    }
    const CefPoint end(pending_->to_x,pending_->to_y);
    MovePointer(end,true);
    // The person's button comes up where the drop happened, so the page sees
    // the release it would see from a hand.
    PressPointer(end,false);
    CefPostDelayedTask(TID_UI,base::BindOnce([](CefRefPtr<PageAutomation> self,uint64_t token) {
      if(!self->pending_ || self->pending_->token!=token) return;
      // Where the source is still in place nothing took the drop, so the page's
      // own drag and drop handlers are replayed at the same two points.
      self->DragFallback();
    },CefRefPtr<PageAutomation>(this),pending_->token),kReleaseFrameMs);
  }
  // The drop the page replayed itself, over the same two ends, so its own
  // dragstart, dragover and drop handlers move what a person's drag would.
  void DragFallback() {
    auto command=CefDictionaryValue::Create();
    command->SetInt("doc",document_);command->SetString("action","drag_fallback");
    // The element the drag left, and the one it was dropped on, resolved by the
    // page itself when the gesture began.
    const auto& begin=pending_->drag_fallback;
    const auto ref=[&](const char* key,const char* fallback) {
      if(begin->GetType(key)==VTYPE_STRING) return begin->GetString(key).ToString();
      return begin->GetType(fallback)==VTYPE_STRING?begin->GetString(fallback).ToString():std::string();
    };
    const auto source=ref("from","element"),target=ref("to","to_target");
    if(!source.empty()) command->SetString("element",source);
    if(!target.empty()) command->SetString("to",target);
    // The ends the engine's pointer actually used, in the view's space.
    const double zoom=std::pow(1.2,browser_->GetHost()->GetZoomLevel());
    command->SetDouble("x",pending_->from_x/zoom);command->SetDouble("y",pending_->from_y/zoom);
    command->SetDouble("to_x",pending_->to_x/zoom);command->SetDouble("to_y",pending_->to_y/zoom);
    auto wrapped=CefValue::Create();wrapped->SetDictionary(command);
    auto params=CefDictionaryValue::Create();
    params->SetString("expression",std::string(kPageAutomation)+"\n;globalThis.__tiloPage.run("+CefWriteJSON(wrapped,JSON_WRITER_DEFAULT).ToString()+")");
    params->SetInt("contextId",world_);params->SetBool("returnByValue",true);params->SetBool("userGesture",false);params->SetBool("silent",true);
    Call(Stage::GestureEnd,"Runtime.evaluate",params);
  }
  // A page drag nothing is driving is refused at the press it would start from
  // (`host.cc`'s `StartDragging`), so a move that arrives without a gesture is
  // ignored.
  void DragMoved() {
    if(!pending_ || (Action()!="drag" && Action()!="path")) return;
    DragStep();
  }
  // A drawing path: a press at the first point, a move through each of the
  // rest, and a release at the last, all in page pixels the page resolved
  // against the element's own box.
  void StartPath(CefRefPtr<CefDictionaryValue> value) {
    std::vector<std::pair<double,double>> path;
    if(!ReadPoints(value->GetList("points"),path)) { Fail("script","The page resolved no drawing path");return; }
    pending_->points=std::move(path);pending_->point_index=0;pending_->drawing=true;
    const auto& first=pending_->points.front();
    PressPointer(CefPoint(static_cast<int>(std::lround(first.first)),static_cast<int>(std::lround(first.second))),true);
    CefPostDelayedTask(TID_UI,base::BindOnce([](CefRefPtr<PageAutomation> self) { self->PathStep(); },CefRefPtr<PageAutomation>(this)),kPathPointMs);
  }
  static bool ReadPoints(CefRefPtr<CefListValue> points,std::vector<std::pair<double,double>>& out) {
    if(!points || points->GetSize()<2 || points->GetSize()>1024) return false;
    for(size_t i=0;i<points->GetSize();++i) {
      auto pair=points->GetType(i)==VTYPE_LIST?points->GetList(i):nullptr;
      if(!pair || pair->GetSize()!=2) return false;
      // A whole number arrives as an int; a fractional one as a double.
      const auto coordinate=[&](size_t i) { return pair->GetType(i)==VTYPE_DOUBLE?pair->GetDouble(i):static_cast<double>(pair->GetInt(i)); };
      if((pair->GetType(0)!=VTYPE_DOUBLE && pair->GetType(0)!=VTYPE_INT) ||
         (pair->GetType(1)!=VTYPE_DOUBLE && pair->GetType(1)!=VTYPE_INT)) return false;
      const double x=coordinate(0),y=coordinate(1);
      if(!std::isfinite(x) || !std::isfinite(y)) return false;
      out.emplace_back(x,y);
    }
    return true;
  }
  // One more move of the path, then the release at its last point.
  void PathStep() {
    if(!pending_ || !pending_->drawing) return;
    if(++pending_->point_index<pending_->points.size()) {
      const auto& point=pending_->points[pending_->point_index];
      MovePointer(CefPoint(static_cast<int>(std::lround(point.first)),static_cast<int>(std::lround(point.second))),true);
      CefPostDelayedTask(TID_UI,base::BindOnce([](CefRefPtr<PageAutomation> self) { self->PathStep(); },CefRefPtr<PageAutomation>(this)),kPathPointMs);
      return;
    }
    const auto& last=pending_->points.back();
    PressPointer(CefPoint(static_cast<int>(std::lround(last.first)),static_cast<int>(std::lround(last.second))),false);
    CefPostDelayedTask(TID_UI,base::BindOnce([](CefRefPtr<PageAutomation> self,uint64_t token) {
      if(self->pending_ && self->pending_->token==token) self->GesturePage(Stage::GestureEnd,PageGesture::End);
    },CefRefPtr<PageAutomation>(this),pending_->token),kReleaseFrameMs);
  }
  // The engine's pointer, at a point in the view's space.
  void MovePointer(CefPoint point,bool down) { PointerAt(point,"move",down); }
  void PressPointer(CefPoint point,bool down) { PointerAt(point,down?"down":"up",down); }
  void PointerAt(CefPoint point,const char* type,bool down) {
    if(!browser_ || !pointer_) return;
    auto d=CefDictionaryValue::Create();
    // The app's pointer ops take physical pixels; the view's space is DIPs.
    // The app's pointer step names what it does as `kind`; `down` says the
    // button stays held through the move that follows a press.
    d->SetString("kind",type);d->SetString("type",type);d->SetBool("down",down);
    d->SetDouble("x",point.x);d->SetDouble("y",point.y);
    pointer_(d);
    // The page's `:hover` styles follow the browser's own pointer, which an
    // isolated world cannot move, so the page is told where it came to rest:
    // over the element the button is held on, or nowhere once it came up.
    // A drag never rests the page's pointer: the engine's own drag is in flight
    // across these steps, and a script asked for while it is never answers, so
    // every later page action in this tab would wait for one that cannot come.
    if(Action()=="drag") return;
    RestPointer(down ? point : CefPoint(-1,-1));
  }
  // Tell the page where the pointer rests. Only a hover or a drag rests it:
  // every other action leaves the page as it was.
  void RestPointer(CefPoint point) {
    auto command=CefDictionaryValue::Create();
    command->SetString("action","pointer_rest");
    // Only a hover rests the page's pointer: a drag is running in the engine
    // while its steps go out, and asking for a script then never answers.
    if(Action()!="hover") return;
    // One DevTools method is answered at a time, and the gesture's own step is
    // already on its way: resting the pointer now would replace the call whose
    // answer ends the request, and the request would wait for one that never
    // comes. The rest is remembered and told after the gesture has ended.
    if(call_) { rest_point_=point;rest_pending_=true;return; }
    rest_pending_=false;
    const double zoom=std::pow(1.2,browser_->GetHost()->GetZoomLevel());
    if(point.x>=0 && point.y>=0) { command->SetDouble("x",point.x/zoom);command->SetDouble("y",point.y/zoom); }
    else command->SetBool("nowhere",true);
    auto wrapped=CefValue::Create();wrapped->SetDictionary(command);
    auto params=CefDictionaryValue::Create();
    params->SetString("expression",std::string(kPageAutomation)+"\n;globalThis.__tiloPage.run("+CefWriteJSON(wrapped,JSON_WRITER_DEFAULT).ToString()+")");
    params->SetInt("contextId",world_);params->SetBool("returnByValue",true);params->SetBool("userGesture",false);params->SetBool("silent",true);
    params->SetBool("awaitPromise",true);
    Call(Stage::Pointer,"Runtime.evaluate",params);
  }
  void GestureFinished(CefRefPtr<CefDictionaryValue> value) {
    const auto action=Action();
    std::optional<std::string> outline;
    if(const auto text=Text(value,"outline"); !text.empty()) outline=text;
    Succeed(Text(value,"message"),outline,value->GetType("data")==VTYPE_DICTIONARY?value->GetDictionary("data"):nullptr);
  }
  void Evaluate() {
    auto command=CefDictionaryValue::Create();
    const auto action=Action();
    const bool elementless=!pending_->command->HasKey("element");
    command->SetString("action",action=="click"?"locate":action=="press"?(elementless?"arm":"focus"):action);
    command->SetInt("doc",PrivateText()?text_frame_.document:document_);
    if(!PrivateText() && !world_frame_.number.empty()) command->SetString("frame",world_frame_.number);
    for(const char* key:{"element","text","key","lease","history_scope"}) if(pending_->command->HasKey(key)) command->SetString(key,pending_->command->GetString(key));
    for(const char* key:{"cross_origin","secret","native_caret","enabled"}) if(pending_->command->HasKey(key)) command->SetBool(key,pending_->command->GetBool(key));
    for(const char* key:{"x","y"}) if(pending_->command->HasKey(key)) command->SetDouble(key,Number(pending_->command,key));
    for(const char* key:{"quiet_ms","timeout_ms"}) if(pending_->command->HasKey(key)) command->SetInt(key,pending_->command->GetInt(key));
    if(pending_->command->HasKey("expires_ms")) command->SetDouble("expires_ms",Number(pending_->command,"expires_ms"));
    if(pending_->command->HasKey("values")) command->SetList("values",pending_->command->GetList("values")->Copy());
    if(pending_->command->HasKey("paths")) command->SetList("paths",pending_->command->GetList("paths")->Copy());
    auto wrapped=CefValue::Create();wrapped->SetDictionary(command);
    auto params=CefDictionaryValue::Create();
    params->SetString("expression",std::string(kPageAutomation)+"\n;globalThis.__tiloPage.run("+CefWriteJSON(wrapped,JSON_WRITER_DEFAULT).ToString()+")");
    params->SetInt("contextId",PrivateText()?text_frame_.world:world_);params->SetBool("returnByValue",true);
    // Private focus/check/start/edit does not grant a site transient activation.
    params->SetBool("userGesture",!PrivateText());params->SetBool("silent",true);
    // `settle` answers with a promise; everything else with a value.
    params->SetBool("awaitPromise",true);
    if(pending_->command->GetString("op")=="text_input") params->SetDouble("timeout",5000);
    Call(Stage::Evaluate,"Runtime.evaluate",params);
  }
  // Ask the main document where the frame now sits: the page script scrolls
  // the frame's element into view before it reads a point in the document.
  void MeasureFrameOffset(bool settling) {
    auto command=CefDictionaryValue::Create();auto wrapped=CefValue::Create();
    command->SetString("action","frame_offset");command->SetString("frame",world_frame_.number);
    wrapped->SetDictionary(command);
    auto params=CefDictionaryValue::Create();
    params->SetString("expression",std::string(kPageAutomation)+"\n;globalThis.__tiloPage.run("+CefWriteJSON(wrapped,JSON_WRITER_DEFAULT).ToString()+")");
    params->SetInt("contextId",main_world_);params->SetBool("returnByValue",true);params->SetBool("silent",true);
    settling_=settling;
    Call(Stage::FrameOffset,"Runtime.evaluate",params);
  }
  void FrameOffsetResult(bool success,CefRefPtr<CefDictionaryValue> result) {
    auto remote=result?result->GetDictionary("result"):nullptr;
    auto value=remote && remote->GetType("value")==VTYPE_DICTIONARY?remote->GetDictionary("value"):nullptr;
    FrameOffsetValue(value);
  }
  // Where the frame sits, once the page answered with its place.
  void FrameOffsetValue(CefRefPtr<CefDictionaryValue> value) {
    if(!value || value->GetType("ok")!=VTYPE_BOOL || !value->GetBool("ok")) {
      Fail(value && value->GetType("code")==VTYPE_STRING?value->GetString("code").ToString():"unavailable",
        value && value->GetType("message")==VTYPE_STRING?value->GetString("message").ToString()
          :"The frame moved off the page before it could be clicked");return;
    }
    const auto number=[&](const char* key){return Number(value,key);};
    world_frame_.x=number("x");world_frame_.y=number("y");
    // A gesture that aims into the frame sends real pointer input in the top
    // document, so its step lands at the point the frame's place moves it to.
    // The frame's own world reads the box and reports it moved by that place.
    if(gesture_) { FramePlaceRead(value);return; }
    ClickTarget(settling_);
  }
  void Act(CefRefPtr<CefDictionaryValue> value) {
    const auto action=Action();
    // A click inside a frame is a pointer event in the top document, so its
    // point is moved by the frame's place in the page. The frame is where the
    // click settles: the page's own script hears neither the pointer nor what
    // the frame does with it, so there is no page fence to run. The frame
    // answers the settle on its own step, after the click has reached it.
    const bool aimed_into_frame=action=="click" && !PrivateText() && !world_frame_.number.empty();
    if(aimed_into_frame && world_frame_.id!=main_id_) {
      const auto number=[&](const char* key){return value->GetType(key)==VTYPE_DOUBLE?value->GetDouble(key):value->GetInt(key);};
      clicked_={number("x"),number("y")};
      // The page around the frame hears of a frame's click as the frame's own
      // navigation, never as the page leaving its document.
      pending_->navigation="frame";
      MeasureFrameOffset(false);return;
    }
    if(PrivateText() && action=="prepare_text_input") {
      auto data=value?value->GetDictionary("data"):nullptr;
      if(!data || data->GetType("lease")!=VTYPE_STRING || data->GetType("multiline")!=VTYPE_BOOL) { Fail("script","The private preparation returned no field policy");return; }
      text_owner_=text_frame_;text_lease_=data->GetString("lease").ToString();
    }
    if(PrivateText() && (action=="cancel_text_input" || action=="commit_text_input" || action=="undo_text_input" || action=="redo_text_input")) { text_owner_.reset();text_lease_.clear(); }
    // An edit runs the page's input handlers.
    if(action=="commit_text_input" || action=="undo_text_input" || action=="redo_text_input") pending_->navigation="possible";
    // A read runs none of the page's code, so it cannot have led anywhere.
    if(action=="outline" || action=="changes") { pending_->navigation="none";Succeed(Text(value,"message"),Text(value,"outline"));return; }
    if(action=="fill_files") { FileTarget(value);return; }
    if(action=="at" || action=="settle" || action=="reader" || action=="prepare_text_input" || action=="start_text_input" || action=="check_text_input" || action=="commit_text_input" || action=="cancel_text_input" || action=="undo_text_input" || action=="redo_text_input") {
      Succeed(Text(value,"message"),std::nullopt,value->GetType("data")==VTYPE_DICTIONARY?value->GetDictionary("data"):nullptr,
        PrivateText() && value->GetType("history")==VTYPE_DICTIONARY?value->GetDictionary("history"):nullptr);return;
    }
    if(action=="click") {
      const auto number=[&](const char* key){return value->GetType(key)==VTYPE_DOUBLE?value->GetDouble(key):value->GetInt(key);};
      clicked_={number("x"),number("y")};
      // A click inside a frame needs the frame's place in the page, which
      // only the page's own world can measure.
      if(!world_frame_.number.empty()) { MeasureFrameOffset(true);return; }
      ClickTarget(false);
    } else if(action=="press") {
      auto key=CefDictionaryValue::Create();key->SetString("key",pending_->command->GetString("key"));
      for(const char* flag:{"ctrl","shift","alt","meta"}) key->SetBool(flag,pending_->command->GetBool(flag));
      key_(key);
    }
    if(action=="press") pending_->message="Pressed "+Chord();
    else if(action!="click") pending_->message=Text(value,"message");
    else pending_->message="Clicked "+Text(pending_->command,"element");
    // Answers once the page has handled the input: see `fence` in page_automation.js.
    auto params=CefDictionaryValue::Create();
    params->SetString("expression","globalThis.__tiloPage.fence()");
    params->SetInt("contextId",world_);params->SetBool("returnByValue",true);
    params->SetBool("awaitPromise",true);params->SetBool("silent",true);
    Call(Stage::Fence,"Runtime.evaluate",params);
  }
  void Fenced(const std::string& navigation) {
    pending_->navigation=navigation=="started"?"started":"possible";
    // A click that opened the page's file chooser is held, not shown: say so,
    // so the caller knows the page is waiting and how to answer it.
    if(file_dialog_)
      pending_->message+="; the page opened a file chooser, which is answered by fill-page-files for that element";
    if(pending_->navigation!="started") { Succeed(pending_->message,std::nullopt);return; }
    if(navigations_==pending_->navigations) pending_->awaiting_start=true;
    else if(browser_->IsLoading()) pending_->awaiting_load=true;
    else { Succeed(pending_->message,std::nullopt);return; }
    CefPostDelayedTask(TID_UI,base::BindOnce([](CefRefPtr<PageAutomation> self,uint64_t token) {
      if(self->pending_ && self->pending_->token==token) self->Succeed(self->pending_->message,std::nullopt);
    },CefRefPtr<PageAutomation>(this),pending_->token),kLoadWaitMs);
  }
  // A click inside a frame is a pointer event in the top document, so its point
  // is moved by the frame's place in the page. The frame is where the click
  // settles: the page's own script hears neither the pointer nor what the
  // frame does with it, so the frame is asked for its own settled state once
  // the click and this turn have run.
  void ClickTarget(bool settling) {
    Click click=click_;
    const double x=clicked_.x+world_frame_.x,y=clicked_.y+world_frame_.y;
    pending_->navigation=world_frame_.id!=main_id_?"frame":"possible";
    click(x,y);
    if(world_frame_.id==main_id_ || settling) return;
    auto params=CefDictionaryValue::Create();
    params->SetString("expression",std::string(kPageAutomation)+"\n;globalThis.__tiloPage.run({action:'settle',quiet_ms:250,timeout_ms:5000})");
    params->SetInt("contextId",world_);params->SetBool("returnByValue",true);params->SetBool("awaitPromise",true);params->SetBool("silent",true);
    CefPostTask(TID_UI,base::BindOnce([](CefRefPtr<PageAutomation> self,CefRefPtr<CefDictionaryValue> params) {
      if(self->pending_) self->Call(Stage::FrameSettle,"Runtime.evaluate",params);
    },CefRefPtr<PageAutomation>(this),params));
  }
  // A frame's settle answers from the frame's own document: the click may have
  // replaced it, which is the frame leaving and never the page.
  void FrameSettled(bool success,const std::string& json) {
    if(!pending_) return;
    auto parsed=CefParseJSON(json,JSON_PARSER_RFC);
    auto answer=parsed && parsed->GetType()==VTYPE_DICTIONARY?parsed->GetDictionary():nullptr;
    if(!success) {
      if(Text(answer,"message").find("Execution context was destroyed")==std::string::npos) {
        Fail("script","The frame did not answer in time");return;
      }
      Succeed(pending_->message,std::nullopt);return;
    }
    auto remote=answer?answer->GetDictionary("result"):nullptr;
    auto value=remote && remote->GetType("value")==VTYPE_DICTIONARY?remote->GetDictionary("value"):nullptr;
    const auto message=Text(value,"message");
    Succeed(message.empty()?pending_->message:message,std::nullopt);
  }
  std::string Chord() const {
    static constexpr std::pair<const char*,const char*> kModifiers[]={{"ctrl","Control+"},{"shift","Shift+"},{"alt","Alt+"},{"meta","Meta+"}};
    std::string chord;const auto& command=pending_->command;
    for(const auto& [flag,name]:kModifiers) if(command->GetBool(flag)) chord+=name;
    const auto key=command->GetString("key").ToString();
    return chord+(key==" "?"Space":key);
  }
  static std::string Text(CefRefPtr<CefDictionaryValue> value,const char* key) {
    return value && value->GetType(key)==VTYPE_STRING?value->GetString(key).ToString():std::string();
  }
  // A number the page wrote: the JSON object it arrives in holds it as either
  // a double or an int, and a key it never wrote counts as zero.
  static double Number(CefRefPtr<CefDictionaryValue> d,const char* key) {
    const auto type=d->GetType(key);
    return type==VTYPE_DOUBLE?d->GetDouble(key):type==VTYPE_INT?d->GetInt(key):0;
  }
  void Succeed(const std::string& message,std::optional<std::string> outline,CefRefPtr<CefDictionaryValue> data=nullptr,CefRefPtr<CefDictionaryValue> history=nullptr) {
    auto fields=CefDictionaryValue::Create();fields->SetBool("ok",true);
    fields->SetString("url",browser_->GetMainFrame()->GetURL());fields->SetString("message",message.substr(0,1024));
    // A navigation the page asked for that the browser never began is not one.
    const bool navigated=pending_->navigation=="started" && navigations_!=pending_->navigations;
    // A frame is not a way for the page to leave its document.
    const bool in_frame=pending_->navigation=="frame";
    fields->SetString("navigation",navigated && !in_frame?"started":pending_->navigation=="none"||in_frame?"none":"possible");
    fields->SetBool("navigated",navigated);fields->SetBool("loading",navigated && browser_->IsLoading());
    if(outline) fields->SetString("outline",*outline);else fields->SetNull("outline");
    if(data) fields->SetDictionary("data",data->Copy(false));
    if(history) fields->SetDictionary("history",history->Copy(false));
    Finish(fields);
  }
  static CefRefPtr<CefDictionaryValue> Failure(const std::string& code,const std::string& message) {
    auto fields=CefDictionaryValue::Create();fields->SetBool("ok",false);
    fields->SetString("code",code.empty()?"script":code);fields->SetString("message",message.substr(0,1024));
    return fields;
  }
  void Fail(const std::string& code,const std::string& message) {
    if(PrivateText() && code=="text_input_retired" && text_owner_ && pending_->command->GetString("lease").ToString()==text_lease_) {
      text_owner_.reset();text_lease_.clear();
    }
    Finish(Failure(code,message));
  }
  void Finish(CefRefPtr<CefDictionaryValue> fields) {
    const int request=pending_->request;const bool text_input=pending_->command->GetString("op")=="text_input";
    // A rest the gesture could not tell mid-step is told now, while the request
    // that asked for it is still the one running.
    if(rest_pending_) { rest_pending_=false;RestPointer(rest_point_);rest_pending_=false;call_=0; }
    pending_.reset();call_=0;
    if(native_attempted_) {
      auto params=CefDictionaryValue::Create();params->SetString("objectGroup","tilo-private-native-caret");
      browser_->GetHost()->ExecuteDevToolsMethod(0,"Runtime.releaseObjectGroup",params);
      native_object_.clear();native_attempted_=false;
    }
    // A chooser the finished action did not answer must not outlive it, or the
    // page would wait for input nobody is going to send.
    if(file_dialog_ && request==file_dialog_request_) CancelFileDialog();
    if(!file_object_.empty()) {
      auto params=CefDictionaryValue::Create();params->SetString("objectGroup","tilo-page-file-input");
      browser_->GetHost()->ExecuteDevToolsMethod(0,"Runtime.releaseObjectGroup",params);
      file_object_.clear();
    }
    Reply(request,fields,text_input);
  }
  void Reply(int request,CefRefPtr<CefDictionaryValue> fields,bool text_input=false) { fields->SetInt("request_id",request);emit_(text_input?"text_input_result":"page_result",fields); }
  CefRefPtr<CefBrowser> browser_;CefRefPtr<CefRegistration> registration_;
  Emit emit_;Click click_;Key key_;Pointer pointer_;
  std::optional<Pending> pending_;int stage_=static_cast<int>(Stage::FrameTree);int call_=0;
  // The stage that asked for that call, which its answer is answered in.
  int stage_at_call_=static_cast<int>(Stage::FrameTree);
  std::vector<TextFrame> text_candidates_;TextFrame text_frame_;std::optional<TextFrame> text_owner_;std::string text_lease_;
  bool native_attempted_=false;std::string native_object_;
  std::string file_object_;
  CefRefPtr<CefFileDialogCallback> file_dialog_;int file_dialog_request_=0;
  std::map<int,int> contexts_;uint64_t token_=0,navigations_=0;
  // Where the pointer came to rest, when the gesture was mid-step and the page
  // could not be told yet.
  CefPoint rest_point_{-1,-1};bool rest_pending_=false;
  void SetGesture(double x,double y) {gesture_x_=x;gesture_y_=y;}
  // The frame the pending place answers for: a step placed before it may name
  // another frame than the step now waiting.
  PageFrame pointer_place_;
  int world_=0,document_=0,main_world_=0;std::string main_id_;
  // The document a gesture's world was made in: a cached id is used only for
  // the document it belongs to.
  std::string world_loader_;
  // The frame's own context and the id it was made for, kept apart from the
  // page's so measuring the frame's place cannot retarget the action.
  std::optional<int> frame_world_;
  // The frame whose isolated world the pending request is creating, empty for
  // the world the action runs in.
  std::string world_request_;
  CefRefPtr<CefDictionaryValue> tree_;uint64_t tree_generation_=~uint64_t{0};uint64_t generation_=0;
  // The gesture's own step cursor and the last box an element resolved to.
  size_t pointer_index_=0;CefRefPtr<CefDictionaryValue> pointer_box_;
  // Whether the pending action is a pointer gesture, and the place of the
  // frame the gesture currently aims into.
  bool gesture_=false;double gesture_x_=0,gesture_y_=0;
  // What an element step of the pending gesture resolved to: its box when the
  // step was first placed, and the point it is stepped around.
  static constexpr size_t kGestureBoxes=256;
  struct HeldBoxEntry { std::string ref;double x=0,y=0; };
  std::vector<HeldBoxEntry> gesture_boxes_;
  Clicked step_box_;bool step_located_=false;
  // The gesture's frames, in the numbering the page printed.
  std::string loader_,world_id_;int frame_place_document_=0;PageFrame world_frame_;
  // The frame a gesture's step names, with the world its document is read in
  // and the frame's own place in the page.
  PageFrame place_frame_;
  // The world a named frame's own document is read in: a frame that replaced
  // its document gets a fresh one, so a step never runs in a document gone.
  int frame_place_world_=0;
  // The place the page gave for the frame it was read for, kept as a copy:
  // the answer's own body lives only for the callback that received it.
  CefRefPtr<CefDictionaryValue> frame_place_read_;
  // The frame the place kept above was read for: the world's answer places the
  // step with the place of the frame it was made for, never another frame's.
  std::string frame_place_id_;
  Clicked clicked_;bool settling_=false;
  IMPLEMENT_REFCOUNTING(PageAutomation);
};
}
