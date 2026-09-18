/* Minimal architecture-neutral EFI application for the Secure Boot VM probe. */
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

efi_status_t efi_main(void *image, struct efi_system_table *system_table)
{
    static efi_char16_t marker[] =
        L"TILO_SECURE_BOOT_SIGNED_EFI_PASS\r\n";
    (void)image;
    if (system_table == 0 || system_table->console_out == 0 ||
        system_table->console_out->output_string == 0)
        return 1;
    return system_table->console_out->output_string(
        system_table->console_out, marker);
}
