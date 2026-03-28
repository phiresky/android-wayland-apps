/*
 * GBM API header for AHardwareBuffer backend.
 *
 * Compatible subset of minigbm's gbm.h, plus an Android-specific
 * extension to retrieve the underlying AHardwareBuffer pointer.
 *
 * SPDX-License-Identifier: BSD-3-Clause
 */

#ifndef GBM_AHB_H
#define GBM_AHB_H

#include <stddef.h>
#include <stdint.h>

#ifdef __cplusplus
extern "C" {
#endif

struct gbm_device;
struct gbm_bo;
struct gbm_surface;

/* ── Format codes (DRM fourcc, same as minigbm/gbm.h) ────────────────── */

#define __gbm_fourcc_code(a, b, c, d)                                          \
    ((uint32_t)(a) | ((uint32_t)(b) << 8) | ((uint32_t)(c) << 16) |           \
     ((uint32_t)(d) << 24))

#define GBM_FORMAT_XRGB8888 __gbm_fourcc_code('X', 'R', '2', '4')
#define GBM_FORMAT_XBGR8888 __gbm_fourcc_code('X', 'B', '2', '4')
#define GBM_FORMAT_ARGB8888 __gbm_fourcc_code('A', 'R', '2', '4')
#define GBM_FORMAT_ABGR8888 __gbm_fourcc_code('A', 'B', '2', '4')
#define GBM_FORMAT_RGBX8888 __gbm_fourcc_code('R', 'X', '2', '4')
#define GBM_FORMAT_BGRX8888 __gbm_fourcc_code('B', 'X', '2', '4')
#define GBM_FORMAT_RGBA8888 __gbm_fourcc_code('R', 'A', '2', '4')
#define GBM_FORMAT_BGRA8888 __gbm_fourcc_code('B', 'A', '2', '4')
#define GBM_FORMAT_RGB565 __gbm_fourcc_code('R', 'G', '1', '6')
#define GBM_FORMAT_RGB888 __gbm_fourcc_code('R', 'G', '2', '4')
#define GBM_FORMAT_ABGR2101010 __gbm_fourcc_code('A', 'B', '3', '0')
#define GBM_FORMAT_ABGR16161616F __gbm_fourcc_code('A', 'B', '4', 'H')

/* ── Usage flags (same values as minigbm/gbm.h) ──────────────────────── */

enum gbm_bo_flags {
    GBM_BO_USE_SCANOUT = (1 << 0),
    GBM_BO_USE_CURSOR = (1 << 1),
    GBM_BO_USE_RENDERING = (1 << 2),
    GBM_BO_USE_WRITE = (1 << 3),
    GBM_BO_USE_LINEAR = (1 << 4),
    GBM_BO_USE_TEXTURING = (1 << 5),
    GBM_BO_USE_CAMERA_WRITE = (1 << 6),
    GBM_BO_USE_CAMERA_READ = (1 << 7),
    GBM_BO_USE_PROTECTED = (1 << 8),
    GBM_BO_USE_SW_READ_OFTEN = (1 << 9),
    GBM_BO_USE_SW_READ_RARELY = (1 << 10),
    GBM_BO_USE_SW_WRITE_OFTEN = (1 << 11),
    GBM_BO_USE_SW_WRITE_RARELY = (1 << 12),
    GBM_BO_USE_HW_VIDEO_DECODER = (1 << 13),
    GBM_BO_USE_HW_VIDEO_ENCODER = (1 << 14),
    GBM_BO_USE_FRONT_RENDERING = (1 << 16),
    GBM_BO_USE_GPU_DATA_BUFFER = (1 << 18),
};

/* ── Buffer object handle ─────────────────────────────────────────────── */

union gbm_bo_handle {
    void *ptr;
    int32_t s32;
    uint32_t u32;
    int64_t s64;
    uint64_t u64;
};

/* ── Transfer flags for gbm_bo_map ────────────────────────────────────── */

enum gbm_bo_transfer_flags {
    GBM_BO_TRANSFER_READ = (1 << 0),
    GBM_BO_TRANSFER_WRITE = (1 << 1),
    GBM_BO_TRANSFER_READ_WRITE = (GBM_BO_TRANSFER_READ | GBM_BO_TRANSFER_WRITE),
};

/* ── Import types ─────────────────────────────────────────────────────── */

#define GBM_BO_IMPORT_FD 0x5503
#define GBM_BO_IMPORT_FD_MODIFIER 0x5505
#define GBM_MAX_PLANES 4

struct gbm_import_fd_data {
    int fd;
    uint32_t width;
    uint32_t height;
    uint32_t stride;
    uint32_t format;
};

struct gbm_import_fd_modifier_data {
    uint32_t width;
    uint32_t height;
    uint32_t format;
    uint32_t num_fds;
    int fds[GBM_MAX_PLANES];
    int strides[GBM_MAX_PLANES];
    int offsets[GBM_MAX_PLANES];
    uint64_t modifier;
};

/* ── Device API ───────────────────────────────────────────────────────── */

struct gbm_device *gbm_create_device(int fd);
void gbm_device_destroy(struct gbm_device *dev);
int gbm_device_get_fd(struct gbm_device *dev);
const char *gbm_device_get_backend_name(struct gbm_device *dev);
int gbm_device_is_format_supported(struct gbm_device *dev, uint32_t format,
                                   uint32_t usage);
int gbm_device_get_format_modifier_plane_count(struct gbm_device *dev,
                                               uint32_t format,
                                               uint64_t modifier);

/* ── Buffer object API ────────────────────────────────────────────────── */

struct gbm_bo *gbm_bo_create(struct gbm_device *dev, uint32_t width,
                             uint32_t height, uint32_t format, uint32_t flags);
struct gbm_bo *gbm_bo_create_with_modifiers(struct gbm_device *dev,
                                            uint32_t width, uint32_t height,
                                            uint32_t format,
                                            const uint64_t *modifiers,
                                            const unsigned int count);
struct gbm_bo *gbm_bo_create_with_modifiers2(struct gbm_device *dev,
                                             uint32_t width, uint32_t height,
                                             uint32_t format,
                                             const uint64_t *modifiers,
                                             const unsigned int count,
                                             uint32_t flags);
void gbm_bo_destroy(struct gbm_bo *bo);

struct gbm_bo *gbm_bo_import(struct gbm_device *dev, uint32_t type,
                             void *buffer, uint32_t usage);

void *gbm_bo_map(struct gbm_bo *bo, uint32_t x, uint32_t y, uint32_t width,
                 uint32_t height, uint32_t flags, uint32_t *stride,
                 void **map_data);
void gbm_bo_unmap(struct gbm_bo *bo, void *map_data);

uint32_t gbm_bo_get_width(struct gbm_bo *bo);
uint32_t gbm_bo_get_height(struct gbm_bo *bo);
uint32_t gbm_bo_get_stride(struct gbm_bo *bo);
uint32_t gbm_bo_get_stride_for_plane(struct gbm_bo *bo, int plane);
uint32_t gbm_bo_get_format(struct gbm_bo *bo);
uint32_t gbm_bo_get_bpp(struct gbm_bo *bo);
uint64_t gbm_bo_get_modifier(struct gbm_bo *bo);
struct gbm_device *gbm_bo_get_device(struct gbm_bo *bo);
union gbm_bo_handle gbm_bo_get_handle(struct gbm_bo *bo);
int gbm_bo_get_fd(struct gbm_bo *bo);
int gbm_bo_get_fd_for_plane(struct gbm_bo *bo, int plane);
int gbm_bo_get_plane_count(struct gbm_bo *bo);
union gbm_bo_handle gbm_bo_get_handle_for_plane(struct gbm_bo *bo, int plane);
uint32_t gbm_bo_get_offset(struct gbm_bo *bo, int plane);
int gbm_bo_write(struct gbm_bo *bo, const void *buf, size_t count);
void gbm_bo_set_user_data(struct gbm_bo *bo, void *data,
                          void (*destroy_user_data)(struct gbm_bo *, void *));
void *gbm_bo_get_user_data(struct gbm_bo *bo);

/* ── Minigbm extras ───────────────────────────────────────────────────── */

uint32_t gbm_bo_get_plane_size(struct gbm_bo *bo, size_t plane);
int gbm_bo_get_plane_fd(struct gbm_bo *bo, size_t plane);

/* ── Surface (stub) ───────────────────────────────────────────────────── */

struct gbm_surface *gbm_surface_create(struct gbm_device *dev, uint32_t width,
                                       uint32_t height, uint32_t format,
                                       uint32_t flags);
struct gbm_surface *gbm_surface_create_with_modifiers(
    struct gbm_device *dev, uint32_t width, uint32_t height, uint32_t format,
    const uint64_t *modifiers, const unsigned int count);
struct gbm_bo *gbm_surface_lock_front_buffer(struct gbm_surface *surface);
void gbm_surface_release_buffer(struct gbm_surface *surface,
                                struct gbm_bo *bo);
int gbm_surface_has_free_buffers(struct gbm_surface *surface);
void gbm_surface_destroy(struct gbm_surface *surface);

/* ── Android extension ────────────────────────────────────────────────── */

/* Forward-declare; actual type from <android/hardware_buffer.h> */
struct AHardwareBuffer;

/* Get the underlying AHardwareBuffer for Vulkan import / ASurfaceTransaction */
struct AHardwareBuffer *gbm_bo_get_ahardwarebuffer(struct gbm_bo *bo);

#ifdef __cplusplus
}
#endif

#endif /* GBM_AHB_H */
