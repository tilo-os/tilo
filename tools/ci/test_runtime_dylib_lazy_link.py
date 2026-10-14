#!/usr/bin/env python3
"""tools/ci/runtime-dylib-lazy-link.py against real ELF objects.

Stand-in libglib-2.0 and libgstreamer-1.0 (built here, so no GLib is needed)
are linked into a stand-in runtime through the wrapper, the way rustc links
libruntime_dylib.so. The runtime must name neither as DT_NEEDED, an app that
never calls into them must never load them, and the first call must load them
and behave exactly like a direct call: variadic and stack arguments, floating
point registers, a function pointer, many threads at once, the mirrored
gst_memory_alignment, and one library instance shared with an app that links
GLib itself.
"""

from __future__ import annotations

import importlib.util
import os
import pathlib
import shutil
import subprocess
import sys
import tempfile
import unittest

HERE = pathlib.Path(__file__).resolve().parent
SCRIPT = HERE / "runtime-dylib-lazy-link.py"
SPEC = importlib.util.spec_from_file_location("runtime_dylib_lazy_link", SCRIPT)
LAZY = importlib.util.module_from_spec(SPEC)
sys.modules[SPEC.name] = LAZY
SPEC.loader.exec_module(LAZY)

GLIB = r"""
#include <stdarg.h>
#include <stdio.h>
#include <stdlib.h>
__attribute__((constructor)) static void loaded(void) { fputs("stand-in glib loaded\n", stderr); }
char *g_strdup_printf(const char *format, ...) {
    va_list arguments;
    char *out = NULL;
    va_start(arguments, format);
    if (vasprintf(&out, format, arguments) < 0) out = NULL;
    va_end(arguments);
    return out;
}
void g_free(void *memory) { free(memory); }
double g_mix(int a, int b, int c, int d, int e, int f, int g, int h, double x, double y) {
    return a + b + c + d + e + f + g + h + x * y;
}
int g_counter = 5;
"""

GSTREAMER = r"""
#include <stddef.h>
size_t gst_memory_alignment = 7;
int gst_init_check(int *argc, char ***argv, void **error) {
    (void)argc; (void)argv; (void)error;
    gst_memory_alignment = 63;
    return 1;
}
void gst_init(int *argc, char ***argv) { gst_init_check(argc, argv, NULL); }
const char *gst_version_string(void) { return "stand-in 1.0"; }
"""

RUNTIME = r"""
#include <pthread.h>
#include <stddef.h>
#include <stdio.h>
#include <stdlib.h>
extern char *g_strdup_printf(const char *, ...);
extern void g_free(void *);
extern double g_mix(int, int, int, int, int, int, int, int, double, double);
extern int gst_init_check(int *, char ***, void **);
extern const char *gst_version_string(void);
extern size_t gst_memory_alignment;

int runtime_plain(void) { return 42; }

int runtime_media(void) {
    printf("alignment before %zu\n", gst_memory_alignment);
    char *text = g_strdup_printf("%s %d %.2f %.1f", "printf", 7, 1.25, 2.5);
    printf("%s\n", text);
    void (*release)(void *) = g_free;
    release(text);
    printf("mix %.1f\n", g_mix(1, 2, 3, 4, 5, 6, 7, 8, 1.5, 2.0));
    printf("init %d\n", gst_init_check(NULL, NULL, NULL));
    printf("alignment after %zu, %s\n", gst_memory_alignment, gst_version_string());
    return 0;
}

static void *worker(void *seed) {
    char *text = g_strdup_printf("%d", (int)(size_t)seed);
    int good = text && (size_t)atoi(text) == (size_t)seed;
    g_free(text);
    return good ? seed : NULL;
}

int runtime_threads(void) {
    pthread_t threads[16];
    for (size_t i = 0; i < 16; i++) pthread_create(&threads[i], NULL, worker, (void *)(i + 1));
    int good = 0;
    for (size_t i = 0; i < 16; i++) {
        void *result;
        pthread_join(threads[i], &result);
        good += result == (void *)(i + 1);
    }
    printf("threads %d\n", good);
    return 0;
}
"""

APP = r"""
#include <stdio.h>
#include <string.h>
extern int runtime_plain(void);
extern int runtime_media(void);
extern int runtime_threads(void);
#ifdef DIRECT
extern void g_free(void *);
#endif
int main(int argc, char **argv) {
#ifdef DIRECT
    g_free(NULL);
#endif
    if (argc > 1 && !strcmp(argv[1], "media")) return runtime_media();
    if (argc > 1 && !strcmp(argv[1], "threads")) return runtime_threads();
    printf("plain %d\n", runtime_plain());
    return 0;
}
"""


