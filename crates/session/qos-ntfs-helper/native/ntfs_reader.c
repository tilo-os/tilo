/* GPL-2.0-or-later. No path-opening adapter and no mutating device authority.
 */
#include "ntfs_reader.h"
#include "config.h"
#include <errno.h>
#include <fcntl.h>
#include <inttypes.h>
#include <stdarg.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/stat.h>
#include <unistd.h>
#ifdef __linux__
#include <linux/fs.h>
#endif
#ifdef __APPLE__
#include <sys/disk.h>
#endif
#include "attrib.h"
#include "bootsect.h"
#include "device.h"
#include "dir.h"
#include "index.h"
#include "inode.h"
#include "logfile.h"
#include "logging.h"
#include "security.h"
#include "acls.h"
#include "volume.h"

#define MAX_ENTRIES 4096
#define MAX_METADATA (1024 * 1024)
struct state {
  int fd, failure, mode;
  uint64_t capacity, cursor, limit, output;
  struct qos_stats *stats;
  qos_emit emit;
  void *context;
  ntfschar *upcase;
};
static int fail(struct state *s, int status, int error) {
  if (!s->failure)
    s->failure = status;
  errno = error;
  return -1;
}
static int event(struct state *s, const char *format, ...) {
  if (s->failure)
    return -1;
  char buf[16384];
  va_list args;
  va_start(args, format);
  int n = vsnprintf(buf, sizeof buf, format, args);
  va_end(args);
  if (n < 0 || (size_t)n >= sizeof buf ||
      s->output + (uint64_t)n > 4 * 1024 * 1024)
    return fail(s, 6, EFBIG);
  s->output += (uint64_t)n;
  if (s->emit(s->context, buf, (size_t)n))
    return fail(s, 6, EFBIG);
  return 0;
}
static void hex(const void *bytes, size_t length, char *out) {
  const unsigned char *b = bytes;
  const char *digits = "0123456789abcdef";
  for (size_t i = 0; i < length; ++i) {
    out[2 * i] = digits[b[i] >> 4];
    out[2 * i + 1] = digits[b[i] & 15];
  }
  out[2 * length] = 0;
}
static int dev_open(struct ntfs_device *d, int flags) {
  struct state *s = d->d_private;
  if ((flags & O_ACCMODE) != O_RDONLY ||
      (fcntl(s->fd, F_GETFL) & O_ACCMODE) != O_RDONLY)
    return fail(s, 7, EROFS);
  NDevSetOpen(d);
  NDevSetReadOnly(d);
  return 0;
}
static int dev_close(struct ntfs_device *d) {
  NDevClearOpen(d);
  return 0;
} /* Rust owns FD */
static s64 dev_pread(struct ntfs_device *d, void *buf, s64 count, s64 offset) {
  struct state *s = d->d_private;
  if (s->failure) {
    errno = EIO;
    return -1;
  }
  if (count < 0 || offset < 0 || (uint64_t)offset > s->capacity ||
      (uint64_t)count > s->capacity - (uint64_t)offset)
    return fail(s, 5, EIO);
  if (s->stats->calls >= 31744 || (uint64_t)count > s->limit - s->stats->bytes)
    return fail(s, 6, EFBIG);
  ++s->stats->calls;
  s->stats->bytes += (uint64_t)count;
  /* Exact attempted range is diagnostic evidence, bounded by total output. */
  if (event(s,
            "{\"kind\":\"read\",\"offset\":%" PRIu64 ",\"length\":%" PRIu64 "}",
            (uint64_t)offset, (uint64_t)count))
    return -1;
  ssize_t n;
  do {
    n = pread(s->fd, buf, (size_t)count, (off_t)offset);
  } while (n < 0 && errno == EINTR);
  if (n != count)
    return fail(s, 5, EIO);
  return n;
}
static s64 dev_seek(struct ntfs_device *d, s64 offset, int whence) {
  struct state *s = d->d_private;
  int64_t base = whence == SEEK_SET   ? 0
                 : whence == SEEK_CUR ? (int64_t)s->cursor
                 : whence == SEEK_END ? (int64_t)s->capacity
                                      : -1;
  if (base < 0 || offset < -base || (offset > 0 && offset > INT64_MAX - base) ||
      (uint64_t)(base + offset) > s->capacity) {
    errno = EINVAL;
    return -1;
  }
  s->cursor = (uint64_t)(base + offset);
  return (s64)s->cursor;
}
static s64 dev_read(struct ntfs_device *d, void *buf, s64 count) {
  struct state *s = d->d_private;
  s64 n = dev_pread(d, buf, count, (s64)s->cursor);
  if (n > 0)
    s->cursor += (uint64_t)n;
  return n;
}
static s64 dev_write(struct ntfs_device *d, const void *buf, s64 count) {
  (void)buf;
  (void)count;
  struct state *s = d->d_private;
  ++s->stats->writes;
  return fail(s, 7, EROFS);
}
static s64 dev_pwrite(struct ntfs_device *d, const void *buf, s64 count,
                      s64 offset) {
  (void)offset;
  return dev_write(d, buf, count);
}
static int dev_sync(struct ntfs_device *d) {
  if (NDevDirty(d))
    return (int)dev_write(d, NULL, 0);
  return 0;
}
static int dev_stat(struct ntfs_device *d, struct stat *st) {
  struct state *s = d->d_private;
  return fstat(s->fd, st);
}
static int dev_ioctl(struct ntfs_device *d, unsigned long request, void *arg) {
  struct state *s = d->d_private;
  /* Only synthesized, already validated geometry. Never forward any ioctl. */
#ifdef BLKGETSIZE64
  if (request == BLKGETSIZE64) {
    *(uint64_t *)arg = s->capacity;
    return 0;
  }
#endif
#ifdef DKIOCGETBLOCKCOUNT
  if (request == DKIOCGETBLOCKCOUNT) {
    *(uint64_t *)arg = s->capacity / 512;
    return 0;
  }
  if (request == DKIOCGETBLOCKSIZE) {
    *(uint32_t *)arg = 512;
    return 0;
  }
#endif
  (void)s;
  (void)request;
  (void)arg;
  errno = ENOTTY;
  return -1;
}
static struct ntfs_device_operations ops = {
    dev_open,  dev_close,  dev_seek, dev_read, dev_write,
    dev_pread, dev_pwrite, dev_sync, dev_stat, dev_ioctl};
