#ifndef QOS_NTFS_READER_H
#define QOS_NTFS_READER_H
#include <stddef.h>
#include <stdint.h>
typedef int (*qos_emit)(void *, const char *, size_t);
struct qos_stats {
  uint64_t bytes, calls, writes;
};
int qos_ntfs_probe(int fd, uint64_t capacity, uint64_t root,
                   uint64_t read_limit, qos_emit emit, void *context,
                   struct qos_stats *stats);

struct qos_ntfs_read_request {
  uint64_t serial, root_record, root_creation, object_record, size, offset;
  uint32_t length;
  uint16_t root_sequence, object_sequence;
  uint64_t reserved;
};
_Static_assert(sizeof(struct qos_ntfs_read_request) == 64, "read request ABI");
_Static_assert(offsetof(struct qos_ntfs_read_request, length) == 48, "length ABI");
_Static_assert(offsetof(struct qos_ntfs_read_request, root_sequence) == 52, "sequence ABI");
_Static_assert(offsetof(struct qos_ntfs_read_request, reserved) == 56, "reserved ABI");
int qos_ntfs_manifest(int fd, uint64_t capacity, uint64_t root, uint64_t read_limit,
                      qos_emit emit, void *context, struct qos_stats *stats);
int qos_ntfs_read_chunk(int fd, uint64_t capacity, const struct qos_ntfs_read_request *request,
                       uint64_t read_limit, qos_emit emit, void *context, struct qos_stats *stats);
#endif