class Arguments(unittest.TestCase):
    def test_a_response_file_round_trips(self):
        arguments = ["-o", "out dir/lib x.so", "a\\b", "quote'd", 'double"d', "", "-lglib-2.0"]
        self.assertEqual(LAZY.parse_response(LAZY.format_response(arguments)), arguments)
        self.assertEqual(LAZY.parse_response('-o "a b" \'c d\' e\\ f\n'), ["-o", "a b", "c d", "e f"])

    def test_only_glib_and_gstreamer_leave_the_link_line(self):
        kept, lazy = LAZY.split_libraries(["x.o", "-lgstreamer-1.0", "-lgstapp-1.0", "-lglib-2.0", "-lc",
                                           "-l", "gobject-2.0", "-lgio-2.0", "-lglib-2.0", "-lgstreamerx"])
        self.assertEqual(kept, ["x.o", "-lc", "-lgstreamerx"])
        self.assertEqual(lazy, ["gstreamer-1.0", "gstapp-1.0", "glib-2.0", "gobject-2.0", "gio-2.0"])

    def test_a_variable_other_than_the_mirrored_one_cannot_be_served(self):
        libraries = [LAZY.Library("gstreamer-1.0", pathlib.Path("/g"), "libgstreamer-1.0.so.0",
                                  {"gst_init", "gst_object_ref"}, {"gst_memory_alignment"}),
                     LAZY.Library("glib-2.0", pathlib.Path("/l"), "libglib-2.0.so.0", {"g_free"}, {"g_counter"})]
        stubs, problems = LAZY.plan(libraries, {"gst_init", "gst_object_ref", "gst_memory_alignment", "g_free",
                                                "memcpy"})
        self.assertEqual(stubs, [("g_free", 1), ("gst_object_ref", 0)])
        self.assertEqual(problems, [])
        _, problems = LAZY.plan(libraries, {"g_counter"})
        self.assertEqual(len(problems), 1)
        self.assertIn("variable g_counter", problems[0])

    def test_only_items_of_macro_only_crates_leave_the_export_list(self):
        # Shared generics name the crate that instantiated them last; when
        # that is proc_macro2 the item is still alloc's, and apps link it.
        alloc = "_RNvMs3_NtCsi2GJixwxVUa_5alloc7raw_vecINtB5_6RawVecjE8grow_one"
        self.assertFalse(LAZY.compile_time_only(f"    {alloc}Cs9H9DpKkakbh_11proc_macro2;\n"))
        self.assertFalse(LAZY.compile_time_only(f"    {alloc}Cs1a2b3c_5quote;\n"))
        self.assertFalse(LAZY.compile_time_only(f"    {alloc}Csx_7tilo_ui;\n"))
        self.assertTrue(LAZY.compile_time_only(
            "    _RNvMNtCs9H9DpKkakbh_11proc_macro23impNtB2_11TokenStream3new;\n"))
        self.assertTrue(LAZY.compile_time_only("    _RNvNtCsab_3syn5parse5parse;\n"))
        self.assertTrue(LAZY.compile_time_only(
            "    _RNvNtNtCsq_14zvariant_utils6macros3ty7matches;\n"))


