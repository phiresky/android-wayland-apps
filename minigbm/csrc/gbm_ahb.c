/*
 * GBM implementation backed by Android AHardwareBuffer.
 *
 * Provides the standard GBM API (gbm_create_device, gbm_bo_create, etc.)
 * using AHardwareBuffer for allocation. Extracts dmabuf fds from the
 * native_handle_t embedded in each AHardwareBuffer, enabling buffer sharing
 * with Linux clients running in proot.
 *
 * SPDX-License-Identifier: BSD-3-Clause
 */

#include <android/hardware_buffer.h>
#include <android/log.h>
#include <dlfcn.h>
#include <errno.h>
#include <pthread.h>
#include <stdint.h>
#include <stdlib.h>
#include <string.h>
#include <sys/system_properties.h>
#include <unistd.h>

#include "gbm_ahb.h"

#define LOG_TAG "gbm_ahb"
#define LOGI(...) __android_log_print(ANDROID_LOG_INFO, LOG_TAG, __VA_ARGS__)
#define LOGW(...) __android_log_print(ANDROID_LOG_WARN, LOG_TAG, __VA_ARGS__)
#define LOGE(...) __android_log_print(ANDROID_LOG_ERROR, LOG_TAG, __VA_ARGS__)

/* ── native_handle_t ──────────────────────────────────────────────────────
 *
 * The NDK doesn't expose native_handle_t, but the ABI is stable:
 *   struct native_handle_t {
 *       int version;    // = sizeof(native_handle_t)
 *       int numFds;     // number of file descriptors
 *       int numInts;    // number of ints following fds
 *       int data[];     // fds[numFds] then ints[numInts]
 *   };
 *
 * On Qualcomm (and most Android vendors), the first fd in data[] is the
 * dmabuf fd for the buffer.
 */
struct native_handle_t {
    int version;
    int numFds;
    int numInts;
    int data[0]; /* fds[numFds], then ints[numInts] */
};

/* Private NDK function to get native handle from AHardwareBuffer.
 * Available since Android 8.0, stable ABI, but not in NDK headers. */
typedef const struct native_handle_t *(*pfn_AHardwareBuffer_getNativeHandle)(
    const AHardwareBuffer *buffer);

/* ── Format conversion tables ─────────────────────────────────────────── */

/* DRM fourcc → AHardwareBuffer format */
static uint32_t drm_to_ahb_format(uint32_t drm_fmt) {
    switch (drm_fmt) {
    /* GBM_FORMAT_XRGB8888 = DRM_FORMAT_XRGB8888 */
    case 0x34325258: /* 'XR24' */
        return AHARDWAREBUFFER_FORMAT_R8G8B8X8_UNORM;
    /* GBM_FORMAT_ARGB8888 = DRM_FORMAT_ARGB8888 */
    case 0x34325241: /* 'AR24' */
        return AHARDWAREBUFFER_FORMAT_R8G8B8A8_UNORM;
    /* GBM_FORMAT_XBGR8888 = DRM_FORMAT_XBGR8888 */
    case 0x34324258: /* 'XB24' */
        return AHARDWAREBUFFER_FORMAT_R8G8B8X8_UNORM;
    /* GBM_FORMAT_ABGR8888 = DRM_FORMAT_ABGR8888 */
    case 0x34324241: /* 'AB24' */
        return AHARDWAREBUFFER_FORMAT_R8G8B8A8_UNORM;
    /* GBM_FORMAT_RGB565 = DRM_FORMAT_RGB565 */
    case 0x36314752: /* 'RG16' */
        return AHARDWAREBUFFER_FORMAT_R5G6B5_UNORM;
    /* GBM_FORMAT_RGB888 = DRM_FORMAT_RGB888 */
    case 0x34324752: /* 'RG24' */
        return AHARDWAREBUFFER_FORMAT_R8G8B8_UNORM;
    /* GBM_FORMAT_ABGR2101010 */
    case 0x30334241: /* 'AB30' */
        return AHARDWAREBUFFER_FORMAT_R10G10B10A2_UNORM;
    /* GBM_FORMAT_ABGR16161616F */
    case 0x48344241: /* 'AB4H' */
        return AHARDWAREBUFFER_FORMAT_R16G16B16A16_FLOAT;
    default:
        return 0; /* unsupported */
    }
}

