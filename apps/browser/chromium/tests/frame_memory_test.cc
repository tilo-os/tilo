#include "frame_memory.h"
#include <cassert>
#include <cstdio>
#include <cstdlib>
#include <fcntl.h>
#include <string>
using namespace tilo;
namespace {
// Memory of `size` bytes, created as the app creates it (tilo-process's
// shared_memory): a memfd on Linux, an unlinked POSIX shared memory object on
// macOS, whose size macOS reports rounded up to its page.
int Memory(off_t size) {
#if defined(__APPLE__)
  const std::string name="/tilo-fm-"+std::to_string(getpid());
  const int fd=shm_open(name.c_str(),O_RDWR|O_CREAT|O_EXCL,0600);assert(fd>2);shm_unlink(name.c_str());
#else
  const int fd=memfd_create("tilo-frame-memory-test",MFD_CLOEXEC);assert(fd>2);
#endif
  assert(ftruncate(fd,size)==0);return fd;
}
uint32_t State(const FrameMemory& memory,uint32_t slot) {
  return __atomic_load_n(reinterpret_cast<uint32_t*>(memory.Base()+kFrameSlotStates+4*slot),__ATOMIC_ACQUIRE);
}
void Free(FrameMemory& memory,uint32_t slot) {
  __atomic_store_n(reinterpret_cast<uint32_t*>(memory.Base()+kFrameSlotStates+4*slot),kFrameSlotFree,__ATOMIC_RELEASE);
}
}
int main() {
  // Only the app's memory is taken, and its descriptor is closed either way.
  // Memory a page or more past it is not the app's.
  assert(!FrameMemory::Map(-1) && !FrameMemory::Map(2));
  { const int fd=Memory(4096);assert(!FrameMemory::Map(fd));assert(fcntl(fd,F_GETFD)<0); }
  { const int fd=Memory(kFrameMemoryBytes+sysconf(_SC_PAGESIZE));assert(!FrameMemory::Map(fd));assert(fcntl(fd,F_GETFD)<0); }
  // The app's memory is taken where the system rounds its size up to a page
  // larger than the header, as macOS does on Apple silicon.
  const int fd=Memory(kFrameMemoryBytes);
  auto memory=FrameMemory::Map(fd);assert(memory);assert(fcntl(fd,F_GETFD)<0);

  // A routed tab's frame lands in a free slot, which is ready until the app
  // frees it; the notice names tab, size and slot.
  std::string pixels(2*3*4,'\0');for(size_t i=0;i<pixels.size();++i) pixels[i]=static_cast<char>(i);
  const uint32_t last=kFrameSlots-1;
  assert(memory->FreeSlot(last)==0);
  auto notice=memory->Publish(0,7,FramePacket(7,2,3,pixels.data(),pixels.size()));
  assert(notice.size()==21 && U32(notice.data())==17 && uint8_t(notice[4])==kFrameNoticeKind);
  assert(U32(notice.data()+5)==7 && U32(notice.data()+9)==2 && U32(notice.data()+13)==3 && U32(notice.data()+17)==0);
  assert(std::string(memory->Base()+kFrameMemoryHeader,pixels.size())==pixels);
  assert(State(*memory,0)==kFrameSlotReady);

  // The slots take turns, and none is free while the app reads both.
  assert(memory->FreeSlot(0)==1);
  notice=memory->Publish(1,1,FramePacket(1,2,3,pixels.data(),pixels.size()));
  assert(!notice.empty() && U32(notice.data()+5)==1 && U32(notice.data()+17)==1);
  assert(std::string(memory->Base()+kFrameMemoryHeader+kFrameSlotBytes,pixels.size())==pixels);
  assert(memory->FreeSlot(1)==-1 && memory->FreeSlot(0)==-1);
  Free(*memory,0);assert(memory->FreeSlot(1)==0 && memory->FreeSlot(0)==0);
  Free(*memory,1);assert(memory->FreeSlot(0)==1);

  // A packet that is not a frame of the tab leaves the slot free.
  assert(memory->Publish(0,7,FramePacket(1,2,3,pixels.data(),pixels.size())).empty());
  assert(memory->Publish(0,7,"short").empty());
  assert(memory->Publish(kFrameSlots,7,FramePacket(7,2,3,pixels.data(),pixels.size())).empty());
  assert(State(*memory,0)==kFrameSlotFree);
  std::puts("frame memory: ok");
}