class RealLink(unittest.TestCase):
    """Builds and runs ELF objects; this is the production link path, so it needs Linux."""

    def setUp(self):
        if not sys.platform.startswith("linux"):
            self.fail("links and runs ELF objects; run on Linux (CI runs it in the ci-fast image)")
        for tool in ("cc", "nm", "readelf"):
            self.assertIsNotNone(shutil.which(tool), f"{tool} is required")
        self.temporary = tempfile.TemporaryDirectory(prefix="lazy-link-")
        self.addCleanup(self.temporary.cleanup)
        self.directory = pathlib.Path(self.temporary.name)
        for name, source in (("glib.c", GLIB), ("gstreamer.c", GSTREAMER), ("runtime.c", RUNTIME),
                             ("app.c", APP), ("bad.c", "extern int g_counter; int bad(void) { return g_counter; }\n")):
            (self.directory / name).write_text(source)
        self.cc("-shared", "-fPIC", "glib.c", "-Wl,-soname,libglib-2.0.so.0", "-o", "libglib-2.0.so.0")
        self.cc("-shared", "-fPIC", "gstreamer.c", "-Wl,-soname,libgstreamer-1.0.so.0", "-o",
                "libgstreamer-1.0.so.0")
        for name in ("libglib-2.0", "libgstreamer-1.0"):
            (self.directory / f"{name}.so").symlink_to(f"{name}.so.0")
        self.cc("-c", "-fPIC", "runtime.c", "-o", "runtime.o")
        self.cc("-c", "-fPIC", "bad.c", "-o", "bad.o")

    def cc(self, *arguments: str) -> None:
        subprocess.run(["cc", *arguments], cwd=self.directory, check=True, capture_output=True)

    def lazy_link(self, *arguments: str, response: bool = False) -> subprocess.CompletedProcess:
        line = ["-shared", *arguments, "-L.", "-lgstreamer-1.0", "-lglib-2.0", "-Wl,-z,now", "-pthread",
                "-Wl,-soname,libruntime_dylib.so", "-o", "libruntime_dylib.so"]
        if response:
            (self.directory / "arguments").write_text(LAZY.format_response(line))
            line = ["@arguments"]
        return subprocess.run([sys.executable, str(SCRIPT), *line], cwd=self.directory, capture_output=True,
                              text=True, env=dict(os.environ, TILO_LAZY_LINK_CC="cc"))

    def run_app(self, binary: str, *arguments: str) -> subprocess.CompletedProcess:
        return subprocess.run([str(self.directory / binary), *arguments], capture_output=True, text=True,
                              env=dict(os.environ, LD_LIBRARY_PATH=str(self.directory)), timeout=60)

    def build_apps(self) -> None:
        self.cc("app.c", "-L.", "-lruntime_dylib", "-Wl,-z,now", "-o", "app")
        self.cc("-DDIRECT", "app.c", "-L.", "-lruntime_dylib", "-lglib-2.0", "-Wl,-z,now", "-o", "direct")

    def test_the_runtime_loads_glib_and_gstreamer_only_on_first_use(self):
        for response in (False, True):
            with self.subTest(response_file=response):
                linked = self.lazy_link("runtime.o", response=response)
                self.assertEqual(linked.returncode, 0, linked.stderr)
                runtime = self.directory / "libruntime_dylib.so"
                needed = subprocess.run(["readelf", "-dW", str(runtime)], capture_output=True, text=True).stdout
                self.assertNotIn("libglib", needed)
                self.assertNotIn("libgstreamer", needed)
                self.assertRegex(needed, r"BIND_NOW|Flags:.*\bNOW\b")
                lazy = subprocess.run(["readelf", "-W", "-p", LAZY.SECTION, str(runtime)],
                                      capture_output=True, text=True).stdout
                self.assertLess(lazy.index("libgstreamer-1.0.so.0"), lazy.index("libglib-2.0.so.0"))
                self.build_apps()

                plain = self.run_app("app")
                self.assertEqual((plain.returncode, plain.stdout), (0, "plain 42\n"), plain.stderr)
                self.assertNotIn("stand-in glib loaded", plain.stderr)

                media = self.run_app("app", "media")
                self.assertEqual(media.returncode, 0, media.stderr)
                self.assertEqual(media.stdout, "alignment before 7\nprintf 7 1.25 2.5\nmix 39.0\ninit 1\n"
                                               "alignment after 63, stand-in 1.0\n")
                self.assertEqual(media.stderr.count("stand-in glib loaded"), 1, media.stderr)

                threads = self.run_app("app", "threads")
                self.assertEqual((threads.returncode, threads.stdout), (0, "threads 16\n"), threads.stderr)

                direct = self.run_app("direct", "media")
                self.assertEqual(direct.returncode, 0, direct.stderr)
                self.assertEqual(direct.stderr.count("stand-in glib loaded"), 1, direct.stderr)

    def test_a_library_without_a_function_aborts_at_first_use_and_names_it(self):
        # A missing library would fall back to the host's own GLib, so take
        # the function away instead: the stand-in now exports nothing.
        self.assertEqual(self.lazy_link("runtime.o").returncode, 0)
        self.build_apps()
        (self.directory / "empty.c").write_text("int g_unrelated(void) { return 0; }\n")
        self.cc("-shared", "-fPIC", "empty.c", "-Wl,-soname,libglib-2.0.so.0", "-o", "libglib-2.0.so.0")
        self.assertEqual(self.run_app("app").returncode, 0)
        media = self.run_app("app", "media")
        self.assertNotEqual(media.returncode, 0)
        self.assertRegex(media.stderr, r"tilo runtime: cannot resolve g_\w+ for media")

    def test_reading_a_variable_of_a_lazy_library_fails_the_link(self):
        linked = self.lazy_link("runtime.o", "bad.o")
        self.assertNotEqual(linked.returncode, 0)
        self.assertIn("variable g_counter", linked.stderr)
        self.assertFalse((self.directory / "libruntime_dylib.so").exists())

    def test_a_link_without_glib_or_gstreamer_is_unchanged(self):
        (self.directory / "leaf.c").write_text("int leaf(void) { return 7; }\n")
        result = subprocess.run([sys.executable, str(SCRIPT), "-shared", "-fPIC", "leaf.c", "-o", "libleaf.so"],
                                cwd=self.directory, capture_output=True, text=True,
                                env=dict(os.environ, TILO_LAZY_LINK_CC="cc"))
        self.assertEqual(result.returncode, 0, result.stderr)
        sections = subprocess.run(["readelf", "-SW", str(self.directory / "libleaf.so")],
                                  capture_output=True, text=True).stdout
        self.assertNotIn(LAZY.SECTION, sections)

    def test_code_nothing_exported_reaches_and_macro_only_crates_are_left_out(self):
        # rustc links a dylib without collecting sections and exports the
        # public items of every crate in it, syn's included.
        (self.directory / "leaf.c").write_text(
            "int kept(void) { return 7; }\n"
            "int _RNvCsabc_3syn10parse_file(void) { return 8; }\n"
            "int unreached(void) { return 9; }\n")
        (self.directory / "list").write_text(
            "{\n  global:\n    kept;\n    _RNvCsabc_3syn10parse_file;\n  local:\n    *;\n};\n")
        result = subprocess.run([sys.executable, str(SCRIPT), "-shared", "-fPIC", "-ffunction-sections",
                                 "leaf.c", "-Wl,--version-script=list", "-o", "libleaf.so"],
                                cwd=self.directory, capture_output=True, text=True,
                                env=dict(os.environ, TILO_LAZY_LINK_CC="cc"))
        self.assertEqual(result.returncode, 0, result.stderr)
        symbols = subprocess.run(["nm", str(self.directory / "libleaf.so")],
                                 capture_output=True, text=True).stdout
        self.assertIn(" kept", symbols)
        self.assertNotIn("3syn", symbols)
        self.assertNotIn("unreached", symbols)

    def test_a_shared_generic_instantiated_in_a_macro_only_crate_stays_exported(self):
        # With shared generics, an app compiled against the runtime links
        # RawVec<usize>::grow_one from whichever upstream crate rustc picks
        # (the lowest crate id), which can be proc_macro2. Its instance is
        # alloc's item, named after proc_macro2 only as the instantiating
        # crate, so leaving it out made those apps fail to link.
        shared = "_RNvMs3_NtCsi2GJixwxVUa_5alloc7raw_vecINtB5_6RawVecjE8grow_oneCs9H9DpKkakbh_11proc_macro2"
        own = "_RNvMNtCs9H9DpKkakbh_11proc_macro23impNtB2_11TokenStream3new"
        (self.directory / "leaf.c").write_text(
            f"int {shared}(void) {{ return 1; }}\n"
            f"int {own}(void) {{ return 2; }}\n")
        (self.directory / "list").write_text(
            f"{{\n  global:\n    {shared};\n    {own};\n  local:\n    *;\n}};\n")
        result = subprocess.run([sys.executable, str(SCRIPT), "-shared", "-fPIC", "-ffunction-sections",
                                 "leaf.c", "-Wl,--version-script=list", "-o", "libleaf.so"],
                                cwd=self.directory, capture_output=True, text=True,
                                env=dict(os.environ, TILO_LAZY_LINK_CC="cc"))
        self.assertEqual(result.returncode, 0, result.stderr)
        exported = subprocess.run(["nm", "-D", "--defined-only", str(self.directory / "libleaf.so")],
                                  capture_output=True, text=True).stdout
        self.assertIn(shared, exported)
        self.assertNotIn(own, exported)


if __name__ == "__main__":
    unittest.main()