static int chunks(struct state *s, ntfs_attr *a, uint64_t record,
                  uint32_t attribute, const char *name, uint64_t size) {
  unsigned char b[4096];
  char encoded[8193];
  for (uint64_t offset = 0; offset < size;) {
    uint64_t n = size - offset;
    if (n > sizeof b)
      n = sizeof b;
    if (ntfs_attr_pread(a, (s64)offset, (s64)n, b) != (s64)n)
      return fail(s, 5, EIO);
    hex(b, (size_t)n, encoded);
    if (event(s,
              "{\"kind\":\"chunk\",\"record\":%" PRIu64
              ",\"attribute\":%u,\"name\":\"%s\",\"offset\":%" PRIu64
              ",\"hex\":\"%s\"}",
              record, attribute, name, offset, encoded))
      return -1;
    offset += n;
  }
  return 0;
}
static int raw_chunks(struct state *s, const void *data, uint64_t size,
                      uint64_t record, uint32_t type, const char *name,
                      uint16_t instance) {
  const unsigned char *p = data;
  char encoded[8193];
  for (uint64_t offset = 0; offset < size;) {
    uint64_t n = size - offset;
    if (n > 4096)
      n = 4096;
    hex(p + offset, (size_t)n, encoded);
    if (event(s,
              "{\"kind\":\"chunk\",\"record\":%" PRIu64
              ",\"attribute\":%u,\"name\":\"%s\",\"instance\":%u,\"offset\":"
              "%" PRIu64 ",\"hex\":\"%s\"}",
              record, type, name, instance, offset, encoded))
      return -1;
    offset += n;
  }
  return 0;
}
/* Resolve the actual $SII/$SDS bytes directly. The upstream convenience ACL
 * getter fabricates a minimum-rights descriptor on failure; that fallback is
 * forbidden here. */
