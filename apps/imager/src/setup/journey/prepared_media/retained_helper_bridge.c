/* Test-only fixed privilege bridge. Never installed in a production image. */
#define _GNU_SOURCE
#include <sys/types.h>
#include <sys/stat.h>
#include <sys/prctl.h>
#include <sys/syscall.h>
#include <fcntl.h>
#include <unistd.h>
#include <signal.h>
#include <grp.h>
#include <errno.h>
#include <stdlib.h>
#include <string.h>
#include <stdint.h>
#include <stdio.h>
#include <openssl/evp.h>
#include "retained-helper-facts.h"
static void refuse_at(int line) {
  dprintf(STDERR_FILENO,"owned retained bridge refused at line %d (errno %d)\n",line,errno);
  _exit(121);
}
#define refuse() refuse_at(__LINE__)
static void directory(int fd, uid_t owner) {
  struct stat s;
  if (fstat(fd,&s) || !S_ISDIR(s.st_mode) || s.st_uid!=owner || (s.st_mode&0777)!=0700) refuse();
}
static void request(const char *path) {
  size_t n=strlen(ACTOR_TMP);
  if (strncmp(path,ACTOR_TMP,n) || path[n]!='/' || strlen(path)>4096) refuse();
  int dir=open(ACTOR_TMP,O_RDONLY|O_DIRECTORY|O_NOFOLLOW|O_CLOEXEC);
  if(dir<0) refuse();
  directory(dir,1000);
  struct stat anchor;
  if(fstat(dir,&anchor) || anchor.st_dev!=ACTOR_DEV || anchor.st_ino!=ACTOR_INO) refuse();
  char remaining[4097]; strcpy(remaining,path+n+1);
  char *part=remaining;
  for(;;) {
    char *slash=strchr(part,'/');
    if(slash) *slash=0;
    if(!*part || !strcmp(part,".") || !strcmp(part,"..")) refuse();
    if(!slash) {
      if(strcmp(part,"request.json")) refuse();
      int file=openat(dir,part,O_RDONLY|O_NOFOLLOW|O_NONBLOCK|O_CLOEXEC);
      struct stat s;
      if(file<0 || fstat(file,&s) || !S_ISREG(s.st_mode) || s.st_uid!=1000 || s.st_nlink!=1 || (s.st_mode&0777)!=0600 || s.st_size<=0 || s.st_size>32768) refuse();
      close(file); close(dir); return;
    }
    int next=openat(dir,part,O_RDONLY|O_DIRECTORY|O_NOFOLLOW|O_CLOEXEC);
    if(next<0) { refuse(); }
    directory(next,1000); close(dir); dir=next; part=slash+1;
  }
}
int main(int argc,char **argv) {
  /* setuid exec clears inherited PDEATHSIG. Reinstall immediately, before
     request traversal or helper hashing, and never adopt a replacement parent. */
  pid_t parent=getppid();
  if(parent<=1 || prctl(PR_SET_PDEATHSIG,SIGKILL) || getppid()!=parent) refuse();
  if(argc!=4 || strcmp(argv[1],ACTOR_PROGRAM) || strcmp(argv[2],"--tilo-home-capture-helper") || getuid()!=1000 || geteuid()!=0 || getgid()!=1001) refuse();
  gid_t groups[4]; int count=getgroups(4,groups);
  if(count!=2 || groups[0]!=1002 || groups[1]!=1003) refuse();
  request(argv[3]);
  int fd=open(HELPER_PROGRAM,O_RDONLY|O_NOFOLLOW|O_CLOEXEC);
  struct stat before,after;
  if(fd<0 || fstat(fd,&before) || !S_ISREG(before.st_mode) || before.st_uid!=0 || before.st_nlink!=1 || (before.st_mode&0777)!=0555 || before.st_dev!=HELPER_DEV || before.st_ino!=HELPER_INO || before.st_size!=HELPER_SIZE) refuse();
  EVP_MD_CTX *ctx=EVP_MD_CTX_new(); unsigned char digest[32]; unsigned int size=0;
  if(!ctx || EVP_DigestInit_ex(ctx,EVP_sha256(),NULL)!=1) refuse();
  unsigned char bytes[65536]; ssize_t got; uint64_t total=0;
  while((got=read(fd,bytes,sizeof(bytes)))>0) {
    total+=(uint64_t)got;
    if(total>HELPER_SIZE || EVP_DigestUpdate(ctx,bytes,(size_t)got)!=1) refuse();
  }
  if(got<0 || total!=HELPER_SIZE || EVP_DigestFinal_ex(ctx,digest,&size)!=1 || size!=32 || memcmp(digest,HELPER_SHA256,32)) refuse();
  EVP_MD_CTX_free(ctx);
  if(fstat(fd,&after) || memcmp(&before.st_dev,&after.st_dev,sizeof(before.st_dev)) || before.st_ino!=after.st_ino || before.st_size!=after.st_size || before.st_mtim.tv_sec!=after.st_mtim.tv_sec || before.st_mtim.tv_nsec!=after.st_mtim.tv_nsec || before.st_ctim.tv_sec!=after.st_ctim.tv_sec || before.st_ctim.tv_nsec!=after.st_ctim.tv_nsec) refuse();
  if(getppid()!=parent) refuse();
  if(setgroups(0,NULL) || setgid(0) || setuid(0) || getuid()!=0 || geteuid()!=0 || getgid()!=0 || getegid()!=0) refuse();
  if(prctl(PR_SET_PDEATHSIG,SIGKILL) || getppid()!=parent) refuse();
  /* This is fixed from the actual authenticated kernel caller above, never argv. */
  if(clearenv() || setenv("PKEXEC_UID","1000",1) || setenv("TILO_NATIVE_CAPTURE_DIAGNOSTICS","1",1)) refuse();
  if(fd!=3) { if(dup3(fd,3,O_CLOEXEC)<0) refuse(); close(fd); }
  if(syscall(SYS_close_range,4u,~0u,0)) refuse();
  char *args[]={HELPER_PROGRAM,"--tilo-home-capture-helper",argv[3],NULL};
  if(getppid()!=parent) refuse();
  syscall(SYS_execveat,3,"",args,environ,AT_EMPTY_PATH);
  refuse();
}
