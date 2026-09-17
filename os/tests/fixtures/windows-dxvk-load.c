/* A freestanding Windows PE fixture: DLL loading/export lookup, never rendering.
 * Link only kernel32. Minimal Win32 declarations avoid a host C runtime or SDK
 * header dependency; the calling convention remains explicit for PE32.
 */
typedef void *HANDLE;
typedef void *HMODULE;
typedef unsigned long DWORD;
typedef int BOOL;
typedef void (__stdcall *FARPROC)(void);
#define IMPORT __declspec(dllimport)
IMPORT HMODULE __stdcall LoadLibraryExA(const char *, HANDLE, DWORD);
IMPORT FARPROC __stdcall GetProcAddress(HMODULE, const char *);
IMPORT DWORD __stdcall GetModuleFileNameA(HMODULE, char *, DWORD);
IMPORT BOOL __stdcall FreeLibrary(HMODULE);
IMPORT HANDLE __stdcall GetStdHandle(DWORD);
IMPORT BOOL __stdcall WriteFile(HANDLE, const void *, DWORD, DWORD *, void *);
IMPORT DWORD __stdcall GetLastError(void);
IMPORT void __stdcall ExitProcess(unsigned int);

#ifdef _WIN64
#define BITS "64"
#else
#define BITS "32"
#endif

static void write_text(const char *text) {
    DWORD length = 0, written = 0;
    while (text[length]) ++length;
    WriteFile(GetStdHandle((DWORD)-11), text, length, &written, (void *)0);
}

static void failure(const char *dll, const char *operation) {
    DWORD error = GetLastError();
    char hex[9];
    static const char digits[] = "0123456789abcdef";
    for (unsigned int i = 0; i != 8; ++i)
        hex[i] = digits[(error >> ((7 - i) * 4)) & 15];
    hex[8] = 0;
    write_text("TILO_DXVK_LOAD_FAIL_" BITS " ");
    write_text(dll);
    write_text(" ");
    write_text(operation);
    write_text(" error=0x");
    write_text(hex);
    write_text("\n");
}

void mainCRTStartup(void) {
    static const struct {
        const char *dll;
        const char *symbol;
    } probes[] = {
        {"d3d8.dll", "Direct3DCreate8"},
        {"d3d9.dll", "Direct3DCreate9"},
        {"d3d10core.dll", "D3D10CoreCreateDevice"},
        {"d3d11.dll", "D3D11CreateDevice"},
        {"dxgi.dll", "CreateDXGIFactory1"},
    };
    unsigned int failures = 0;
    for (unsigned int i = 0; i != sizeof(probes) / sizeof(probes[0]); ++i) {
        /* LOAD_LIBRARY_SEARCH_SYSTEM32 excludes an app-local lookalike. Wine
         * applies normal 32-bit system directory redirection where applicable.
         */
        HMODULE module = LoadLibraryExA(probes[i].dll, (HANDLE)0, 0x800);
        char path[1024];
        if (!module) {
            failure(probes[i].dll, "LoadLibraryExA");
            ++failures;
            continue;
        }
        if (!GetProcAddress(module, probes[i].symbol)) {
            failure(probes[i].dll, "GetProcAddress");
            ++failures;
            FreeLibrary(module);
            continue;
        }
        DWORD length = GetModuleFileNameA(module, path, sizeof(path));
        if (!length || length >= sizeof(path)) {
            failure(probes[i].dll, "GetModuleFileNameA");
            ++failures;
            FreeLibrary(module);
            continue;
        }
        write_text("TILO_DXVK_MODULE_" BITS " ");
        write_text(probes[i].dll);
        write_text(" ");
        write_text(path);
        write_text("\n");
        write_text("TILO_DXVK_EXPORT_" BITS " ");
        write_text(probes[i].dll);
        write_text(" ");
        write_text(probes[i].symbol);
        write_text("\n");
        FreeLibrary(module);
    }
    if (!failures) write_text("TILO_DXVK_LOAD_PASS_" BITS "\n");
    ExitProcess(failures ? 1 : 0);
}
