#include <atomic>
#include <cstdlib>
#include <cstring>
#include <dlfcn.h>
#include <new>
#include <unistd.h>
#include <execinfo.h>
static std::atomic<bool> armed{false};
using Write=ssize_t(*)(int,const void*,size_t);
static Write real_write;
__attribute__((constructor)) static void initialize(){
 real_write=reinterpret_cast<Write>(dlsym(RTLD_NEXT,"write"));
 void*frames[24];backtrace(frames,24);
 if(!real_write)_exit(125);
}
extern "C" ssize_t write(int fd,const void* data,size_t size){
 if(fd==1&&size<=16384&&memmem(data,size,"draft-fixed-language",20))armed.store(true);
 return real_write(fd,data,size);
}
void* operator new(size_t size){
 if(size==3200&&armed.exchange(false)){
  const char marker[]="PUBLIC_PROBE_INJECTED_FFT_ALLOCATION_FAILURE\n";
  real_write(2,marker,sizeof(marker)-1);
  void*frames[32];int count=backtrace(frames,32);backtrace_symbols_fd(frames,count,2);
  throw std::bad_alloc();
 }
 if(void*p=malloc(size?size:1))return p;
 throw std::bad_alloc();
}
