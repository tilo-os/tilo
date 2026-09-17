#include "protocol.h"
#include "view_geometry.h"
#include <cassert>
#include <sstream>
#include <iostream>
using namespace qos;
int main() {
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
  { std::istringstream in(EncodePacket(99,"x")); Packet p; assert(ReadPacket(in,p)==ReadResult::Error); }
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
  std::cout << "protocol tests passed\n";
}
