/*
 * Vulkan ICD that loads Android's proprietary Vulkan driver via libhybris.
 *
 * This allows glibc programs (inside proot) to use the Android GPU driver
 * with zero command serialization overhead — direct function pointer calls.
 *
 * Three fixes are needed for Android 14+ bionic compatibility:
 *   1. Bionic TLS: allocate fake bionic_tls at TPIDR_EL0[-1]
 *   2. CFI: patch __cfi_slowpath in bionic libdl to RET
 *   3. Stack guard: set TLS slot 5 to a valid canary
 *
 * Build (inside proot Arch):
 *   clang -shared -fPIC -fno-stack-protector -o libvulkan_hybris.so \
 *         vulkan_hybris_icd.c -lhybris-common
 *
 * Install:
 *   cp libvulkan_hybris.so /usr/lib/
 *   cp hybris_vulkan_icd.json /usr/share/vulkan/icd.d/
 */

#define _GNU_SOURCE
#include <stdio.h>
#include <stdlib.h>
#include <stdint.h>
#include <string.h>
#include <unistd.h>
#include <dlfcn.h>
#include <sys/mman.h>
#include <pthread.h>
#include <hybris/common/binding.h>

/* Minimal Vulkan types — avoids depending on vulkan/vulkan.h at build time */
#define VK_NULL_HANDLE 0
#define VKAPI_ATTR
#define VKAPI_CALL
typedef uint32_t VkResult;
typedef struct VkInstance_T *VkInstance;
typedef void (*PFN_vkVoidFunction)(void);
typedef PFN_vkVoidFunction (*PFN_vkGetInstanceProcAddr)(VkInstance, const char *);
#define VK_SUCCESS 0

/* libhybris functions not in binding.h */
extern void android_update_LD_LIBRARY_PATH(const char *path);

/* ── State ─────────────────────────────────────────────────────────── */

static void *android_vulkan_handle;
static PFN_vkGetInstanceProcAddr real_get_instance_proc_addr;
static pthread_once_t init_once = PTHREAD_ONCE_INIT;
static int init_failed;

/* ── Bionic TLS fix ────────────────────────────────────────────────── */

/*
 * Android's bionic libc reads per-thread state from TPIDR_EL0 (the ARM64
 * thread pointer register). In a glibc process, TPIDR_EL0 points to glibc's
 * TCB, not bionic's. Two critical slots must be set up:
 *
 *   Slot -1 (tp - 8): pointer to bionic_tls struct (~12KB, holds locale/errno)
 *   Slot  5 (tp + 40): stack guard canary
 *
 * We allocate a fake bionic_tls and set slot -1. For slot 5, we write a
 * random canary value — this must match what bionic functions store on the
 * stack, so it must be set BEFORE any bionic code runs.
 */
static __thread void *bionic_tls_block;

static void setup_bionic_tls_for_thread(void) {
    void *tp;
    __asm__ volatile("mrs %0, tpidr_el0" : "=r"(tp));

    /* Allocate bionic_tls if not done for this thread */
    if (!bionic_tls_block) {
        bionic_tls_block = calloc(1, 0x10000);
    }

    /* Slot -1: bionic_tls pointer */
    ((void **)tp)[-1] = bionic_tls_block;

    /* NOTE: Do NOT write to TLS slot 5 (tp + 40). On bionic it's the stack guard
     * canary, but on glibc aarch64 it overlaps the DTV or other TCB data. Writing
     * here corrupts the glibc Vulkan loader's callee-saved register restore.
     * Bionic functions compiled with -fstack-protector will use whatever value
     * glibc has at this offset as the canary — this works as long as the value
     * is consistent within a thread (which it is, since glibc doesn't change it). */
}

/* ── CFI patch ─────────────────────────────────────────────────────── */

/*
 * Android 14+ system libraries use Clang CFI (Control Flow Integrity).
 * When the Vulkan loader calls into the driver via function pointers,
 * CFI validates the call target against a shadow map. Since hybris-loaded
 * libraries aren't in the CFI shadow, the check crashes.
 *
 * Fix: patch __cfi_slowpath in bionic's libdl.so to a RET instruction.
 */
