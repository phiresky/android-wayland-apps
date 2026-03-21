/*
 * GBM proxy shim for proot: replaces libgbm.so inside the rootfs.
 *
 * Forwards gbm_bo_create() to the compositor's GBM allocator server
 * via a Unix socket, receives dmabuf fds via SCM_RIGHTS.
 *
 * Mesa/Turnip links against this instead of the real libgbm, so all
 * buffer allocations go through the compositor's AHardwareBuffer pool.
 *
 * Build (inside proot):
 *   gcc -shared -fPIC -o /usr/lib/libgbm.so.1 gbm_proxy.c
 *   ln -sf libgbm.so.1 /usr/lib/libgbm.so
 *
 * SPDX-License-Identifier: MIT
 */

#include <errno.h>
#include <fcntl.h>
#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/socket.h>
#include <sys/un.h>
#include <unistd.h>

/* ── Wire protocol (must match gbm_server.rs) ──────────────────────────── */

#define MSG_ALLOC   1
#define MSG_DESTROY 2

struct alloc_request {
    uint32_t msg_type;
    uint32_t width;
    uint32_t height;
    uint32_t format;
    uint32_t flags;
    uint32_t _pad;
};

struct alloc_response {
    uint32_t success;
    uint32_t width;
    uint32_t height;
    uint32_t stride;
    uint32_t format;
    uint32_t _pad;
    uint64_t modifier;
};

/* ── GBM format codes (DRM fourcc) ─────────────────────────────────────── */

#define __gbm_fourcc_code(a, b, c, d) \
    ((uint32_t)(a) | ((uint32_t)(b) << 8) | ((uint32_t)(c) << 16) | ((uint32_t)(d) << 24))

#define GBM_FORMAT_XRGB8888 __gbm_fourcc_code('X', 'R', '2', '4')
#define GBM_FORMAT_ARGB8888 __gbm_fourcc_code('A', 'R', '2', '4')
#define GBM_FORMAT_ABGR8888 __gbm_fourcc_code('A', 'B', '2', '4')
#define GBM_FORMAT_XBGR8888 __gbm_fourcc_code('X', 'B', '2', '4')

/* ── GBM flags ─────────────────────────────────────────────────────────── */

#define GBM_BO_USE_SCANOUT   (1 << 0)
#define GBM_BO_USE_RENDERING (1 << 2)
#define GBM_BO_USE_LINEAR    (1 << 4)

/* ── Internal types ────────────────────────────────────────────────────── */

struct gbm_device {
    int sock_fd;
};

struct gbm_bo {
    struct gbm_device *dev;
    uint32_t width;
    uint32_t height;
    uint32_t stride;
    uint32_t format;
    uint64_t modifier;
    int dmabuf_fd;
    void *user_data;
    void (*destroy_fn)(struct gbm_bo *, void *);
};

union gbm_bo_handle {
    void *ptr;
    int32_t s32;
    uint32_t u32;
    int64_t s64;
    uint64_t u64;
};

/* ── Socket path ───────────────────────────────────────────────────────── */

#define GBM_SOCKET_PATH "/tmp/gbm-alloc-0"

/* ── Helpers ───────────────────────────────────────────────────────────── */

static int connect_to_server(void) {
    int fd = socket(AF_UNIX, SOCK_STREAM, 0);
    if (fd < 0) return -1;

    struct sockaddr_un addr;
    memset(&addr, 0, sizeof(addr));
    addr.sun_family = AF_UNIX;
    strncpy(addr.sun_path, GBM_SOCKET_PATH, sizeof(addr.sun_path) - 1);

    if (connect(fd, (struct sockaddr *)&addr, sizeof(addr)) < 0) {
        close(fd);
        return -1;
    }
    return fd;
}

static int recv_with_fd(int sock, void *buf, size_t len, int *out_fd) {
    char cmsg_buf[CMSG_SPACE(sizeof(int))];
    struct iovec iov = { .iov_base = buf, .iov_len = len };
    struct msghdr msg = {
        .msg_iov = &iov,
        .msg_iovlen = 1,
        .msg_control = cmsg_buf,
        .msg_controllen = sizeof(cmsg_buf),
    };

    ssize_t n = recvmsg(sock, &msg, 0);
    if (n <= 0) return (int)n;

    *out_fd = -1;
    struct cmsghdr *cmsg = CMSG_FIRSTHDR(&msg);
    if (cmsg && cmsg->cmsg_level == SOL_SOCKET && cmsg->cmsg_type == SCM_RIGHTS) {
        memcpy(out_fd, CMSG_DATA(cmsg), sizeof(int));
    }

    return (int)n;
}

/* ── GBM Device API ────────────────────────────────────────────────────── */

struct gbm_device *gbm_create_device(int fd) {
    (void)fd;
    int sock = connect_to_server();
    if (sock < 0) {
        fprintf(stderr, "[gbm-proxy] Failed to connect to %s: %s\n",
                GBM_SOCKET_PATH, strerror(errno));
        return NULL;
    }

    struct gbm_device *dev = calloc(1, sizeof(*dev));
    if (!dev) { close(sock); return NULL; }
    dev->sock_fd = sock;
    fprintf(stderr, "[gbm-proxy] Connected to compositor GBM server\n");
    return dev;
}

