#pragma once
// A page's frames reach the app through memory both processes map, not
// through the output pipe: copying sixteen megabytes into a pipe and out of it
// again cost two kernel copies and a write per 64 KiB of pipe. The app creates
// the memory (a sealed memfd on Linux, an unlinked POSIX shared memory object
// on macOS), hands its descriptor to the host once, at spawn
// (`--frame-memory-fd=N`), and the host maps it before anything else opens a
// descriptor.
//
// Layout: a header page, then kFrameSlots slots of kFrameSlotBytes each. The
// header holds one 32-bit state per slot, at kFrameSlotStates + 4 * slot. The
// host writes a frame's BGRA pixels into a free slot, marks it ready and sends
// a frame notice packet (kind 5: tab, width, height, slot); the app copies the
// pixels out and marks the slot free again. A slot is only ever written by the
// process whose turn it is, so neither waits on a lock.
#include "protocol.h"
#include <cstdint>
#include <cstring>
#include <memory>
#include <string>
#include <sys/mman.h>
#include <sys/stat.h>
#include <unistd.h>
namespace tilo {
constexpr size_t kFrameMemoryHeader=4096;
constexpr size_t kFrameSlotStates=64;
// Every frame FramePacket accepts fits one slot.
constexpr size_t kFrameSlotBytes=kMaxPacket;
constexpr uint32_t kFrameSlots=2;
constexpr size_t kFrameMemoryBytes=kFrameMemoryHeader+kFrameSlots*kFrameSlotBytes;
constexpr uint32_t kFrameSlotFree=0,kFrameSlotReady=1;
constexpr uint8_t kFrameNoticeKind=5;

class FrameMemory {
 public:
  // Maps the app's frame memory and closes `fd`. Null when `fd` is not a
  // descriptor of that much memory: the host then sends its frames down the
  // pipe, as a host started without one does.
  static std::unique_ptr<FrameMemory> Map(int fd) {
    if(fd<=2) return nullptr;
    struct stat info{};
    void* base=MAP_FAILED;
    if(fstat(fd,&info)==0 && SizedForFrames(static_cast<uint64_t>(info.st_size)))
      base=mmap(nullptr,kFrameMemoryBytes,PROT_READ|PROT_WRITE,MAP_SHARED,fd,0);
    close(fd);
    if(base==MAP_FAILED) return nullptr;
    return std::unique_ptr<FrameMemory>(new FrameMemory(static_cast<char*>(base)));
  }
  ~FrameMemory() { munmap(base_,kFrameMemoryBytes); }
  FrameMemory(const FrameMemory&)=delete;
  FrameMemory& operator=(const FrameMemory&)=delete;
  // A slot the app has finished reading, the one after `after` first so the
  // slots take turns; -1 while the app still reads both.
  int FreeSlot(uint32_t after) const {
    for(uint32_t step=1;step<=kFrameSlots;++step) {
      const uint32_t slot=(after+step)%kFrameSlots;
      if(__atomic_load_n(State(slot),__ATOMIC_ACQUIRE)==kFrameSlotFree) return static_cast<int>(slot);
    }
    return -1;
  }
  // Copies the pixels of a frame packet (FramePacket) into `slot`, a free one,
  // marks it ready and returns the notice to send. Empty, with the slot left
  // free, when the packet is not a frame of `tab`.
  std::string Publish(uint32_t slot,uint32_t tab,const std::string& packet) {
    const size_t offset=FramePixelOffset(tab);
    if(slot>=kFrameSlots || !tab || packet.size()<offset) return {};
    const uint32_t width=U32(packet.data()+offset-8),height=U32(packet.data()+offset-4);
    const size_t size=FrameSize(width,height);
    if(!size || size>kFrameSlotBytes || packet.size()!=offset+size) return {};
    std::memcpy(Pixels(slot),packet.data()+offset,size);
    __atomic_store_n(State(slot),kFrameSlotReady,__ATOMIC_RELEASE);
    std::string notice;
    AppendU32(notice,tab);AppendU32(notice,width);AppendU32(notice,height);AppendU32(notice,slot);
    return EncodePacket(kFrameNoticeKind,notice);
  }
  // The memory as the app sees it, for tests.
  char* Base() const { return base_; }
 private:
  explicit FrameMemory(char* base):base_(base) {}
  // The app sizes the memory to kFrameMemoryBytes. macOS reports a shared
  // memory object's size rounded up to its page, 16 KiB on Apple silicon,
  // where the header page is 4 KiB: that rounding is the app's memory too.
  static bool SizedForFrames(uint64_t size) {
    const long page=sysconf(_SC_PAGESIZE);
    const uint64_t rounding=page>0 ? static_cast<uint64_t>(page) : 1;
    return size>=kFrameMemoryBytes && size-kFrameMemoryBytes<rounding;
  }
  uint32_t* State(uint32_t slot) const { return reinterpret_cast<uint32_t*>(base_+kFrameSlotStates+4*size_t(slot)); }
  char* Pixels(uint32_t slot) const { return base_+kFrameMemoryHeader+size_t(slot)*kFrameSlotBytes; }
  char* base_;
};
}
