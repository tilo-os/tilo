#include <spa/buffer/meta.h>

bool libspa_rs_meta_check(const void *p, const struct spa_meta *m) {
    return spa_meta_check(p, m);
}

bool libspa_rs_meta_bitmap_is_valid(const struct spa_meta_bitmap *m) {
    return spa_meta_bitmap_is_valid(m);
}

bool libspa_rs_meta_cursor_is_valid(const struct spa_meta_cursor *m) {
    return spa_meta_cursor_is_valid(m);
}
