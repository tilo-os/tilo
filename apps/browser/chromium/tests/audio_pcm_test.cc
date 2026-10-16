#include "audio_pcm.h"
#include <cassert>
#include <limits>

using namespace tilo;
static int16_t At(const std::string& s,size_t i) {
  return static_cast<int16_t>(static_cast<uint16_t>(static_cast<uint8_t>(s[2*i]))|(static_cast<uint16_t>(static_cast<uint8_t>(s[2*i+1]))<<8));
}
int main() {
  std::string out;
  // Planar float channels interleave frame by frame, little-endian.
  const float left[]={0.0f,0.5f,-0.5f},right[]={1.0f,-1.0f,0.25f};
  const float* planar[]={left,right};
  assert(InterleavePcm16(planar,2,3,out));
  assert(out.size()==12);
  assert(At(out,0)==0 && At(out,1)==32767);
  assert(At(out,2)==16384 && At(out,3)==-32767);
  assert(At(out,4)==-16384 && At(out,5)==8192);
  // Out-of-range and non-finite samples are clipped or silenced, never wrapped.
  const float wild[]={2.0f,-3.0f,std::numeric_limits<float>::quiet_NaN(),std::numeric_limits<float>::infinity()};
  const float* mono[]={wild};
  assert(InterleavePcm16(mono,1,4,out));
  assert(At(out,0)==32767 && At(out,1)==-32767 && At(out,2)==0 && At(out,3)==0);
  // What the wire cannot carry is refused with an empty buffer.
  assert(!InterleavePcm16(nullptr,2,3,out) && out.empty());
  assert(!InterleavePcm16(planar,0,3,out));
  assert(!InterleavePcm16(planar,kMaxAudioChannels+1,3,out));
  assert(!InterleavePcm16(planar,2,0,out));
  assert(!InterleavePcm16(planar,2,kMaxAudioFrames+1,out));
  const float* hole[]={left,nullptr};
  assert(!InterleavePcm16(hole,2,3,out));
}
