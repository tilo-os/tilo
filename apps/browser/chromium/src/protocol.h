#pragma once
#include <cstdint>
#include <cmath>
#include <istream>
#include <stdexcept>
#include <string>
namespace tilo {
constexpr uint32_t kMaxPacket = 64 * 1024 * 1024;
constexpr uint32_t kMaxDimension = 8192;
struct Packet { uint8_t kind = 0; std::string data; };
enum class ReadResult { Ok, Eof, Error };
inline bool ValidDocumentGeneration(double value) {
  return std::isfinite(value) && value>=1 && value<=9007199254740991.0 && std::trunc(value)==value;
}
inline void AppendU32(std::string& s, uint32_t v) { for (int i=0;i<4;++i) s.push_back(static_cast<char>(v >> (8*i))); }
inline uint32_t U32(const char* p) { uint32_t v=0; for (int i=0;i<4;++i) v |= uint32_t(static_cast<unsigned char>(p[i])) << (8*i); return v; }
inline size_t FrameSize(uint32_t w, uint32_t h) {
  if (!w || !h || w>kMaxDimension || h>kMaxDimension) return 0;
  uint64_t n=uint64_t(w)*h*4;
  return n+9 <= kMaxPacket ? static_cast<size_t>(n) : 0;
}
inline bool ValidFrame(const std::string& s) {
  if(s.size()<8) return false;
  size_t size=FrameSize(U32(s.data()),U32(s.data()+4));
  return size && s.size()==size+8;
}
inline ReadResult ReadPacket(std::istream& in, Packet& p) {
  char header[4]; in.read(header,4);
  if(in.gcount()==0 && in.eof()) return ReadResult::Eof;
  if(in.gcount()!=4) return ReadResult::Error;
  uint32_t n=U32(header); if(!n || n>kMaxPacket) return ReadResult::Error;
  char kind; if(!in.get(kind)) return ReadResult::Error;
  p.kind=static_cast<uint8_t>(kind); p.data.resize(n-1);
  if(n>1) { in.read(p.data.data(),n-1); if(in.gcount()!=n-1) return ReadResult::Error; }
  if(p.kind!=1 && p.kind!=2) return ReadResult::Error;
  if(p.kind==2 && !ValidFrame(p.data)) return ReadResult::Error;
  return ReadResult::Ok;
}
// Where a frame packet's pixels start: after its length, kind, tab (routed
// tabs only) and dimensions.
inline size_t FramePixelOffset(uint32_t tab) { return tab==1 ? 13 : 17; }
// One tab's frame as a whole packet, its pixels copied once: kind 2 for the
// first tab, kind 3 carrying the tab for the others. Empty when the pixels do
// not match the dimensions or the packet would pass the limit.
inline std::string FramePacket(uint32_t tab,uint32_t width,uint32_t height,const char* pixels,size_t size) {
  const size_t offset=FramePixelOffset(tab);
  if(!tab || size!=FrameSize(width,height) || !size || size+offset-4>kMaxPacket) return {};
  std::string out; out.reserve(offset+size);
  AppendU32(out,static_cast<uint32_t>(size+offset-4)); out.push_back(static_cast<char>(tab==1?2:3));
  if(tab!=1) AppendU32(out,tab);
  AppendU32(out,width); AppendU32(out,height); out.append(pixels,size); return out;
}
// At most `limit` bytes of UTF-8 `text`, cut where a character starts.
inline std::string BoundedText(std::string text,size_t limit) {
  if(text.size()<=limit) return text;
  size_t end=limit;while(end>0 && (static_cast<unsigned char>(text[end])&0xC0)==0x80) --end;
  text.resize(end);return text;
}
inline std::string EncodePacket(uint8_t kind,const std::string& data) {
  if(data.size()+1>kMaxPacket) throw std::length_error("IPC packet too large");
  std::string out; out.reserve(data.size()+5); AppendU32(out,static_cast<uint32_t>(data.size()+1)); out.push_back(kind); out+=data; return out;
}
}
