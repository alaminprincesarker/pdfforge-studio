#include <windows.h>

static HMODULE get_wv2_module(void) {
    HMODULE h = GetModuleHandleW(L"WebView2Loader.dll");
    if (!h) {
        h = LoadLibraryW(L"WebView2Loader.dll");
    }
    return h;
}

typedef HRESULT (WINAPI *PFN_CompareBrowserVersions)(PCWSTR, PCWSTR, int*);
typedef HRESULT (WINAPI *PFN_CreateCoreWebView2Environment)(void*);
typedef HRESULT (WINAPI *PFN_CreateCoreWebView2EnvironmentWithOptions)(PCWSTR, PCWSTR, void*, void*);
typedef HRESULT (WINAPI *PFN_GetAvailableCoreWebView2BrowserVersionString)(PCWSTR, PWSTR*);
typedef HRESULT (WINAPI *PFN_GetAvailableCoreWebView2BrowserVersionStringWithOptions)(PCWSTR, void*, PWSTR*);

HRESULT WINAPI CompareBrowserVersions(PCWSTR version1, PCWSTR version2, int* result) {
    HMODULE h = get_wv2_module();
    if (!h) return E_FAIL;
    PFN_CompareBrowserVersions fn = (PFN_CompareBrowserVersions)GetProcAddress(h, "CompareBrowserVersions");
    if (!fn) return E_FAIL;
    return fn(version1, version2, result);
}

HRESULT WINAPI CreateCoreWebView2Environment(void* environmentCreatedHandler) {
    HMODULE h = get_wv2_module();
    if (!h) return E_FAIL;
    PFN_CreateCoreWebView2Environment fn = (PFN_CreateCoreWebView2Environment)GetProcAddress(h, "CreateCoreWebView2Environment");
    if (!fn) return E_FAIL;
    return fn(environmentCreatedHandler);
}

HRESULT WINAPI CreateCoreWebView2EnvironmentWithOptions(
    PCWSTR browserExecutableFolder,
    PCWSTR userDataFolder,
    void* environmentOptions,
    void* environmentCreatedHandler
) {
    HMODULE h = get_wv2_module();
    if (!h) return E_FAIL;
    PFN_CreateCoreWebView2EnvironmentWithOptions fn =
        (PFN_CreateCoreWebView2EnvironmentWithOptions)GetProcAddress(h, "CreateCoreWebView2EnvironmentWithOptions");
    if (!fn) return E_FAIL;
    return fn(browserExecutableFolder, userDataFolder, environmentOptions, environmentCreatedHandler);
}

HRESULT WINAPI GetAvailableCoreWebView2BrowserVersionString(
    PCWSTR browserExecutableFolder,
    PWSTR* versionInfo
) {
    HMODULE h = get_wv2_module();
    if (!h) return E_FAIL;
    PFN_GetAvailableCoreWebView2BrowserVersionString fn =
        (PFN_GetAvailableCoreWebView2BrowserVersionString)GetProcAddress(h, "GetAvailableCoreWebView2BrowserVersionString");
    if (!fn) return E_FAIL;
    return fn(browserExecutableFolder, versionInfo);
}

HRESULT WINAPI GetAvailableCoreWebView2BrowserVersionStringWithOptions(
    PCWSTR browserExecutableFolder,
    void* environmentOptions,
    PWSTR* versionInfo
) {
    HMODULE h = get_wv2_module();
    if (!h) return E_FAIL;
    PFN_GetAvailableCoreWebView2BrowserVersionStringWithOptions fn =
        (PFN_GetAvailableCoreWebView2BrowserVersionStringWithOptions)GetProcAddress(h, "GetAvailableCoreWebView2BrowserVersionStringWithOptions");
    if (!fn) return E_FAIL;
    return fn(browserExecutableFolder, environmentOptions, versionInfo);
}