void gbm_device_destroy(struct gbm_device *dev) {
    if (!dev) return;
    close(dev->sock_fd);
    free(dev);
}

int gbm_device_get_fd(struct gbm_device *dev) {
    return dev ? dev->sock_fd : -1;
}

const char *gbm_device_get_backend_name(struct gbm_device *dev) {
    (void)dev;
    return "proxy";
}

int gbm_device_is_format_supported(struct gbm_device *dev, uint32_t format,
                                   uint32_t usage) {
    (void)dev; (void)usage;
    return format == GBM_FORMAT_ABGR8888 || format == GBM_FORMAT_XBGR8888
        || format == GBM_FORMAT_ARGB8888 || format == GBM_FORMAT_XRGB8888;
}

int gbm_device_get_format_modifier_plane_count(struct gbm_device *dev,
                                               uint32_t format,
                                               uint64_t modifier) {
    (void)dev; (void)format; (void)modifier;
    return 1;
}

/* ── GBM Buffer Object API ─────────────────────────────────────────────── */

static struct gbm_bo *alloc_bo(struct gbm_device *dev, uint32_t width,
                                uint32_t height, uint32_t format,
                                uint32_t flags) {
    struct alloc_request req = {
        .msg_type = MSG_ALLOC,
        .width = width,
        .height = height,
        .format = format,
        .flags = flags,
    };

    if (send(dev->sock_fd, &req, sizeof(req), 0) != sizeof(req)) {
        fprintf(stderr, "[gbm-proxy] send failed: %s\n", strerror(errno));
        return NULL;
    }

    struct alloc_response resp;
    int dmabuf_fd = -1;
    int n = recv_with_fd(dev->sock_fd, &resp, sizeof(resp), &dmabuf_fd);
    if (n < (int)sizeof(resp) || !resp.success || dmabuf_fd < 0) {
        fprintf(stderr, "[gbm-proxy] alloc failed: n=%d success=%u fd=%d\n",
                n, resp.success, dmabuf_fd);
        if (dmabuf_fd >= 0) close(dmabuf_fd);
        return NULL;
    }

    struct gbm_bo *bo = calloc(1, sizeof(*bo));
    if (!bo) { close(dmabuf_fd); return NULL; }
    bo->dev = dev;
    bo->width = resp.width;
    bo->height = resp.height;
    bo->stride = resp.stride;
    bo->format = resp.format;
    bo->modifier = resp.modifier;
    bo->dmabuf_fd = dmabuf_fd;

    fprintf(stderr, "[gbm-proxy] Allocated %ux%u stride=%u fd=%d\n",
            bo->width, bo->height, bo->stride, bo->dmabuf_fd);
    return bo;
}

struct gbm_bo *gbm_bo_create(struct gbm_device *dev, uint32_t width,
                              uint32_t height, uint32_t format,
                              uint32_t flags) {
    if (!dev) return NULL;
    return alloc_bo(dev, width, height, format, flags);
}

struct gbm_bo *gbm_bo_create_with_modifiers(struct gbm_device *dev,
                                            uint32_t width, uint32_t height,
                                            uint32_t format,
                                            const uint64_t *modifiers,
                                            const unsigned int count) {
    (void)modifiers; (void)count;
    return gbm_bo_create(dev, width, height, format, GBM_BO_USE_RENDERING | GBM_BO_USE_SCANOUT);
}

struct gbm_bo *gbm_bo_create_with_modifiers2(struct gbm_device *dev,
                                             uint32_t width, uint32_t height,
                                             uint32_t format,
                                             const uint64_t *modifiers,
                                             const unsigned int count,
                                             uint32_t flags) {
    (void)modifiers; (void)count;
    return gbm_bo_create(dev, width, height, format, flags);
}

void gbm_bo_destroy(struct gbm_bo *bo) {
    if (!bo) return;
    if (bo->destroy_fn) bo->destroy_fn(bo, bo->user_data);
    if (bo->dmabuf_fd >= 0) close(bo->dmabuf_fd);
    free(bo);
}

uint32_t gbm_bo_get_width(struct gbm_bo *bo) { return bo ? bo->width : 0; }
uint32_t gbm_bo_get_height(struct gbm_bo *bo) { return bo ? bo->height : 0; }
uint32_t gbm_bo_get_stride(struct gbm_bo *bo) { return bo ? bo->stride : 0; }
uint32_t gbm_bo_get_stride_for_plane(struct gbm_bo *bo, int plane) {
    (void)plane;
    return bo ? bo->stride : 0;
}
uint32_t gbm_bo_get_format(struct gbm_bo *bo) { return bo ? bo->format : 0; }
uint32_t gbm_bo_get_bpp(struct gbm_bo *bo) { (void)bo; return 4; }
uint64_t gbm_bo_get_modifier(struct gbm_bo *bo) { return bo ? bo->modifier : 0; }
struct gbm_device *gbm_bo_get_device(struct gbm_bo *bo) { return bo ? bo->dev : NULL; }

