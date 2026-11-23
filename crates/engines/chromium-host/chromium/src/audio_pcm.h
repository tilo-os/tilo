#pragma once
#include <cmath>
#include <cstdint>
#include <string>

namespace tilo {

// CEF hands a page's sound over as planar 32-bit float (one array per
// channel). The app plays it through its governed output service, which takes
// 16-bit integers; the host interleaves and converts here so a packet is one
// compact buffer (little-endian, whole frames) and the app does the rest.
constexpr int kMaxAudioChannels = 8;
constexpr int kMaxAudioFrames = 48000;

// Interleaves `frames` frames of `channels` planar float channels into
// little-endian signed 16-bit PCM, clipping to [-1, 1]. Returns false (and an
// empty `out`) for input the wire cannot carry.
inline bool InterleavePcm16(const float* const* planar,int channels,int frames,std::string& out) {
  out.clear();
  if(!planar || channels<1 || channels>kMaxAudioChannels || frames<1 || frames>kMaxAudioFrames) return false;
  for(int c=0;c<channels;++c) if(!planar[c]) return false;
  out.resize(static_cast<size_t>(frames)*channels*2);
  size_t at=0;
  for(int f=0;f<frames;++f) for(int c=0;c<channels;++c) {
    float v=planar[c][f];
    if(!std::isfinite(v)) v=0;
    if(v>1) v=1;
    if(v<-1) v=-1;
    const int16_t s=static_cast<int16_t>(std::lrintf(v*32767.0f));
    out[at++]=static_cast<char>(s&0xff);
    out[at++]=static_cast<char>((s>>8)&0xff);
  }
  return true;
}

}  // namespace tilo
