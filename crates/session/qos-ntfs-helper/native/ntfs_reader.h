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
#endif