static int sds_security(struct state *s, ntfs_inode *ni, uint64_t *raw) {
  ntfschar sii[] = {cpu_to_le16('$'), cpu_to_le16('S'), cpu_to_le16('I'),
                    cpu_to_le16('I')};
  ntfschar sds[] = {cpu_to_le16('$'), cpu_to_le16('S'), cpu_to_le16('D'),
                    cpu_to_le16('S')};
  ntfs_inode *secure = ntfs_inode_open(ni->vol, FILE_Secure);
  ntfs_index_context *ctx = secure ? ntfs_index_ctx_get(secure, sii, 4) : NULL;
  ntfs_attr *a = NULL;
  unsigned char *bytes = NULL;
  int rc = -1;
  SII_INDEX_KEY key = {ni->security_id};
  if (!ctx || ntfs_index_lookup(&key, sizeof key, ctx))
    goto out;
  INDEX_ENTRY *entry = ctx->entry;
  size_t length = le16_to_cpu(entry->length),
         offset = le16_to_cpu(entry->data_offset);
  if (offset > length || sizeof(SECURITY_DESCRIPTOR_HEADER) > length - offset ||
      le16_to_cpu(entry->data_length) != sizeof(SECURITY_DESCRIPTOR_HEADER))
    goto out;
  SECURITY_DESCRIPTOR_HEADER header;
  memcpy(&header, (char *)entry + offset, sizeof header);
  uint64_t pos = le64_to_cpu(header.offset);
  uint32_t total = le32_to_cpu(header.length);
  if (s->mode && header.security_id == ni->security_id && total > sizeof header &&
      total > MAX_METADATA - *raw) {
    rc = event(s, "{\"kind\":\"incomplete\",\"record\":%" PRIu64
                  ",\"reason\":\"metadata-budget\"}", ni->mft_no);
    goto out;
  }
  if (header.security_id != ni->security_id || total <= sizeof header ||
      total > MAX_METADATA - *raw) {
    fail(s, 6, EFBIG);
    goto out;
  }
  a = ntfs_attr_open(secure, AT_DATA, sds, 4);
  if (!a || a->data_size < 0 || pos > (uint64_t)a->data_size ||
      total > (uint64_t)a->data_size - pos)
    goto out;
  bytes = malloc(total);
  if (!bytes) {
    fail(s, 6, ENOMEM);
    goto out;
  }
  if (ntfs_attr_pread(a, (s64)pos, total, bytes) != total ||
      memcmp(bytes, &header, sizeof header))
    goto out;
  const char *descriptor = (const char *)bytes + sizeof header;
  unsigned size = total - sizeof header;
  if (!ntfs_valid_descr(descriptor, size) ||
      ntfs_security_hash((const SECURITY_DESCRIPTOR_RELATIVE *)descriptor,
                         size) != header.hash)
    goto out;
  *raw += total;
  rc = event(s,
             "{\"kind\":\"security\",\"record\":%" PRIu64
             ",\"source\":\"sds\",\"security_id\":%u,\"size\":%u}",
             ni->mft_no, le32_to_cpu(ni->security_id), size);
  if (!rc)
    rc = raw_chunks(s, descriptor, size, ni->mft_no, 0x50, "", 0);
out:
  free(bytes);
  if (a)
    ntfs_attr_close(a);
  if (ctx)
    ntfs_index_ctx_put(ctx);
  if (secure && ntfs_inode_close(secure))
    rc = -1;
  if (rc && !s->failure)
    fail(s, 5, EIO);
  return rc;
}
static int check_hiber(struct state *s, ntfs_volume *v) {
  ntfs_inode *root = ntfs_inode_open(v, FILE_root);
  if (!root)
    return fail(s, 5, EIO);
  ntfschar name[] = {cpu_to_le16('h'), cpu_to_le16('i'), cpu_to_le16('b'),
                     cpu_to_le16('e'), cpu_to_le16('r'), cpu_to_le16('f'),
                     cpu_to_le16('i'), cpu_to_le16('l'), cpu_to_le16('.'),
                     cpu_to_le16('s'), cpu_to_le16('y'), cpu_to_le16('s')};
  errno = 0;
  u64 ref = ntfs_inode_lookup_by_name(root, name, 12);
  int lookup_errno = errno;
  if (ntfs_inode_close(root))
    return fail(s, 5, EIO);
  if (ref == (u64)-1) {
    if (lookup_errno == ENOENT)
      return event(s, "{\"kind\":\"hibernation\",\"state\":\"absent\"}");
    return fail(s, 5, EIO);
  }
  ntfs_inode *ni = ntfs_inode_open(v, ref);
  ntfs_attr *a = ni ? ntfs_attr_open(ni, AT_DATA, AT_UNNAMED, 0) : NULL;
  unsigned char b[4096];
  int ok = a && a->data_size >= 4096 &&
           ntfs_attr_pread(a, 0, sizeof b, b) == sizeof b;
  if (ok)
    for (size_t i = 0; i < sizeof b; ++i)
      if (b[i]) {
        ok = 0;
        break;
      }
  if (a)
    ntfs_attr_close(a);
  if (ni && ntfs_inode_close(ni))
    ok = 0;
  if (!ok)
    return fail(s, 4, EPERM);
  return event(s, "{\"kind\":\"hibernation\",\"state\":\"zero-header\"}");
}
int qos_check_logfile_strict(ntfs_attr *attr);
static int check_log(struct state *s, ntfs_volume *v) {
  ntfs_inode *ni = ntfs_inode_open(v, FILE_LogFile);
  ntfs_attr *a = ni ? ntfs_attr_open(ni, AT_DATA, AT_UNNAMED, 0) : NULL;
  int ok = a && qos_check_logfile_strict(a);
  if (a)
    ntfs_attr_close(a);
  if (ni && ntfs_inode_close(ni))
    ok = 0;
  if (!ok)
    return fail(s, 3, EIO);
  return event(s, "{\"kind\":\"logfile\",\"state\":\"clean\"}");
}
struct item {
  MFT_REF ref;
  uint64_t parent;
  unsigned depth;
  int name_len, name_type;
  ntfschar name[255];
};
struct walk {
  struct state *s;
  struct item *items;
  unsigned count;
  unsigned parent_depth;
  uint64_t parent;
};
static int found(void *context, const ntfschar *name, int length, int name_type,
                 s64 pos, MFT_REF ref, unsigned type) {
  (void)pos;
  (void)type;
  struct walk *w = context;
  if ((length == 1 && le16_to_cpu(name[0]) == '.') ||
      (length == 2 && le16_to_cpu(name[0]) == '.' &&
       le16_to_cpu(name[1]) == '.'))
    return 0;
  if (length < 1 || length > 255)
    return fail(w->s, 5, EIO);
  if (w->count >= MAX_ENTRIES || w->parent_depth >= 64)
    return fail(w->s, 6, EFBIG);
  struct item *i = &w->items[w->count++];
  i->ref = ref;
  i->parent = w->parent;
  i->depth = w->parent_depth + 1;
  i->name_len = length;
  i->name_type = name_type;
  memcpy(i->name, name, (size_t)length * 2);
  return 0;
}
static int attributes(struct state *s, ntfs_inode *ni) {
  ntfs_attr_search_ctx *ctx = ntfs_attr_get_search_ctx(ni, NULL);
  if (!ctx)
    return fail(s, 5, EIO);
  unsigned count = 0;
  uint64_t raw = 0;
  int rc = 0;
  int inline_security = 0;
  while (!ntfs_attrs_walk(ctx)) {
    if (++count > 4096) {
      rc = fail(s, 6, EFBIG);
      break;
    }
    ATTR_RECORD *a = ctx->attr;
    uint32_t len = le32_to_cpu(a->length), type = le32_to_cpu(a->type);
    uint16_t no = le16_to_cpu(a->name_offset);
    uintptr_t begin = (uintptr_t)ctx->mrec,
              end = begin + ni->vol->mft_record_size, ptr = (uintptr_t)a;
    if (ptr < begin || ptr > end || len > end - ptr || len < 24 || no > len ||
        (uint32_t)a->name_length * 2 > len - no) {
      rc = fail(s, 5, EIO);
      break;
    }
    char name[1021];
    ntfschar *aname = (ntfschar *)((char *)a + no);
    hex(aname, (size_t)a->name_length * 2, name);
    s64 size = a->non_resident ? sle64_to_cpu(a->data_size)
                               : (s64)le32_to_cpu(a->value_length);
    if (size < 0) {
      rc = fail(s, 5, EIO);
      break;
    }
    if (event(s,
              "{\"kind\":\"attribute\",\"record\":%" PRIu64
              ",\"type\":%u,\"flags\":%u,\"name\":\"%s\",\"size\":%" PRIu64
              ",\"instance\":%u}",
              ni->mft_no, type, le16_to_cpu(a->flags), name, (uint64_t)size,
              le16_to_cpu(a->instance))) {
      rc = -1;
      break;
    }
    if(a->flags & ATTR_IS_ENCRYPTED){
      if(event(s,"{\"kind\":\"incomplete\",\"record\":%"PRIu64",\"reason\":\"encrypted-attribute\"}",ni->mft_no)){rc=-1;break;}
      continue;
    }
    /* Native system entries can be huge; explicitly scoped as uncollected. */
    if ((ni->mft_no < 16 && ni->mft_no != FILE_root) ||
        (a->non_resident && sle64_to_cpu(a->lowest_vcn) > 0)) {
      if (event(s,
                "{\"kind\":\"incomplete\",\"record\":%" PRIu64
                ",\"reason\":\"system-or-extent-data\"}",
                ni->mft_no)) {
        rc = -1;
        break;
      }
      continue;
    }
    if (s->mode && a->type == AT_DATA && !a->name_length &&
        !(ni->mrec->flags & MFT_RECORD_IS_DIRECTORY))
      continue;
    if (s->mode && (uint64_t)size > MAX_METADATA - raw) {
      if (event(s, "{\"kind\":\"incomplete\",\"record\":%" PRIu64
                   ",\"reason\":\"metadata-budget\"}", ni->mft_no)) { rc = -1; break; }
      continue;
    }
    if ((uint64_t)size > MAX_METADATA - raw) {
      rc = fail(s, 6, EFBIG);
      break;
    }
    raw += (uint64_t)size;
    if (!a->non_resident) {
      uint16_t vo = le16_to_cpu(a->value_offset);
      if (vo > len || (uint64_t)size > len - vo) {
        rc = fail(s, 5, EIO);
        break;
      }
      const void *value = (const char *)a + vo;
      if (a->type == AT_SECURITY_DESCRIPTOR) {
        if (!ntfs_valid_descr(value, (unsigned)size)) {
          rc = fail(s, 5, EIO);
          break;
        }
        inline_security = 1;
        if (event(s,
                  "{\"kind\":\"security\",\"record\":%" PRIu64
                  ",\"source\":\"attribute\",\"size\":%" PRIu64 "}",
                  ni->mft_no, (uint64_t)size)) {
          rc = -1;
          break;
        }
      }
      rc = raw_chunks(s, value, (uint64_t)size, ni->mft_no, type, name,
                      le16_to_cpu(a->instance));
      if (rc)
        break;
      continue;
    }
    ntfs_attr *na = ntfs_attr_open(ni, a->type, aname, a->name_length);
    if (!na || na->data_size != size) {
      if (na)
        ntfs_attr_close(na);
      rc = fail(s, 5, EIO);
      break;
    }
    if(a->type==AT_SECURITY_DESCRIPTOR){
      char *descriptor=malloc((size_t)size);
      if(!descriptor||ntfs_attr_pread(na,0,size,descriptor)!=size||!ntfs_valid_descr(descriptor,(unsigned)size))rc=fail(s,5,EIO);
      else {
        inline_security=1;
        rc=event(s,"{\"kind\":\"security\",\"record\":%"PRIu64",\"source\":\"attribute\",\"size\":%"PRIu64"}",ni->mft_no,(uint64_t)size);
        if(!rc)rc=raw_chunks(s,descriptor,(uint64_t)size,ni->mft_no,type,name,le16_to_cpu(a->instance));
      }free(descriptor);
    }else rc = chunks(s, na, ni->mft_no, type, name, (uint64_t)size);
    ntfs_attr_close(na);
    if (rc)
      break;
  }
  if (!rc && errno != ENOENT)
    rc = fail(s, 5, EIO);
  ntfs_attr_put_search_ctx(ctx);
  if (!rc && (ni->mft_no >= 16 || ni->mft_no == FILE_root)) {
    if (test_nino_flag(ni, v3_Extensions) && ni->security_id)
      rc = sds_security(s, ni, &raw);
    else if (!inline_security)
      rc = event(s,
                 "{\"kind\":\"incomplete\",\"record\":%" PRIu64
                 ",\"reason\":\"security-descriptor-absent\"}",
                 ni->mft_no);
  }
  if (!rc && s->mode)
    rc = event(s, "{\"kind\":\"attributes_end\",\"record\":%" PRIu64
                  ",\"sequence\":%u}", ni->mft_no, le16_to_cpu(ni->mrec->sequence_number));
  return rc;
}
static int tree(struct state *s, ntfs_volume *v, uint64_t root) {
  struct item *items = calloc(MAX_ENTRIES, sizeof *items);
  if (!items)
    return fail(s, 6, ENOMEM);
  struct walk w = {s, items, 1, 0, 0};
  items[0].ref = root;
  int rc = 0;
  for (unsigned n = 0; n < w.count; ++n) {
    struct item *i = &items[n];
    ntfs_inode *ni = ntfs_inode_open(v, i->ref);
    if (!ni) {
      rc = fail(s, 5, EIO);
      break;
    }
    char name[1021], key[1021];
    ntfschar canon[255];
    hex(i->name, (size_t)i->name_len * 2, name);
    for (int c = 0; c < i->name_len; ++c)
      canon[c] = s->upcase[le16_to_cpu(i->name[c])];
    hex(canon, (size_t)i->name_len * 2, key);
    rc = event(s,
               "{\"kind\":\"entry\",\"record\":%" PRIu64
               ",\"sequence\":%u,\"parent\":%" PRIu64
               ",\"name\":\"%s\",\"key\":\"%s\",\"namespace\":%d,\"links\":%u,"
               "\"dos_flags\":%u,\"directory\":%s}",
               ni->mft_no, le16_to_cpu(ni->mrec->sequence_number), i->parent,
               name, key, i->name_type, le16_to_cpu(ni->mrec->link_count),
               le32_to_cpu(ni->flags),
               (ni->mrec->flags & MFT_RECORD_IS_DIRECTORY) ? "true" : "false");
    if (!rc && ((ni->flags & FILE_ATTR_REPARSE_POINT) || le16_to_cpu(ni->mrec->link_count)>1))
      rc=event(s,"{\"kind\":\"incomplete\",\"record\":%"PRIu64",\"reason\":\"reparse-or-hardlink-group\"}",ni->mft_no);
    if (!rc)
      rc = attributes(s, ni);
    if (!rc && (ni->mrec->flags & MFT_RECORD_IS_DIRECTORY) &&
        !(ni->flags & FILE_ATTR_REPARSE_POINT) &&
        i->name_type != FILE_NAME_DOS) {
      int repeated = 0;
      for (unsigned k = 0; k < n; ++k)
        if (MREF(items[k].ref) == ni->mft_no)
          repeated = 1;
      if (repeated)
        rc = fail(s, 5, ELOOP);
      else {
        w.parent_depth = i->depth;
        w.parent = ni->mft_no;
        s64 position = 0;
        if (ntfs_readdir(ni, &position, &w, found))
          rc = fail(s, 5, EIO);
        if (!rc && s->mode)
          rc = event(s, "{\"kind\":\"directory_end\",\"record\":%" PRIu64
                       ",\"sequence\":%u}", ni->mft_no, le16_to_cpu(ni->mrec->sequence_number));
      }
    }
    if (ntfs_inode_close(ni) && !rc)
      rc = fail(s, 5, EIO);
    if (rc)
      break;
  }
  free(items);
  return rc;
}

