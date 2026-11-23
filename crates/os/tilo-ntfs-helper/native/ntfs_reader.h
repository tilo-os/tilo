#ifndef TILO_NTFS_READER_H
#define TILO_NTFS_READER_H
#include <stddef.h>
#include <stdint.h>
typedef int (*tilo_emit)(void *, const char *, size_t);
struct tilo_stats {
  uint64_t bytes, calls, writes;
};
int tilo_ntfs_probe(int fd, uint64_t capacity, uint64_t root,
                   uint64_t read_limit, tilo_emit emit, void *context,
                   struct tilo_stats *stats);

struct tilo_ntfs_read_request {
  uint64_t serial, root_record, root_creation, object_record, size, offset;
  uint32_t length;
  uint16_t root_sequence, object_sequence;
  uint64_t reserved;
};
_Static_assert(sizeof(struct tilo_ntfs_read_request) == 64, "read request ABI");
_Static_assert(offsetof(struct tilo_ntfs_read_request, length) == 48, "length ABI");
_Static_assert(offsetof(struct tilo_ntfs_read_request, root_sequence) == 52, "sequence ABI");
_Static_assert(offsetof(struct tilo_ntfs_read_request, reserved) == 56, "reserved ABI");
int tilo_ntfs_manifest(int fd, uint64_t capacity, uint64_t root, uint64_t read_limit,
                      tilo_emit emit, void *context, struct tilo_stats *stats);
int tilo_ntfs_read_chunk(int fd, uint64_t capacity, const struct tilo_ntfs_read_request *request,
                       uint64_t read_limit, tilo_emit emit, void *context, struct tilo_stats *stats);
#endif
