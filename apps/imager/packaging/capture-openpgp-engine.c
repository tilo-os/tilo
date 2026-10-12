/* Trusted native-builder probe; never accepts a runtime-selected engine path. */
#include <gpgme.h>
#include <stdio.h>
#include <string.h>
static int emit(const char *role, const char *value) {
    if (!value || !*value || strlen(value)>4096) return 1;
    for (const unsigned char *p=(const unsigned char *)value; *p; ++p)
        if (*p<32 || *p==127) return 1;
    return printf("%s\t%s\n",role,value)<0;
}
int main(void) {
    if (!gpgme_check_version(NULL)) return 1;
    gpgme_engine_info_t engines=NULL;
    if (gpgme_get_engine_info(&engines)) return 1;
    gpgme_engine_info_t openpgp=NULL;
    unsigned count=0;
    for (gpgme_engine_info_t e=engines;e;e=e->next) {
        if (++count>32) return 1;
        if (e->protocol==GPGME_PROTOCOL_OpenPGP) {
            if (openpgp) return 1;
            openpgp=e;
        }
    }
    const char *default_gpg=gpgme_get_dirinfo("gpg-name");
    /* A changed per-context/default path needs its own explicit signed alias;
       this fresh native probe has installed no engine override. */
    if (!openpgp || !openpgp->file_name || !default_gpg
        || strcmp(openpgp->file_name,default_gpg)
        || gpgme_engine_check_version(GPGME_PROTOCOL_OpenPGP)
        || emit("openpgp",openpgp->file_name) || emit("version",openpgp->version)
        || emit("gpgconf",gpgme_get_dirinfo("gpgconf-name"))
        || emit("agent",gpgme_get_dirinfo("agent-name"))
        || emit("libexecdir",gpgme_get_dirinfo("libexecdir"))) return 1;
    const char *keyboxd=gpgme_get_dirinfo("keyboxd-name");
    if (keyboxd && emit("keyboxd",keyboxd)) return 1;
    return 0;
}
