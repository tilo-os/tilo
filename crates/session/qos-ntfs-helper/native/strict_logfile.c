/* Use the exact pinned parser's internal structural validators, without its
 * recovery policy. Renaming its public entry points prevents ABI collisions;
 * none of these recovery/write entry points is called by the helper.
 * The checksum-verified source is part of the distributed corresponding source.
 */
#define ntfs_check_logfile qos_unused_recovery_check_logfile
#define ntfs_is_logfile_clean qos_unused_recovery_is_clean
#define ntfs_empty_logfile qos_unused_recovery_empty_logfile
#include "libntfs-3g/logfile.c"
#undef ntfs_check_logfile
#undef ntfs_is_logfile_clean
#undef ntfs_empty_logfile

/* Strict Stage 1 admission supports restart pages through 64 KiB. Larger
 * structures are refused, never partially inspected. Reads still pass through
 * the common adapter's aggregate byte/call/deadline limits. */
#define QOS_RESTART_MAX 65536U
static int qos_restart_page(ntfs_attr *attr, u8 *bytes, s64 position,
                            u32 page_size) {
  RESTART_PAGE_HEADER *header = (RESTART_PAGE_HEADER *)bytes;
  if (ntfs_attr_pread(attr, position, page_size, bytes) != page_size ||
      !ntfs_is_rstr_record(header->magic) ||
      header->major_ver != const_cpu_to_sle16(1) ||
      header->minor_ver != const_cpu_to_sle16(1) ||
      le32_to_cpu(header->system_page_size) != page_size ||
      le32_to_cpu(header->log_page_size) > QOS_RESTART_MAX ||
      !ntfs_check_restart_page_header(header, position) ||
      !ntfs_check_restart_area(header))
    return 0;
  RESTART_AREA *area = (RESTART_AREA *)(bytes +
      le16_to_cpu(header->restart_area_offset));
  u32 client_offset = le16_to_cpu(area->client_array_offset);
  u32 client_bytes = (u32)le16_to_cpu(area->log_clients) * sizeof(LOG_CLIENT_RECORD);
  /* Avoid the upstream validator's 16-bit computed-length truncation and
   * reject arrays that overlap their own restart area. */
  if (client_offset < sizeof(RESTART_AREA) ||
      client_offset + client_bytes > le16_to_cpu(area->restart_area_length) ||
      sle64_to_cpu(area->file_size) <= 0 ||
      sle64_to_cpu(area->file_size) > attr->data_size)
    return 0;
  /* The recovery parser tolerates some torn sectors. Stage 1 does not. */
  if (ntfs_mst_post_read_fixup((NTFS_RECORD *)bytes, page_size) ||
      !ntfs_check_log_client_array(header))
    return 0;
  return area->client_in_use_list == LOGFILE_NO_CLIENT ||
         (area->flags & RESTART_VOLUME_IS_CLEAN);
}

int qos_check_logfile_strict(ntfs_attr *attr) {
  u8 bytes[QOS_RESTART_MAX];
  if (attr->data_size < (MinLogRecordPages + 2) * DefaultLogPageSize ||
      ntfs_attr_pread(attr, 0, NTFS_BLOCK_SIZE, bytes) != NTFS_BLOCK_SIZE)
    return 0;
  RESTART_PAGE_HEADER *header = (RESTART_PAGE_HEADER *)bytes;
  if (ntfs_is_empty_record(header->magic)) {
    /* Ignore cached NVolLogFileEmpty: require every byte to be the native
     * erased value, not just magic words at exponentially spaced offsets. */
    for (s64 position = 0; position < attr->data_size;) {
      s64 count = attr->data_size - position;
      if (count > (s64)sizeof bytes)
        count = sizeof bytes;
      if (ntfs_attr_pread(attr, position, count, bytes) != count)
        return 0;
      for (s64 i = 0; i < count; ++i)
        if (bytes[i] != 0xff)
          return 0;
      position += count;
    }
    return 1;
  }
  u32 page_size = le32_to_cpu(header->system_page_size);
  if (page_size < NTFS_BLOCK_SIZE || page_size > QOS_RESTART_MAX ||
      (page_size & (page_size - 1)) ||
      (s64)page_size * 2 > attr->data_size)
    return 0;
  if (!qos_restart_page(attr, bytes, 0, page_size))
    return 0;
  u32 log_page_size = le32_to_cpu(header->log_page_size);
  if (!qos_restart_page(attr, bytes, page_size, page_size) ||
      le32_to_cpu(header->log_page_size) != log_page_size)
    return 0;
  return 1;
}
