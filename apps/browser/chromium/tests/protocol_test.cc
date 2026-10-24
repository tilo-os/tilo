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