/* AHardwareBuffer format → DRM fourcc (used by Rust FFI for format queries) */
__attribute__((unused))
static uint32_t ahb_to_drm_format(uint32_t ahb_fmt) {
    switch (ahb_fmt) {
    case AHARDWAREBUFFER_FORMAT_R8G8B8A8_UNORM:
        return 0x34324241; /* DRM_FORMAT_ABGR8888 */
    case AHARDWAREBUFFER_FORMAT_R8G8B8X8_UNORM:
        return 0x34324258; /* DRM_FORMAT_XBGR8888 */
    case AHARDWAREBUFFER_FORMAT_R5G6B5_UNORM:
        return 0x36314752; /* DRM_FORMAT_RGB565 */
    case AHARDWAREBUFFER_FORMAT_R8G8B8_UNORM:
        return 0x34324752; /* DRM_FORMAT_RGB888 */
    case AHARDWAREBUFFER_FORMAT_R10G10B10A2_UNORM:
        return 0x30334241; /* DRM_FORMAT_ABGR2101010 */
    case AHARDWAREBUFFER_FORMAT_R16G16B16A16_FLOAT:
        return 0x48344241; /* DRM_FORMAT_ABGR16161616F */
    default:
        return 0;
    }
}

/* Bytes per pixel for a DRM fourcc format (plane 0) */
static uint32_t drm_bpp(uint32_t drm_fmt) {
    switch (drm_fmt) {
    case 0x36314752: /* RGB565 */
        return 2;
    case 0x34324752: /* RGB888 / BGR888 */
    case 0x34324742:
        return 3;
    case 0x34325258: /* XRGB8888 */
    case 0x34325241: /* ARGB8888 */
    case 0x34324258: /* XBGR8888 */
    case 0x34324241: /* ABGR8888 */
    case 0x34325852: /* RGBX8888 */
    case 0x34325842: /* BGRX8888 */
    case 0x34324152: /* RGBA8888 */
    case 0x34324142: /* BGRA8888 */
    case 0x30334241: /* ABGR2101010 */
        return 4;
    case 0x48344241: /* ABGR16161616F */
        return 8;
    default:
        return 4; /* assume 32bpp */
    }
}

/* ── GBM data structures ─────────────────────────────────────────────── */

struct gbm_device {
    int fd; /* stored but not used for allocation */
    pfn_AHardwareBuffer_getNativeHandle getNativeHandle;
};

struct gbm_bo {
    struct gbm_device *device;
    AHardwareBuffer *ahb;
    uint32_t width;
    uint32_t height;
    uint32_t stride; /* in bytes */
    uint32_t format; /* DRM fourcc */
    int dmabuf_fd;   /* cached dmabuf fd, -1 = not extracted yet */
    void *user_data;
    void (*destroy_user_data)(struct gbm_bo *, void *);
};

struct gbm_surface {
    uint32_t width;
    uint32_t height;
    uint32_t format;
};

/* ── GBM device API ───────────────────────────────────────────────────── */

struct gbm_device *gbm_create_device(int fd) {
    struct gbm_device *dev = calloc(1, sizeof(*dev));
    if (!dev) return NULL;

    dev->fd = fd;

    /* Resolve AHardwareBuffer_getNativeHandle at runtime */
    void *lib = dlopen("libandroid.so", RTLD_LAZY);
    if (lib) {
        dev->getNativeHandle =
            (pfn_AHardwareBuffer_getNativeHandle)dlsym(lib, "AHardwareBuffer_getNativeHandle");
        /* Don't dlclose — we need the symbol to stay valid */
    }

    if (!dev->getNativeHandle) {
        LOGW("AHardwareBuffer_getNativeHandle not found; dmabuf fd extraction will fail");
    } else {
        LOGI("gbm_create_device: AHardwareBuffer backend ready (fd=%d)", fd);
    }

    return dev;
}

void gbm_device_destroy(struct gbm_device *dev) {
    if (dev) {
        LOGI("gbm_device_destroy");
        free(dev);
    }
}

int gbm_device_get_fd(struct gbm_device *dev) {
    return dev ? dev->fd : -1;
}

const char *gbm_device_get_backend_name(struct gbm_device *dev) {
    (void)dev;
    return "ahardwarebuffer";
}

