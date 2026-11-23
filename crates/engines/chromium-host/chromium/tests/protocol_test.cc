#include "protocol.h"
#include "view_geometry.h"
#include "download_slots.h"
#include <cassert>
#include <sstream>
#include <iostream>
#include <limits>
using namespace tilo;
int main() {
  assert(ValidDocumentGeneration(1));
  assert(ValidDocumentGeneration(9007199254740991.0));
  for(double value:{0.0,-1.0,1.5,9007199254740992.0,1e100,
      std::numeric_limits<double>::infinity(),std::numeric_limits<double>::quiet_NaN()}) {
    assert(!ValidDocumentGeneration(value));
  }
  for (int n = 1; n < 4; ++n) {
    std::istringstream s(std::string(n, '\0')); Packet p;
    assert(ReadPacket(s, p) == ReadResult::Error);
  }
  for (uint32_t n : {0u, kMaxPacket + 1}) {
    std::string b; AppendU32(b,n); std::istringstream s(b); Packet p;
    assert(ReadPacket(s,p) == ReadResult::Error);
  }
  { std::istringstream s(""); Packet p; assert(ReadPacket(s,p) == ReadResult::Eof); }
  { std::string s; AppendU32(s,4); s += "\1{}"; std::istringstream in(s); Packet p; assert(ReadPacket(in,p) == ReadResult::Error); }
  { auto s=EncodePacket(1,"{\"op\":\"back\"}"); std::istringstream in(s); Packet p; assert(ReadPacket(in,p)==ReadResult::Ok); assert(p.kind==1 && p.data=="{\"op\":\"back\"}"); }
  assert(!FrameSize(0,1)); assert(!FrameSize(8193,1)); assert(!FrameSize(8192,8192));
  assert(FrameSize(2,3)==24);
  { std::string body; AppendU32(body,2); AppendU32(body,3); body.append(24,'x'); assert(ValidFrame(body)); body.pop_back(); assert(!ValidFrame(body)); }
  { std::string pixels(24,'p'); auto first=FramePacket(1,2,3,pixels.data(),pixels.size());
    assert(first.size()==FramePixelOffset(1)+24 && U32(first.data())==first.size()-4 && first[4]==2);
    assert(U32(first.data()+5)==2 && U32(first.data()+9)==3 && first.substr(13)==pixels);
    std::istringstream in(first); Packet p; assert(ReadPacket(in,p)==ReadResult::Ok); assert(p.kind==2 && ValidFrame(p.data));
    auto routed=FramePacket(7,2,3,pixels.data(),pixels.size());
    assert(routed.size()==FramePixelOffset(7)+24 && U32(routed.data())==routed.size()-4 && routed[4]==3);
    assert(U32(routed.data()+5)==7 && U32(routed.data()+9)==2 && U32(routed.data()+13)==3 && routed.substr(17)==pixels);
    assert(FramePacket(1,2,3,pixels.data(),23).empty()); assert(FramePacket(0,2,3,pixels.data(),24).empty()); }
  { std::istringstream in(EncodePacket(99,"x")); Packet p; assert(ReadPacket(in,p)==ReadResult::Error); }
  // A caret's blink goes as its own rect, not as the whole frame.
  { std::string pixels(4*3*4,'\0');for(size_t i=0;i<pixels.size();++i)pixels[i]=static_cast<char>(i);
    auto clipped=ClipDamage({{1,1,1,2},{9,0,1,1},{0,0,0,5}},4,3);
    assert(clipped.size()==1 && clipped[0].x==1 && clipped[0].y==1 && clipped[0].width==1 && clipped[0].height==2);
    auto packet=DamagePacket(7,4,3,pixels.data(),pixels.size(),clipped);
    assert(packet.size()==kDamageRectsOffset+16+2*4 && U32(packet.data())==packet.size()-4 && packet[4]==4);
    assert(U32(packet.data()+5)==7 && U32(packet.data()+9)==4 && U32(packet.data()+13)==3 && U32(packet.data()+17)==1);
    assert(U32(packet.data()+21)==1 && U32(packet.data()+25)==1 && U32(packet.data()+29)==1 && U32(packet.data()+33)==2);
    assert(packet.substr(37,4)==pixels.substr((1*4+1)*4,4) && packet.substr(41,4)==pixels.substr((2*4+1)*4,4));
    // Rects past the frame are cut to it; half the frame or more goes whole.
    auto edge=ClipDamage({{3,2,5,5}},4,3);assert(edge.size()==1 && edge[0].width==1 && edge[0].height==1);
    assert(ClipDamage({{0,0,2,3}},4,3).empty());assert(ClipDamage({},4,3).empty());
    // Many rects become their bounding box.
    std::vector<DamageRect> many;for(uint32_t i=0;i<kMaxDamageRects+1;++i)many.push_back({i,0,1,1});
    auto box=ClipDamage(many,64,64);assert(box.size()==1 && box[0].x==0 && box[0].width==kMaxDamageRects+1 && box[0].height==1);
    assert(DamagePacket(7,4,3,pixels.data(),pixels.size(),{{3,0,2,1}}).empty());
    assert(DamagePacket(7,4,3,pixels.data(),pixels.size()-1,clipped).empty());
    assert(DamagePacket(0,4,3,pixels.data(),pixels.size(),clipped).empty()); }
  // A page's question is kept to its bound without cutting a character.
  assert(BoundedText("short",16)=="short");
  assert(BoundedText("abcdef",3)=="abc");
  assert(BoundedText("a\xc3\xa9" "b",2)=="a");
  assert(BoundedText("a\xc3\xa9" "b",3)=="a\xc3\xa9");
  assert(BoundedText("\xe2\x82\xac",2).empty());
  {
    ViewGeometry geometry;
    assert(geometry.ResizePhysical(800,452,1.0)==ResizeEffect::Changed);
    assert(geometry.PaintMatches(800,452));
    assert(geometry.ResizePhysical(1600,904,2.0)==ResizeEffect::Changed);
    assert(!geometry.PaintMatches(800,452));
    assert(geometry.PaintMatches(1600,904));
    assert(geometry.ResizePhysical(1600,904,2.0)==ResizeEffect::Repaint);
    assert(geometry.ResizePhysical(801,451,2.0)==ResizeEffect::Changed);
    assert(!geometry.PaintMatches(801,451));
    assert(geometry.PaintMatches(802,452));
    assert(geometry.ResizePhysical(801,451,2.0)==ResizeEffect::Repaint);
  }
  {
    DownloadSlots slots;
    assert(slots.Admit(1));
    assert(slots.Admit(1));
    assert(slots.Size()==1);
    assert(slots.Stage(1,7));
    assert(slots.Stage(1,7));
    assert(slots.Size()==1 && slots.EarlyForOwner(7)==std::vector<int>({1}));
    slots.Promote(1);
    assert(slots.EarlyForOwner(7).empty() && slots.Size()==1);
    assert(slots.Stage(2,7));
    assert(slots.Stage(3,8));
    for(int id:slots.EarlyForOwner(7))slots.Settle(id);
    assert(!slots.Contains(2) && slots.Contains(3));
    slots.Settle(3);
    for(int id=2;id<=32;++id)assert(slots.Admit(id));
    assert(!slots.Admit(33));
    slots.Settle(1);
    assert(slots.Admit(33));
    assert(slots.Size()==32);
  }
  {
    DownloadProvenance provenance;
    provenance.Record(7,"https://same.test/file","GET");
    provenance.Record(7,"https://same.test/file","POST");
    assert(!provenance.TakeRetryable(7,"https://same.test/file","https://same.test/file"));
    provenance.Record(7,"https://same.test/file","GET");
    assert(!provenance.TakeRetryable(7,"https://same.test/file","https://same.test/file"));
    assert(!provenance.TakeRetryable(7,"https://same.test/file","https://same.test/file"));
    provenance.Record(7,"https://same.test/file","GET");
    assert(provenance.TakeRetryable(7,"https://same.test/file","https://same.test/file"));
    assert(!provenance.TakeRetryable(7,"https://unknown.test/file","https://unknown.test/file"));
    provenance.Record(7,"https://get.test/file","GET");
    assert(provenance.TakeRetryable(7,"https://get.test/file","https://get.test/file"));
    assert(!provenance.TakeRetryable(7,"https://get.test/file","https://get.test/file"));
    provenance.Record(7,"blob:https://get.test/id","GET");
    assert(!provenance.TakeRetryable(7,"blob:https://get.test/id","blob:https://get.test/id"));
  }
  {
    DownloadOwnerReservations owners;
    for(int owner=1;owner<=32;++owner) {
      assert(owners.CanAdmit(owner));
      owners.SetBusy(owner,true);
      assert(owners.Detach(owner));
      owners.SetBusy(owner,false); // Terminal, but OnBeforeClose is delayed.
    }
    assert(owners.Size()==32);
    assert(!owners.CanAdmit(33));
    // Replacement visible tabs remain valid, but cannot admit work that might
    // need a 33rd detached owner until physical destruction is acknowledged.
    owners.Destroy(1);
    assert(owners.CanAdmit(33));
    owners.SetBusy(33,true);  // Early update reserves ownership immediately.
    assert(owners.IsBusy(33)); // Promotion to a pending prompt stays busy.
    assert(owners.Detach(33));
    owners.SetBusy(33,false); // Terminal update queues an idle task.
    owners.SetBusy(33,true);  // New early admission makes that task stale.
    assert(!ShouldCloseDetached(owners.IsDetached(33),owners.IsBusy(33)));
    owners.SetBusy(33,false);
    assert(ShouldCloseDetached(owners.IsDetached(33),owners.IsBusy(33)));
  }
  std::cout << "protocol tests passed\n";
}