static void patch_cfi(void) {
    void *libdl = android_dlopen("libdl.so", RTLD_NOW);
    if (!libdl) return;

    void *cfi = android_dlsym(libdl, "__cfi_slowpath");
    void *cfi_diag = android_dlsym(libdl, "__cfi_slowpath_diag");

    if (cfi) {
        uintptr_t page = (uintptr_t)cfi & ~0xFFFUL;
        if (mprotect((void *)page, 0x2000, PROT_READ | PROT_WRITE | PROT_EXEC) == 0) {
            /* ARM64 RET instruction */
            *(uint32_t *)cfi = 0xd65f03c0;
            if (cfi_diag)
                *(uint32_t *)cfi_diag = 0xd65f03c0;
            /* Clear instruction cache */
            __builtin___clear_cache((char *)cfi, (char *)cfi + 4);
            if (cfi_diag)
                __builtin___clear_cache((char *)cfi_diag, (char *)cfi_diag + 4);
        }
    }
}

/* ── Initialization ────────────────────────────────────────────────── */

static void do_init(void) {
    /* Set up bionic TLS for the main thread */
    setup_bionic_tls_for_thread();

    /* Tell hybris linker where to find Android libraries */
    android_update_LD_LIBRARY_PATH(
        "/system/lib64:/vendor/lib64:/vendor/lib64/hw:/system/lib64/vndk-sp");

    /* Disable CFI before loading anything heavy */
    patch_cfi();

    /* Load Android's Vulkan loader */
    android_vulkan_handle = android_dlopen("libvulkan.so", RTLD_NOW);
    if (!android_vulkan_handle) {
        fprintf(stderr, "[hybris-vulkan] Failed to load libvulkan.so: %s\n",
                android_dlerror());
        init_failed = 1;
        return;
    }

    real_get_instance_proc_addr = (PFN_vkGetInstanceProcAddr)
        android_dlsym(android_vulkan_handle, "vkGetInstanceProcAddr");
    if (!real_get_instance_proc_addr) {
        fprintf(stderr, "[hybris-vulkan] vkGetInstanceProcAddr not found\n");
        init_failed = 1;
        return;
    }

    fprintf(stderr, "[hybris-vulkan] Android Vulkan driver loaded successfully\n");
}

static void ensure_init(void) {
    pthread_once(&init_once, do_init);
}

/* ── Vulkan entry points ───────────────────────────────────────────── */

/*
 * Export vkGetInstanceProcAddr directly so this library can be used as
 * a drop-in libvulkan.so replacement WITHOUT the Khronos loader.
 *
 * The Khronos glibc Vulkan loader's ICD dispatch mechanism has
 * incompatibilities with Android driver function pointers (dispatch table
 * poison values, PAC/BTI mismatches). Bypassing it entirely avoids these.
 *
 * Apps link against our libvulkan_hybris.so (installed as libvulkan.so.1)
 * and call vkGetInstanceProcAddr directly — zero dispatch overhead.
 */

static PFN_vkVoidFunction get_proc(VkInstance instance, const char *pName) {
    ensure_init();
    setup_bionic_tls_for_thread();
    if (init_failed || !real_get_instance_proc_addr)
        return NULL;
    PFN_vkVoidFunction fn = real_get_instance_proc_addr(instance, pName);
    if (getenv("HYBRIS_VK_DEBUG"))
        fprintf(stderr, "[hybris-vk] GetInstanceProcAddr(%p, \"%s\") = %p%s\n",
                (void*)instance, pName, (void*)fn, fn ? "" : " NULL!");
    return fn;
}

__attribute__((visibility("default")))
VKAPI_ATTR PFN_vkVoidFunction VKAPI_CALL
vkGetInstanceProcAddr(VkInstance instance, const char *pName) {
    return get_proc(instance, pName);
}

/* ICD entry points — kept for compatibility if used with a loader */

__attribute__((visibility("default")))
VKAPI_ATTR PFN_vkVoidFunction VKAPI_CALL
vk_icdGetInstanceProcAddr(VkInstance instance, const char *pName) {
    return get_proc(instance, pName);
}

__attribute__((visibility("default")))
VKAPI_ATTR VkResult VKAPI_CALL
vk_icdNegotiateLoaderICDInterfaceVersion(uint32_t *pSupportedVersion) {
    if (*pSupportedVersion > 5)
        *pSupportedVersion = 5;
    return VK_SUCCESS;
}

__attribute__((visibility("default")))
VKAPI_ATTR PFN_vkVoidFunction VKAPI_CALL
vk_icdGetPhysicalDeviceProcAddr(VkInstance instance, const char *pName) {
    /* Return NULL — tells the loader to use its own dispatch for physical
     * device functions. Returning non-NULL here for non-physical-device
     * functions corrupts the loader's dispatch table. */
    (void)instance;
    (void)pName;
    return NULL;
}