/* Every read reopens native full MFT references. No pointer survives the worker. */
static int read_identity(struct state *s, ntfs_volume *v,
                         const struct qos_ntfs_read_request *r,
                         ntfs_inode **object) {
  ntfs_inode *root = ntfs_inode_open(v, r->root_record | ((uint64_t)r->root_sequence << 48));
  int ok = root && root->mft_no == r->root_record &&
      le16_to_cpu(root->mrec->sequence_number) == r->root_sequence &&
      (root->mrec->flags & MFT_RECORD_IS_DIRECTORY) &&
      le64_to_cpu(root->creation_time) == r->root_creation &&
      !(root->flags & FILE_ATTR_REPARSE_POINT);
  if (root && ntfs_inode_close(root)) ok = 0;
  if (!ok) return fail(s, 5, EIO);
  *object = ntfs_inode_open(v, r->object_record | ((uint64_t)r->object_sequence << 48));
  ntfs_inode *ni = *object;
  if (!ni || ni->mft_no != r->object_record ||
      le16_to_cpu(ni->mrec->sequence_number) != r->object_sequence ||
      (ni->mrec->flags & MFT_RECORD_IS_DIRECTORY) ||
      (ni->flags & (FILE_ATTR_REPARSE_POINT | FILE_ATTR_ENCRYPTED | FILE_ATTR_COMPRESSED | FILE_ATTR_SPARSE_FILE)))
    return fail(s, 5, EIO);
  return 0;
}
static int read_range(struct state *s, ntfs_volume *v, const NTFS_BOOT_SECTOR *boot,
                      const struct qos_ntfs_read_request *r) {
  if (!r || r->reserved || !r->length || r->length > 1048576 ||
      r->size > INT64_MAX || r->offset > r->size || r->length > r->size - r->offset ||
      !r->root_sequence || !r->object_sequence ||
      r->root_record > 0xffffffffffffULL || r->object_record > 0xffffffffffffULL ||
      le64_to_cpu(boot->volume_serial_number) != r->serial)
    return fail(s, 5, EINVAL);
  ntfs_inode *ni = NULL;
  ntfs_attr *a = NULL;
  int rc = read_identity(s, v, r, &ni);
  if (!rc) {
    a = ntfs_attr_open(ni, AT_DATA, AT_UNNAMED, 0);
    if (!a || a->data_size < 0 || (uint64_t)a->data_size != r->size ||
        a->initialized_size < 0 || a->initialized_size > a->data_size ||
        a->allocated_size < a->data_size ||
        (a->data_flags & (ATTR_IS_COMPRESSED | ATTR_IS_ENCRYPTED | ATTR_IS_SPARSE)))
      rc = fail(s, 5, EIO);
  }
  if (!rc) rc = attributes(s, ni);
  unsigned char b[4096]; char encoded[8193];
  for (uint64_t offset = 0; !rc && offset < r->length;) {
    uint64_t n = r->length - offset;
    if (n > sizeof b) n = sizeof b;
    if (ntfs_attr_pread(a, (s64)(r->offset + offset), (s64)n, b) != (s64)n) {
      rc = fail(s, 5, EIO); break;
    }
    hex(b, (size_t)n, encoded);
    rc = event(s, "{\"kind\":\"data\",\"offset\":%" PRIu64 ",\"hex\":\"%s\"}", offset, encoded);
    offset += n;
  }
  if (a) ntfs_attr_close(a);
  if (ni && ntfs_inode_close(ni)) rc = fail(s, 5, EIO);
  ni = NULL;
  if (!rc) rc = read_identity(s, v, r, &ni);
  if (!rc) rc = attributes(s, ni);
  if (ni && ntfs_inode_close(ni)) rc = fail(s, 5, EIO);
  NTFS_BOOT_SECTOR after;
  if (!rc && (dev_pread(v->dev, &after, sizeof after, 0) < 0 || memcmp(boot, &after, sizeof after)))
    rc = fail(s, 5, EIO);
  return rc;
}

