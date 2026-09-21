/*
 * Controlled EFI application used by the alongside-install VM acceptance.
 *
 * This is deliberately not a Windows or Linux loader.  The cycle installs two
 * byte-distinct instances at the canonical paths discovered by the production
 * boot-menu helper, selects those entries through the real systemd-boot UI,
 * and observes the marker below on the firmware framebuffer.  That proves the
 * preserved file can actually be loaded and executed without claiming that a
 * licensed Windows image or a complete foreign Linux distribution was booted.
 */
typedef unsigned long long efi_status_t;
typedef unsigned short efi_char16_t;

struct efi_simple_text_output;
typedef efi_status_t (*efi_output_string_t)(
    struct efi_simple_text_output *, efi_char16_t *);

struct efi_simple_text_output {
    void *reset;
    efi_output_string_t output_string;
};

struct efi_table_header {
    unsigned long long signature;
    unsigned int revision;
    unsigned int header_size;
    unsigned int crc32;
    unsigned int reserved;
};

struct efi_system_table {
    struct efi_table_header header;
    efi_char16_t *firmware_vendor;
    unsigned int firmware_revision;
    void *console_in_handle;
    void *console_in;
    void *console_out_handle;
    struct efi_simple_text_output *console_out;
};

#ifndef TILO_FOREIGN_BOOT_MARKER
#error TILO_FOREIGN_BOOT_MARKER must be an EFI UTF-16 string literal
#endif

efi_status_t efi_main(void *image, struct efi_system_table *system_table)
{
    static efi_char16_t marker[] = TILO_FOREIGN_BOOT_MARKER;
    volatile unsigned long long keep_marker_visible = 1;

    (void)image;
    if (system_table == 0 || system_table->console_out == 0 ||
        system_table->console_out->output_string == 0)
        return 1;
    if (system_table->console_out->output_string(
            system_table->console_out, marker) != 0)
        return 1;

    /* Keep the successful marker on-screen until the host records it. */
    while (keep_marker_visible != 0) {
    }
    return 0;
}