int gbm_device_is_format_supported(struct gbm_device *dev, uint32_t format,
                                   uint32_t usage) {
    (void)dev;
    (void)usage;
    return drm_to_ahb_format(format) != 0;
}

int gbm_device_get_format_modifier_plane_count(struct gbm_device *dev,
                                               uint32_t format,
                                               uint64_t modifier) {
    (void)dev;
    (void)format;
    (void)modifier;
    return 1; /* single plane for all supported formats */
}

/* ── GBM buffer object API ────────────────────────────────────────────── */

/* Extract dmabuf fd from AHardwareBuffer via native_handle_t */
static int extract_dmabuf_fd(struct gbm_device *dev, AHardwareBuffer *ahb) {
    if (!dev->getNativeHandle) {
        LOGE("extract_dmabuf_fd: getNativeHandle not available");
        return -1;
    }

    const struct native_handle_t *handle = dev->getNativeHandle(ahb);
    if (!handle) {
        LOGE("extract_dmabuf_fd: getNativeHandle returned NULL");
        return -1;
    }

    if (handle->numFds < 1) {
        LOGE("extract_dmabuf_fd: native handle has no fds (numFds=%d)",
             handle->numFds);
        return -1;
    }

    /* dup the fd so the caller owns it independently */
    int fd = dup(handle->data[0]);
    if (fd < 0) {
        LOGE("extract_dmabuf_fd: dup failed: %s", strerror(errno));
        return -1;
    }

    LOGI("extract_dmabuf_fd: got dmabuf fd=%d (orig=%d, numFds=%d, numInts=%d)",
         fd, handle->data[0], handle->numFds, handle->numInts);
    return fd;
}

struct gbm_bo *gbm_bo_create(struct gbm_device *dev, uint32_t width,
                             uint32_t height, uint32_t format,
                             uint32_t flags) {
    if (!dev) return NULL;

    uint32_t ahb_format = drm_to_ahb_format(format);
    if (ahb_format == 0) {
        LOGE("gbm_bo_create: unsupported format 0x%08x", format);
        return NULL;
    }

    /* Map GBM flags to AHB usage */
    uint64_t usage = AHARDWAREBUFFER_USAGE_GPU_SAMPLED_IMAGE
                   | AHARDWAREBUFFER_USAGE_GPU_FRAMEBUFFER
                   | AHARDWAREBUFFER_USAGE_COMPOSER_OVERLAY;

    if (flags & GBM_BO_USE_SW_READ_OFTEN)
        usage |= AHARDWAREBUFFER_USAGE_CPU_READ_OFTEN;
    if (flags & GBM_BO_USE_SW_WRITE_OFTEN)
        usage |= AHARDWAREBUFFER_USAGE_CPU_WRITE_OFTEN;
    if (flags & GBM_BO_USE_SW_READ_RARELY)
        usage |= AHARDWAREBUFFER_USAGE_CPU_READ_RARELY;
    if (flags & GBM_BO_USE_SW_WRITE_RARELY)
        usage |= AHARDWAREBUFFER_USAGE_CPU_WRITE_RARELY;

    AHardwareBuffer_Desc desc = {
        .width = width,
        .height = height,
        .layers = 1,
        .format = ahb_format,
        .usage = usage,
        .stride = 0,
        .rfu0 = 0,
        .rfu1 = 0,
    };

    AHardwareBuffer *ahb = NULL;
    int ret = AHardwareBuffer_allocate(&desc, &ahb);
    if (ret != 0 || !ahb) {
        LOGE("gbm_bo_create: AHardwareBuffer_allocate failed (ret=%d, %dx%d fmt=0x%x)",
             ret, width, height, ahb_format);
        return NULL;
    }

    /* Query actual description (stride may differ from requested) */
    AHardwareBuffer_Desc actual;
    AHardwareBuffer_describe(ahb, &actual);

    struct gbm_bo *bo = calloc(1, sizeof(*bo));
    if (!bo) {
        AHardwareBuffer_release(ahb);
        return NULL;
    }

    bo->device = dev;
    bo->ahb = ahb;
    bo->width = actual.width;
    bo->height = actual.height;
    bo->stride = actual.stride * drm_bpp(format); /* stride in bytes */
    bo->format = format;
    bo->dmabuf_fd = -1; /* lazy extraction */

    LOGI("gbm_bo_create: %dx%d fmt=0x%08x stride=%u (ahb=%p)",
         bo->width, bo->height, bo->format, bo->stride, ahb);

    return bo;
}