static int probe(int fd, uint64_t capacity, uint64_t root,
                   uint64_t read_limit, qos_emit emit, void *context,
                   struct qos_stats *stats, int mode,
                   const struct qos_ntfs_read_request *request) {
  ntfs_log_set_handler(ntfs_log_handler_null);
  struct state s = {.fd = fd, .mode = mode,
                    .capacity = capacity,
                    .limit = read_limit < 256 * 1024 * 1024 ? read_limit
                                                            : 256 * 1024 * 1024,
                    .stats = stats,
                    .emit = emit,
                    .context = context};
  if (capacity > INT64_MAX || !emit || !stats ||
      (fcntl(fd, F_GETFL) & O_ACCMODE) != O_RDONLY)
    return 7;
  struct ntfs_device *d =
      ntfs_device_alloc("inherited-readonly-descriptor", 0, &ops, &s);
  if (!d)
    return 6;
  NTFS_BOOT_SECTOR boot;
  if (dev_pread(d, &boot, sizeof boot, 0) < 0) {
    ntfs_device_free(d);
    return s.failure;
  }
  if (!memcmp(&boot.oem_id, "-FVE-FS-", 8)) {
    ntfs_device_free(d);
    return 2;
  }
  if (memcmp(&boot.oem_id, "NTFS    ", 8)) {
    ntfs_device_free(d);
    return 1;
  }
  ntfs_volume *v = ntfs_device_mount(d, NTFS_MNT_RDONLY);
  if (!v) {
    ntfs_device_free(d);
    return s.failure ? s.failure : 5;
  }
  if (v->major_ver != 3 || v->minor_ver != 1 || le16_to_cpu(v->flags))
    fail(&s, 3, EPERM);
  if (!s.failure)
    event(&s,
          "{\"kind\":\"volume\",\"serial\":\"%016" PRIx64
          "\",\"sector_size\":%u,\"cluster_size\":%u,\"mft_record_size\":%u,"
          "\"flags\":%u,\"major\":%u,\"minor\":%u}",
          le64_to_cpu(boot.volume_serial_number), v->sector_size,
          v->cluster_size, v->mft_record_size, le16_to_cpu(v->flags),
          v->major_ver, v->minor_ver);
  if (!s.failure && mode != 2)
    check_log(&s, v);
  if (!s.failure && mode != 2)
    check_hiber(&s, v);
  if (!s.failure && mode != 2) {
    ntfs_inode *ni = ntfs_inode_open(v, FILE_UpCase);
    ntfs_attr *a = ni ? ntfs_attr_open(ni, AT_DATA, AT_UNNAMED, 0) : NULL;
    s.upcase = malloc(131072);
    if (!a || !s.upcase || a->data_size != 131072 ||
        ntfs_attr_pread(a, 0, 131072, s.upcase) != 131072)
      fail(&s, 5, EIO);
    else
      chunks(&s, a, FILE_UpCase, 0x80, "", 131072);
    if (a)
      ntfs_attr_close(a);
    if (ni && ntfs_inode_close(ni))
      fail(&s, 5, EIO);
  }
  if (!s.failure && mode != 2)
    tree(&s, v, root);
  if (!s.failure && mode == 2)
    read_range(&s, v, &boot, request);
  if (ntfs_umount(v, 0))
    fail(&s, 5, EIO);
  free(s.upcase);
  return s.failure;
}

int qos_ntfs_probe(int fd, uint64_t capacity, uint64_t root, uint64_t read_limit,
                   qos_emit emit, void *context, struct qos_stats *stats) {
  return probe(fd, capacity, root, read_limit, emit, context, stats, 0, NULL);
}
int qos_ntfs_manifest(int fd, uint64_t capacity, uint64_t root, uint64_t read_limit,
                      qos_emit emit, void *context, struct qos_stats *stats) {
  return probe(fd, capacity, root, read_limit, emit, context, stats, 1, NULL);
}
int qos_ntfs_read_chunk(int fd, uint64_t capacity, const struct qos_ntfs_read_request *request,
                       uint64_t read_limit, qos_emit emit, void *context, struct qos_stats *stats) {
  return probe(fd, capacity, 0, read_limit, emit, context, stats, 2, request);
}
