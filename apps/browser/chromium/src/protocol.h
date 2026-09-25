#pragma once
#include <cstdint>
#include <cmath>
#include <istream>
#include <stdexcept>
#include <string>
namespace qos {
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
inline std::string EncodePacket(uint8_t kind,const std::string& data) {
  if(data.size()+1>kMaxPacket) throw std::length_error("IPC packet too large");
  std::string out; out.reserve(data.size()+5); AppendU32(out,static_cast<uint32_t>(data.size()+1)); out.push_back(kind); out+=data; return out;
}
}