struct gbm_bo *gbm_bo_create_with_modifiers(struct gbm_device *dev,
                                            uint32_t width, uint32_t height,
                                            uint32_t format,
                                            const uint64_t *modifiers,
                                            const unsigned int count) {
    (void)modifiers;
    (void)count;
    /* AHardwareBuffer doesn't support explicit modifiers; create LINEAR */
    return gbm_bo_create(dev, width, height, format,
                         GBM_BO_USE_RENDERING | GBM_BO_USE_SCANOUT);
}

struct gbm_bo *gbm_bo_create_with_modifiers2(struct gbm_device *dev,
                                             uint32_t width, uint32_t height,
                                             uint32_t format,
                                             const uint64_t *modifiers,
                                             const unsigned int count,
                                             uint32_t flags) {
    (void)modifiers;
    (void)count;
    return gbm_bo_create(dev, width, height, format, flags);
}

void gbm_bo_destroy(struct gbm_bo *bo) {
    if (!bo) return;

    if (bo->destroy_user_data) {
        bo->destroy_user_data(bo, bo->user_data);
    }

    if (bo->dmabuf_fd >= 0) {
        close(bo->dmabuf_fd);
    }

    if (bo->ahb) {
        AHardwareBuffer_release(bo->ahb);
    }

    LOGI("gbm_bo_destroy: %dx%d", bo->width, bo->height);
    free(bo);
}

uint32_t gbm_bo_get_width(struct gbm_bo *bo) {
    return bo ? bo->width : 0;
}

uint32_t gbm_bo_get_height(struct gbm_bo *bo) {
    return bo ? bo->height : 0;
}

uint32_t gbm_bo_get_stride(struct gbm_bo *bo) {
    return bo ? bo->stride : 0;
}

uint32_t gbm_bo_get_stride_for_plane(struct gbm_bo *bo, int plane) {
    (void)plane;
    return bo ? bo->stride : 0;
}

uint32_t gbm_bo_get_format(struct gbm_bo *bo) {
    return bo ? bo->format : 0;
}

uint32_t gbm_bo_get_bpp(struct gbm_bo *bo) {
    return bo ? drm_bpp(bo->format) : 0;
}

uint64_t gbm_bo_get_modifier(struct gbm_bo *bo) {
    (void)bo;
    return 0; /* DRM_FORMAT_MOD_LINEAR */
}

struct gbm_device *gbm_bo_get_device(struct gbm_bo *bo) {
    return bo ? bo->device : NULL;
}

union gbm_bo_handle gbm_bo_get_handle(struct gbm_bo *bo) {
    union gbm_bo_handle h;
    h.ptr = bo ? bo->ahb : NULL;
    return h;
}

int gbm_bo_get_fd(struct gbm_bo *bo) {
    if (!bo) return -1;

    /* Return a dup'd fd each time (GBM API contract) */
    if (bo->dmabuf_fd < 0) {
        bo->dmabuf_fd = extract_dmabuf_fd(bo->device, bo->ahb);
    }

    if (bo->dmabuf_fd < 0) return -1;

    int fd = dup(bo->dmabuf_fd);
    if (fd < 0) {
        LOGE("gbm_bo_get_fd: dup failed: %s", strerror(errno));
    }
    return fd;
}

int gbm_bo_get_fd_for_plane(struct gbm_bo *bo, int plane) {
    (void)plane;
    return gbm_bo_get_fd(bo); /* single-plane buffers */
}

int gbm_bo_get_plane_count(struct gbm_bo *bo) {
    (void)bo;
    return 1;
}

union gbm_bo_handle gbm_bo_get_handle_for_plane(struct gbm_bo *bo,
                                                int plane) {
    (void)plane;
    return gbm_bo_get_handle(bo);
}

uint32_t gbm_bo_get_offset(struct gbm_bo *bo, int plane) {
    (void)bo;
    (void)plane;
    return 0;
}

int gbm_bo_write(struct gbm_bo *bo, const void *buf, size_t count) {
    (void)bo;
    (void)buf;
    (void)count;
    return -ENOSYS;
}