union gbm_bo_handle gbm_bo_get_handle(struct gbm_bo *bo) {
    union gbm_bo_handle h = {0};
    if (bo) h.s32 = bo->dmabuf_fd;
    return h;
}

int gbm_bo_get_fd(struct gbm_bo *bo) {
    if (!bo || bo->dmabuf_fd < 0) return -1;
    return dup(bo->dmabuf_fd);
}

int gbm_bo_get_fd_for_plane(struct gbm_bo *bo, int plane) {
    (void)plane;
    return gbm_bo_get_fd(bo);
}

int gbm_bo_get_plane_count(struct gbm_bo *bo) { (void)bo; return 1; }

union gbm_bo_handle gbm_bo_get_handle_for_plane(struct gbm_bo *bo, int plane) {
    (void)plane;
    return gbm_bo_get_handle(bo);
}

uint32_t gbm_bo_get_offset(struct gbm_bo *bo, int plane) {
    (void)bo; (void)plane;
    return 0;
}

int gbm_bo_write(struct gbm_bo *bo, const void *buf, size_t count) {
    (void)bo; (void)buf; (void)count;
    return -1;
}

void gbm_bo_set_user_data(struct gbm_bo *bo, void *data,
                           void (*destroy)(struct gbm_bo *, void *)) {
    if (!bo) return;
    bo->user_data = data;
    bo->destroy_fn = destroy;
}

void *gbm_bo_get_user_data(struct gbm_bo *bo) {
    return bo ? bo->user_data : NULL;
}

/* ── Import (for Wayland dmabuf → GBM bo) ──────────────────────────────── */

struct gbm_import_fd_data {
    int fd;
    uint32_t width;
    uint32_t height;
    uint32_t stride;
    uint32_t format;
};

#define GBM_BO_IMPORT_FD          0x5503
#define GBM_BO_IMPORT_FD_MODIFIER 0x5505
#define GBM_MAX_PLANES 4

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

struct gbm_bo *gbm_bo_import(struct gbm_device *dev, uint32_t type,
                              void *buffer, uint32_t usage) {
    (void)usage;
    if (!dev || !buffer) return NULL;

    struct gbm_bo *bo = calloc(1, sizeof(*bo));
    if (!bo) return NULL;
    bo->dev = dev;

    if (type == GBM_BO_IMPORT_FD) {
        struct gbm_import_fd_data *data = buffer;
        bo->width = data->width;
        bo->height = data->height;
        bo->stride = data->stride;
        bo->format = data->format;
        bo->dmabuf_fd = dup(data->fd);
    } else if (type == GBM_BO_IMPORT_FD_MODIFIER) {
        struct gbm_import_fd_modifier_data *data = buffer;
        bo->width = data->width;
        bo->height = data->height;
        bo->format = data->format;
        bo->stride = (uint32_t)data->strides[0];
        bo->modifier = data->modifier;
        bo->dmabuf_fd = (data->num_fds > 0) ? dup(data->fds[0]) : -1;
    } else {
        free(bo);
        return NULL;
    }
    return bo;
}

/* ── Map/unmap stubs ───────────────────────────────────────────────────── */

void *gbm_bo_map(struct gbm_bo *bo, uint32_t x, uint32_t y, uint32_t width,
                  uint32_t height, uint32_t flags, uint32_t *stride,
                  void **map_data) {
    (void)bo; (void)x; (void)y; (void)width; (void)height;
    (void)flags; (void)stride; (void)map_data;
    return NULL;
}

void gbm_bo_unmap(struct gbm_bo *bo, void *map_data) {
    (void)bo; (void)map_data;
}

/* ── Surface stubs (Mesa doesn't use these for Vulkan WSI) ─────────────── */

struct gbm_surface;

struct gbm_surface *gbm_surface_create(struct gbm_device *dev, uint32_t width,
                                        uint32_t height, uint32_t format,
                                        uint32_t flags) {
    (void)dev; (void)width; (void)height; (void)format; (void)flags;
    return NULL;
}

struct gbm_surface *gbm_surface_create_with_modifiers(
    struct gbm_device *dev, uint32_t width, uint32_t height, uint32_t format,
    const uint64_t *modifiers, const unsigned int count) {
    (void)dev; (void)width; (void)height; (void)format; (void)modifiers; (void)count;
    return NULL;
}

struct gbm_bo *gbm_surface_lock_front_buffer(struct gbm_surface *surface) {
    (void)surface;
    return NULL;
}

void gbm_surface_release_buffer(struct gbm_surface *surface, struct gbm_bo *bo) {
    (void)surface; (void)bo;
}

int gbm_surface_has_free_buffers(struct gbm_surface *surface) {
    (void)surface;
    return 0;
}

void gbm_surface_destroy(struct gbm_surface *surface) { (void)surface; }

/* ── Minigbm extras ────────────────────────────────────────────────────── */

uint32_t gbm_bo_get_plane_size(struct gbm_bo *bo, size_t plane) {
    (void)plane;
    return bo ? bo->stride * bo->height : 0;
}

int gbm_bo_get_plane_fd(struct gbm_bo *bo, size_t plane) {
    (void)plane;
    return gbm_bo_get_fd(bo);
}
