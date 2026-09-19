/* Uninstalled mutation tool. Only fresh regular fixtures supplied by builder
 * script. */
#include "attrib.h"
#include "bootsect.h"
#include "config.h"
#include "dir.h"
#include "inode.h"
#include "logfile.h"
#include "mst.h"
#include "unistr.h"
#include "security.h"
#include "volume.h"
#include <fcntl.h>
#include <stddef.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/stat.h>
#include <unistd.h>
int main(int argc, char **argv) {
  if (argc != 3)
    return 2;
  struct stat st;
  if (lstat(argv[1], &st) || !S_ISREG(st.st_mode) || st.st_nlink != 1)
    return 3;
  ntfs_volume *v = ntfs_mount(argv[1], 0);
  if (!v) {
    perror("fixture mount");
    return 4;
  }
  int rc = 0;
  if (!strcmp(argv[2], "dirty")) {
    rc = ntfs_volume_write_flags(v, cpu_to_le16(1));
  } else if (!strcmp(argv[2], "badlog")) {
    ntfs_inode *ni = ntfs_inode_open(v, FILE_LogFile);
    ntfs_attr *a = ni ? ntfs_attr_open(ni, AT_DATA, AT_UNNAMED, 0) : NULL;
    char b[8192];
    memset(b, 0x37, sizeof b);
    rc = !a || ntfs_attr_pwrite(a, 0, sizeof b, b) != sizeof b;
    if (a)
      ntfs_attr_close(a);
    if (ni)
      ntfs_inode_close(ni);
  } else if (!strncmp(argv[2], "restart-", 8)) {
    ntfs_inode *ni = ntfs_inode_open(v, FILE_LogFile);
    ntfs_attr *a = ni ? ntfs_attr_open(ni, AT_DATA, AT_UNNAMED, 0) : NULL;
    if (!a)
      return 15;
    for (unsigned page = 0; page < 2; ++page) {
      unsigned char bytes[4096] = {0};
      RESTART_PAGE_HEADER *header = (RESTART_PAGE_HEADER *)bytes;
      header->magic = magic_RSTR;
      header->usa_ofs = cpu_to_le16(30);
      header->usa_count = cpu_to_le16(9);
      header->system_page_size = cpu_to_le32(sizeof bytes);
      header->log_page_size = cpu_to_le32(sizeof bytes);
      header->restart_area_offset = cpu_to_le16(48);
      header->major_ver = cpu_to_sle16(1);
      header->minor_ver = cpu_to_sle16(1);
      header->usn = cpu_to_le16(7 + page);
      RESTART_AREA *area = (RESTART_AREA *)(bytes + 48);
      area->log_clients = cpu_to_le16(1);
      area->client_free_list = LOGFILE_NO_CLIENT;
      area->client_in_use_list = cpu_to_le16(0);
      area->flags = RESTART_VOLUME_IS_CLEAN;
      unsigned bits = 0;
      for (uint64_t size = a->data_size; size; size >>= 1)
        ++bits;
      area->seq_number_bits = cpu_to_le32(67 - bits);
      area->restart_area_length = cpu_to_le16(48 + sizeof(LOG_CLIENT_RECORD));
      area->client_array_offset = cpu_to_le16(48);
      area->file_size = cpu_to_sle64(a->data_size);
      area->log_record_header_length = cpu_to_le16(48);
      area->log_page_data_offset = cpu_to_le16(64);
      LOG_CLIENT_RECORD *client = (LOG_CLIENT_RECORD *)(bytes + 96);
      client->prev_client = LOGFILE_NO_CLIENT;
      client->next_client = LOGFILE_NO_CLIENT;
      client->client_name_length = cpu_to_le32(8);
      client->client_name[0] = cpu_to_le16('N');
      client->client_name[1] = cpu_to_le16('T');
      client->client_name[2] = cpu_to_le16('F');
      client->client_name[3] = cpu_to_le16('S');
      if (!strcmp(argv[2], "restart-v2")) {
        header->major_ver = cpu_to_sle16(2);
        header->minor_ver = 0;
      }
      if ((!strcmp(argv[2], "restart-bad-first") && page == 0) ||
          (!strcmp(argv[2], "restart-bad-second") && page == 1))
        area->seq_number_bits = 0;
      if (ntfs_mst_pre_write_fixup((NTFS_RECORD *)bytes, sizeof bytes) ||
          ntfs_attr_pwrite(a, page * sizeof bytes, sizeof bytes, bytes) != sizeof bytes)
        rc = 1;
    }
    ntfs_attr_close(a);
    if (ntfs_inode_close(ni))
      rc = 1;
  } else if (!strcmp(argv[2], "generation")) {
    ntfs_inode *ni = ntfs_inode_open(v, FILE_root);
    if (!ni)
      return 5;
    ni->mrec->sequence_number = cpu_to_le16(42);
    ntfs_inode_mark_dirty(ni);
    rc = ntfs_inode_close(ni);
  } else if (!strcmp(argv[2], "times") || !strcmp(argv[2], "times-v1")) {
    ntfs_inode *ni = ntfs_pathname_to_inode(v, NULL, "/hello.txt");
    uint64_t expected_size = !strcmp(argv[2], "times")
                                 ? sizeof(STANDARD_INFORMATION)
                                 : offsetof(STANDARD_INFORMATION, v1_end);
    ntfs_attr *a = ni ? ntfs_attr_open(ni, AT_STANDARD_INFORMATION,
                                       AT_UNNAMED, 0) : NULL;
    if (!a || ntfs_attr_truncate(a, expected_size))
      return 19;
    ntfs_attr_close(a);
    ntfs_attr_search_ctx *ctx = ni ? ntfs_attr_get_search_ctx(ni, NULL) : NULL;
    if (!ctx || ntfs_attr_lookup(AT_STANDARD_INFORMATION, AT_UNNAMED, 0,
                                 CASE_SENSITIVE, 0, NULL, 0, ctx))
      return 20;
    if (le32_to_cpu(ctx->attr->value_length) != expected_size)
      return 21;
    STANDARD_INFORMATION *info = (STANDARD_INFORMATION *)
        ((char *)ctx->attr + le16_to_cpu(ctx->attr->value_offset));
    info->last_data_change_time = cpu_to_sle64(116444735999999999ULL);
    info->last_access_time = cpu_to_sle64(116444736012345678ULL);
    set_nino_flag(ni, TimesSet);
    ntfs_inode_mark_dirty(ctx->ntfs_ino);
    ntfs_attr_put_search_ctx(ctx);
    rc = ntfs_inode_close(ni);
  } else if (!strcmp(argv[2], "times-missing")) {
    ntfs_inode *ni = ntfs_pathname_to_inode(v, NULL, "/hello.txt");
    if (!ni || ntfs_attr_remove(ni, AT_STANDARD_INFORMATION, AT_UNNAMED, 0))
      return 22;
    /* A base record without STANDARD_INFORMATION is structurally invalid.
     * ntfs_inode_close must fail while still flushing the deliberately
     * malformed test record; the production parser is expected to refuse it. */
    if (!ntfs_inode_close(ni))
      return 23;
    rc = 0;
  } else if (!strcmp(argv[2], "times-duplicate")) {
    ntfs_inode *ni = ntfs_pathname_to_inode(v, NULL, "/hello.txt");
    ntfs_attr_search_ctx *ctx = ni ? ntfs_attr_get_search_ctx(ni, NULL) : NULL;
    if (!ctx || ntfs_attr_lookup(AT_STANDARD_INFORMATION, AT_UNNAMED, 0,
                                 CASE_SENSITIVE, 0, NULL, 0, ctx))
      return 24;
    uint32_t length = le32_to_cpu(ctx->attr->length);
    uint32_t used = le32_to_cpu(ni->mrec->bytes_in_use);
    uint32_t allocated = le32_to_cpu(ni->mrec->bytes_allocated);
    char *record = (char *)ni->mrec;
    char *attribute = (char *)ctx->attr;
    uint32_t offset = (uint32_t)(attribute - record);
    if (used > allocated || length < sizeof(ATTR_RECORD) || offset > used ||
        length > used - offset || length > allocated - used)
      return 25;
    unsigned char *copy = malloc(length);
    if (!copy)
      return 25;
    memcpy(copy, attribute, length);
    memmove(attribute + 2 * length, attribute + length,
            used - offset - length);
    memcpy(attribute + length, copy, length);
    free(copy);
    ATTR_RECORD *duplicate = (ATTR_RECORD *)(attribute + length);
    duplicate->instance = ni->mrec->next_attr_instance;
    ni->mrec->next_attr_instance = cpu_to_le16(
        (uint16_t)(le16_to_cpu(ni->mrec->next_attr_instance) + 1));
    ni->mrec->bytes_in_use = cpu_to_le32(used + length);
    ntfs_inode_mark_dirty(ctx->ntfs_ino);
    ntfs_attr_put_search_ctx(ctx);
    rc = ntfs_inode_close(ni);
  } else if (!strcmp(argv[2], "times-invalid-flags")) {
    ntfs_inode *ni = ntfs_pathname_to_inode(v, NULL, "/hello.txt");
    ntfs_attr_search_ctx *ctx = ni ? ntfs_attr_get_search_ctx(ni, NULL) : NULL;
    if (!ctx || ntfs_attr_lookup(AT_STANDARD_INFORMATION, AT_UNNAMED, 0,
                                 CASE_SENSITIVE, 0, NULL, 0, ctx))
      return 26;
    ctx->attr->flags = cpu_to_le16(ATTR_IS_COMPRESSED);
    ntfs_inode_mark_dirty(ctx->ntfs_ino);
    ntfs_attr_put_search_ctx(ctx);
    rc = ntfs_inode_close(ni);
  } else if (!strcmp(argv[2], "metadata")) {
    ntfs_inode *ni = ntfs_inode_open(v, FILE_root);
    ntfschar ads[] = {cpu_to_le16('d'), cpu_to_le16('i'), cpu_to_le16('r')};
    char payload[] = "directory stream";
    rc = !ni ||
         ntfs_attr_add(ni, AT_DATA, ads, 3, (u8 *)payload, sizeof payload - 1);
    ntfschar name[] = {cpu_to_le16('l'), cpu_to_le16('i'), cpu_to_le16('n'),
                       cpu_to_le16('k')};
    ntfschar target[] = {cpu_to_le16('h'), cpu_to_le16('e'), cpu_to_le16('l'),
                         cpu_to_le16('l'), cpu_to_le16('o'), cpu_to_le16('.'),
                         cpu_to_le16('t'), cpu_to_le16('x'), cpu_to_le16('t')};
    ntfs_inode *link =
        ni ? ntfs_create(ni, const_cpu_to_le32(0), name, 4, S_IFREG) : NULL;
    struct __attribute__((packed)) {
      le32 tag;
      le16 length, reserved, sub_offset, sub_length, print_offset, print_length;
      le32 flags;
      ntfschar path[9];
    } reparse = {
        cpu_to_le32(0xa000000c), cpu_to_le16(30), 0,  0, cpu_to_le16(18), 0,
        cpu_to_le16(18),         cpu_to_le32(1),  {0}};
    memcpy(reparse.path, target, sizeof target);
    if (link) {
      if (ntfs_attr_add(link, AT_REPARSE_POINT, AT_UNNAMED, 0, (u8 *)&reparse,
                        sizeof reparse))
        rc = 1;
      link->flags |= FILE_ATTR_REPARSE_POINT;
      ntfs_inode_mark_dirty(link);
    }
    if (!link)
      rc = 1;
    else if (ntfs_inode_close(link))
      rc = 1;
    if (ni && ntfs_inode_close(ni))
      rc = 1;
  } else if (!strcmp(argv[2], "upcase")) {
    ntfs_inode *ni = ntfs_inode_open(v, FILE_UpCase);
    ntfs_attr *a = ni ? ntfs_attr_open(ni, AT_DATA, AT_UNNAMED, 0) : NULL;
    ntfs_inode *root = ntfs_inode_open(v, FILE_root);
    ntfschar fname[] = {cpu_to_le16(0xe4), cpu_to_le16('.'), cpu_to_le16('t'),
                        cpu_to_le16('x'), cpu_to_le16('t')};
    ntfs_inode *file =
        root ? ntfs_create(root, const_cpu_to_le32(0), fname, 5, S_IFREG)
             : NULL;
    if (!file)
      return 9;
    ntfs_inode_close(file);
    ntfs_inode_close(root);
    ntfschar replacement = cpu_to_le16(0xd6);
    rc = !a || ntfs_attr_pwrite(a, 2 * 0xe4, 2, &replacement) != 2;
    if (a)
      ntfs_attr_close(a);
    if (ni)
      ntfs_inode_close(ni);
  } else if (!strcmp(argv[2], "dos-alias")) {
    ntfs_inode *ni = ntfs_pathname_to_inode(v, NULL, "/hello.txt");
    ntfs_inode *root = ntfs_inode_open(v, FILE_root);
    if (!ni || !root || ntfs_set_ntfs_dos_name(ni, root, "HELLO~1.TXT", 11, 0)) return 17;
    root = ntfs_inode_open(v, FILE_root);
    ntfschar name[] = {cpu_to_le16('L'),cpu_to_le16('o'),cpu_to_le16('n'),cpu_to_le16('g'),cpu_to_le16('D'),cpu_to_le16('i'),cpu_to_le16('r'),cpu_to_le16('e'),cpu_to_le16('c'),cpu_to_le16('t'),cpu_to_le16('o'),cpu_to_le16('r'),cpu_to_le16('y')};
    ni = root ? ntfs_create(root, const_cpu_to_le32(0), name, 13, S_IFDIR) : NULL;
    if (!ni || ntfs_set_ntfs_dos_name(ni, root, "LONGDI~1", 8, 0)) return 18;
  } else if (!strcmp(argv[2], "copy-directories")) {
    ntfs_inode *root = ntfs_inode_open(v, FILE_root);
    ntfschar name[] = {cpu_to_le16('n'),cpu_to_le16('e'),cpu_to_le16('s'),cpu_to_le16('t'),cpu_to_le16('e'),cpu_to_le16('d')};
    ntfs_inode *directory = root ? ntfs_create(root, const_cpu_to_le32(0), name, 6, S_IFDIR) : NULL;
    ntfschar child_name[] = {cpu_to_le16('e'),cpu_to_le16('m'),cpu_to_le16('p'),cpu_to_le16('t'),cpu_to_le16('y')};
    ntfs_inode *child = directory ? ntfs_create(directory, const_cpu_to_le32(0), child_name, 5, S_IFDIR) : NULL;
    rc = !child;
    if (child && ntfs_inode_close(child)) rc = 1;
    if (directory && ntfs_inode_close(directory)) rc = 1;
    if (root && ntfs_inode_close(root)) rc = 1;
  } else if (!strcmp(argv[2], "cancellation")) {
    ntfs_inode *root = ntfs_inode_open(v, FILE_root);
    ntfschar name[] = {cpu_to_le16('c'),cpu_to_le16('a'),cpu_to_le16('n'),cpu_to_le16('c'),cpu_to_le16('e'),cpu_to_le16('l')};
    ntfs_inode *directory = root ? ntfs_create(root, const_cpu_to_le32(0), name, 6, S_IFDIR) : NULL;
    ntfschar fname[] = {cpu_to_le16('l'),cpu_to_le16('a'),cpu_to_le16('s'),cpu_to_le16('t'),cpu_to_le16('.'),cpu_to_le16('b'),cpu_to_le16('i'),cpu_to_le16('n')};
    ntfs_inode *file = directory ? ntfs_create(directory, const_cpu_to_le32(0), fname, 8, S_IFREG) : NULL;
    rc = !file;
    if (file && ntfs_inode_close(file)) rc = 1;
    if (directory && ntfs_inode_close(directory)) rc = 1;
    if (root && ntfs_inode_close(root)) rc = 1;
  } else if (!strcmp(argv[2], "large-metadata")) {
    ntfs_inode *ni = ntfs_pathname_to_inode(v, NULL, "/hello.txt");
    ntfschar name[] = {cpu_to_le16('e'),cpu_to_le16('x'),cpu_to_le16('t'),cpu_to_le16('r'),cpu_to_le16('a')};
    unsigned char b = 42;
    if (!ni || ntfs_attr_add(ni, AT_DATA, name, 5, &b, 1)) return 16;
    ntfs_attr *a = ntfs_attr_open(ni, AT_DATA, name, 5);
    rc = !a || ntfs_attr_truncate(a, 1048577);
    if (a) ntfs_attr_close(a);
    if (ntfs_inode_close(ni)) rc = 1;
  } else if (!strcmp(argv[2], "oversize")) {
    ntfs_inode *ni = ntfs_pathname_to_inode(v, NULL, "/hello.txt");
    ntfs_attr *a = ni ? ntfs_attr_open(ni, AT_DATA, AT_UNNAMED, 0) : NULL;
    rc = !a || ntfs_attr_truncate(a, 1048577);
    if (a)
      ntfs_attr_close(a);
    if (ni)
      ntfs_inode_close(ni);
  } else if (!strcmp(argv[2], "hiber-io")) {
    ntfs_inode *ni = ntfs_pathname_to_inode(v, NULL, "/hiberfil.sys");
    ntfs_attr_search_ctx *ctx = ni ? ntfs_attr_get_search_ctx(ni, NULL) : NULL;
    if (!ctx ||
        ntfs_attr_lookup(AT_DATA, AT_UNNAMED, 0, CASE_SENSITIVE, 0, NULL, 0,
                         ctx) ||
        !ctx->attr->non_resident)
      return 10;
    unsigned char bad[] = {0x41, 1, 0xf0, 0xff, 0xff, 0x7f, 0};
    uint16_t offset = le16_to_cpu(ctx->attr->mapping_pairs_offset);
    if (offset + sizeof bad > le32_to_cpu(ctx->attr->length))
      return 11;
    memcpy((char *)ctx->attr + offset, bad, sizeof bad);
    ntfs_attr_put_search_ctx(ctx);
    ntfs_inode_mark_dirty(ni);
    rc = ntfs_inode_close(ni);
  } else if (!strcmp(argv[2], "attrlist-cycle")) {
    ntfs_inode *ni = ntfs_pathname_to_inode(v, NULL, "/hello.txt");
    unsigned char payload[32] = {0};
    ATTR_LIST_ENTRY *entry = (ATTR_LIST_ENTRY *)payload;
    entry->type = AT_ATTRIBUTE_LIST;
    entry->length = cpu_to_le16(32);
    entry->name_offset = 26;
    entry->mft_reference = cpu_to_le64(
        ni->mft_no | ((uint64_t)le16_to_cpu(ni->mrec->sequence_number) << 48));
    if (ntfs_attr_add(ni, AT_LOGGED_UTILITY_STREAM, AT_UNNAMED, 0, payload,
                      sizeof payload))
      return 12;
    ntfs_attr_search_ctx *ctx = ntfs_attr_get_search_ctx(ni, NULL);
    if (!ctx || ntfs_attr_lookup(AT_LOGGED_UTILITY_STREAM, AT_UNNAMED, 0,
                                 CASE_SENSITIVE, 0, NULL, 0, ctx))
      return 13;
    ctx->attr->type = AT_ATTRIBUTE_LIST;
    ATTR_LIST_ENTRY *stored =
        (ATTR_LIST_ENTRY *)((char *)ctx->attr +
                            le16_to_cpu(ctx->attr->value_offset));
    stored->instance = ctx->attr->instance;
    ntfs_attr_put_search_ctx(ctx);
    ntfs_inode_mark_dirty(ni);
    rc = ntfs_inode_close(ni);
  } else if (!strcmp(argv[2],"acl")) {
    ntfs_inode *ni=ntfs_pathname_to_inode(v,NULL,"/hello.txt");s64 length=0;
    void *sd=ntfs_attr_readall(ni,AT_SECURITY_DESCRIPTOR,AT_UNNAMED,0,&length);
    struct SECURITY_CONTEXT context={.vol=v};
    if(!sd||length<0||ntfs_set_ntfs_acl(&context,ni,sd,(size_t)length,0))return 14;
    free(sd);rc=ntfs_inode_close(ni);
  } else if (!strcmp(argv[2],"hardlink")) {
    ntfs_inode *ni=ntfs_pathname_to_inode(v,NULL,"/hello.txt");ntfs_inode *root=ntfs_inode_open(v,FILE_root);
    ntfschar name[]={cpu_to_le16('s'),cpu_to_le16('e'),cpu_to_le16('c'),cpu_to_le16('o'),cpu_to_le16('n'),cpu_to_le16('d')};
    rc=ntfs_link(ni,root,name,6);if(ntfs_inode_close(ni)||ntfs_inode_close(root))rc=1;
  } else if (strcmp(argv[2], "serial"))
    rc = 1;
  if (ntfs_umount(v, 0))
    rc = 1;
  if (!strcmp(argv[2], "serial")) {
    int fd = open(argv[1], O_RDWR | O_NOFOLLOW);
    NTFS_BOOT_SECTOR bs;
    if (fd < 0 || pread(fd, &bs, sizeof bs, 0) != sizeof bs)
      return 6;
    bs.volume_serial_number = cpu_to_le64(0x1122334455667788ULL);
    if (pwrite(fd, &bs, sizeof bs, 0) != sizeof bs ||
        pwrite(fd, &bs, sizeof bs, st.st_size - sizeof bs) != sizeof bs ||
        fsync(fd))
      rc = 1;
    close(fd);
  }
  return rc != 0;
}