void gbm_bo_set_user_data(struct gbm_bo *bo, void *data,
                          void (*destroy_user_data)(struct gbm_bo *, void *)) {
    if (!bo) return;
    bo->user_data = data;
    bo->destroy_user_data = destroy_user_data;
}

void *gbm_bo_get_user_data(struct gbm_bo *bo) {
    return bo ? bo->user_data : NULL;
}

/* ── GBM buffer import ────────────────────────────────────────────────── */

struct gbm_bo *gbm_bo_import(struct gbm_device *dev, uint32_t type,
                             void *buffer, uint32_t usage) {
    (void)dev;
    (void)type;
    (void)buffer;
    (void)usage;
    LOGW("gbm_bo_import: not implemented for AHardwareBuffer backend");
    return NULL;
}

/* ── GBM buffer mapping ───────────────────────────────────────────────── */

void *gbm_bo_map(struct gbm_bo *bo, uint32_t x, uint32_t y, uint32_t width,
                 uint32_t height, uint32_t flags, uint32_t *stride,
                 void **map_data) {
    if (!bo || !stride || !map_data) return NULL;

    uint64_t usage = 0;
    if (flags & 0x1) /* GBM_BO_TRANSFER_READ */
        usage |= AHARDWAREBUFFER_USAGE_CPU_READ_OFTEN;
    if (flags & 0x2) /* GBM_BO_TRANSFER_WRITE */
        usage |= AHARDWAREBUFFER_USAGE_CPU_WRITE_OFTEN;

    void *addr = NULL;
    int ret = AHardwareBuffer_lock(bo->ahb, usage, -1, NULL, &addr);
    if (ret != 0) {
        LOGE("gbm_bo_map: AHardwareBuffer_lock failed (ret=%d)", ret);
        return NULL;
    }

    *stride = bo->stride;
    /* Store a sentinel so gbm_bo_unmap knows we used AHardwareBuffer_lock */
    *map_data = bo->ahb;

    /* Offset to requested region */
    uint8_t *base = (uint8_t *)addr;
    base += y * bo->stride + x * drm_bpp(bo->format);
    return base;
}

void gbm_bo_unmap(struct gbm_bo *bo, void *map_data) {
    if (!bo || !map_data) return;
    AHardwareBuffer_unlock(bo->ahb, NULL);
}

/* ── GBM surface (stub) ──────────────────────────────────────────────── */

struct gbm_surface *gbm_surface_create(struct gbm_device *dev, uint32_t width,
                                       uint32_t height, uint32_t format,
                                       uint32_t flags) {
    (void)dev;
    (void)flags;
    struct gbm_surface *s = calloc(1, sizeof(*s));
    if (s) {
        s->width = width;
        s->height = height;
        s->format = format;
    }
    return s;
}

struct gbm_surface *gbm_surface_create_with_modifiers(
    struct gbm_device *dev, uint32_t width, uint32_t height, uint32_t format,
    const uint64_t *modifiers, const unsigned int count) {
    (void)modifiers;
    (void)count;
    return gbm_surface_create(dev, width, height, format, 0);
}

struct gbm_bo *gbm_surface_lock_front_buffer(struct gbm_surface *surface) {
    (void)surface;
    return NULL;
}

void gbm_surface_release_buffer(struct gbm_surface *surface,
                                struct gbm_bo *bo) {
    (void)surface;
    (void)bo;
}

int gbm_surface_has_free_buffers(struct gbm_surface *surface) {
    (void)surface;
    return 0;
}

void gbm_surface_destroy(struct gbm_surface *surface) {
    free(surface);
}

/* ── Minigbm extras ───────────────────────────────────────────────────── */

uint32_t gbm_bo_get_plane_size(struct gbm_bo *bo, size_t plane) {
    (void)plane;
    return bo ? bo->stride * bo->height : 0;
}

int gbm_bo_get_plane_fd(struct gbm_bo *bo, size_t plane) {
    (void)plane;
    return gbm_bo_get_fd(bo);
}

/* ── AHardwareBuffer accessor (for Rust side) ─────────────────────────── */

AHardwareBuffer *gbm_bo_get_ahardwarebuffer(struct gbm_bo *bo) {
    return bo ? bo->ahb : NULL;
}
