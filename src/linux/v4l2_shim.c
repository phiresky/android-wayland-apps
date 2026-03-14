/*
 * v4l2_shim.c — LD_PRELOAD shim presenting Android camera as /dev/video0
 *
 * Intercepts open/ioctl/mmap/fstat/close to implement the V4L2 MMAP streaming
 * API over a Unix socket connection to the Rust camera server in the host app.
 *
 * Frame protocol (from camera server):
 *   [u32 LE width][u32 LE height][u32 LE data_len][NV12 frame bytes]
 *
 * Build (inside proot):
 *   gcc -shared -fPIC -O2 -o /usr/lib/libandroid_cam.so /tmp/v4l2_shim.c -lpthread -ldl
 */
#define _GNU_SOURCE
#include <dlfcn.h>
#include <errno.h>
#include <fcntl.h>
#include <linux/videodev2.h>
#include <pthread.h>
#include <stdarg.h>
#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/ioctl.h>
#include <sys/mman.h>
#include <sys/socket.h>
#include <sys/stat.h>
#include <sys/sysmacros.h>
#include <sys/un.h>
#include <time.h>
#include <unistd.h>

#define CAM_SOCK    "/tmp/android_cam.sock"
#define CAM_WIDTH   640
#define CAM_HEIGHT  480
#define CAM_PIXFMT  V4L2_PIX_FMT_NV12
#define FRAME_SIZE  ((CAM_WIDTH) * (CAM_HEIGHT) * 3 / 2)
#define NUM_BUFS    4

typedef enum { BUF_FREE, BUF_QUEUED, BUF_FILLED, BUF_DEQUEUED } buf_state_t;

struct cam_fd {
    int          app_fd;               /* socketpair end given to app */
    int          sig_fd;               /* our end: we write here to signal */
    int          buf_count;
    uint8_t     *bufs[NUM_BUFS];
    buf_state_t  buf_state[NUM_BUFS];
    int          next_dequeue;
    uint32_t     buf_seq;
    volatile int streaming;
    volatile int stop;
    pthread_t    reader;
    pthread_mutex_t mu;
};

static struct cam_fd   *g_cam      = NULL;
static pthread_mutex_t  g_cam_lock = PTHREAD_MUTEX_INITIALIZER;

/* ---- real libc functions ---- */
static int   (*real_open)  (const char *, int, ...) = NULL;
static int   (*real_openat)(int, const char *, int, ...) = NULL;
static int   (*real_ioctl) (int, unsigned long, ...) = NULL;
static int   (*real_close) (int) = NULL;
static void *(*real_mmap)  (void *, size_t, int, int, int, off_t) = NULL;
static int   (*real_munmap)(void *, size_t) = NULL;
static int   (*real_fstat) (int, struct stat *) = NULL;
static int   (*real_stat)  (const char *, struct stat *) = NULL;

static void init_real(void) {
    static int done = 0;
    if (done) return;
    done = 1;
    real_open   = dlsym(RTLD_NEXT, "open");
    real_openat = dlsym(RTLD_NEXT, "openat");
    real_ioctl  = dlsym(RTLD_NEXT, "ioctl");
    real_close  = dlsym(RTLD_NEXT, "close");
    real_mmap   = dlsym(RTLD_NEXT, "mmap");
    real_munmap = dlsym(RTLD_NEXT, "munmap");
    real_fstat  = dlsym(RTLD_NEXT, "fstat");
    real_stat   = dlsym(RTLD_NEXT, "stat");
}

/* ---- helper: find cam if fd matches ---- */
static struct cam_fd *get_cam(int fd) {
    pthread_mutex_lock(&g_cam_lock);
    struct cam_fd *c = (g_cam && g_cam->app_fd == fd) ? g_cam : NULL;
    pthread_mutex_unlock(&g_cam_lock);
    return c;
}

/* ---- reader thread: connects to socket, fills buffers ---- */
static void *reader_thread(void *arg) {
    struct cam_fd *cam = (struct cam_fd *)arg;
    int sock = -1;

    while (!cam->stop) {
        /* connect (retry on failure) */
        if (sock < 0) {
            sock = socket(AF_UNIX, SOCK_STREAM, 0);
            if (sock < 0) { sleep(1); continue; }
            struct sockaddr_un addr;
            memset(&addr, 0, sizeof(addr));
            addr.sun_family = AF_UNIX;
            strncpy(addr.sun_path, CAM_SOCK, sizeof(addr.sun_path) - 1);
            if (connect(sock, (struct sockaddr *)&addr, sizeof(addr)) < 0) {
                close(sock); sock = -1;
                if (!cam->stop) sleep(1);
                continue;
            }
        }

        /* read 12-byte header */
        uint32_t hdr[3];
        if (recv(sock, hdr, 12, MSG_WAITALL) != 12) {
            close(sock); sock = -1; continue;
        }
        uint32_t data_len = hdr[2];
        if (data_len == 0 || data_len > (uint32_t)FRAME_SIZE * 4) {
            close(sock); sock = -1; continue;
        }

        /* find a queued buffer */
        pthread_mutex_lock(&cam->mu);
        int idx = -1;
        for (int i = 0; i < cam->buf_count; i++) {
            if (cam->buf_state[i] == BUF_QUEUED) {
                cam->buf_state[i] = BUF_FILLED; /* claim atomically under lock */
                idx = i;
                break;
            }
        }
        pthread_mutex_unlock(&cam->mu);

        uint32_t to_read = (data_len < (uint32_t)FRAME_SIZE) ? data_len : (uint32_t)FRAME_SIZE;

        if (idx < 0) {
            /* no buffer available — discard frame */
            uint8_t tmp[4096];
            uint32_t rem = data_len;
            while (rem > 0 && !cam->stop) {
                uint32_t n = rem < sizeof(tmp) ? rem : (uint32_t)sizeof(tmp);
                ssize_t r = recv(sock, tmp, n, MSG_WAITALL);
                if (r <= 0) { close(sock); sock = -1; break; }
                rem -= (uint32_t)r;
            }
            continue;
        }

        /* read frame data into buffer */
        if (recv(sock, cam->bufs[idx], to_read, MSG_WAITALL) != (ssize_t)to_read) {
            close(sock); sock = -1;
            pthread_mutex_lock(&cam->mu);
            cam->buf_state[idx] = BUF_QUEUED;
            pthread_mutex_unlock(&cam->mu);
            continue;
        }

        /* discard any excess bytes */
        uint32_t rem = data_len - to_read;
        while (rem > 0) {
            uint8_t tmp[4096];
            uint32_t n = rem < sizeof(tmp) ? rem : (uint32_t)sizeof(tmp);
            ssize_t r = recv(sock, tmp, n, MSG_WAITALL);
            if (r <= 0) { close(sock); sock = -1; break; }
            rem -= (uint32_t)r;
        }
        if (sock < 0) continue;

        /* signal app: write one byte to sig_fd → app_fd becomes readable */
        char sig = 1;
        write(cam->sig_fd, &sig, 1);
    }

    if (sock >= 0) close(sock);
    return NULL;
}

/* ---- V4L2 ioctl handler ---- */
static int cam_ioctl(struct cam_fd *cam, unsigned long req, void *arg) {
    switch (req) {

    case VIDIOC_QUERYCAP: {
        struct v4l2_capability *cap = arg;
        memset(cap, 0, sizeof(*cap));
        strncpy((char *)cap->driver,   "android_cam",         sizeof(cap->driver) - 1);
        strncpy((char *)cap->card,     "Android Camera",      sizeof(cap->card) - 1);
        strncpy((char *)cap->bus_info, "platform:android_cam",sizeof(cap->bus_info) - 1);
        cap->version      = (5 << 16) | (15 << 8); /* 5.15.0 */
        cap->capabilities = V4L2_CAP_VIDEO_CAPTURE | V4L2_CAP_STREAMING | V4L2_CAP_DEVICE_CAPS;
        cap->device_caps  = V4L2_CAP_VIDEO_CAPTURE | V4L2_CAP_STREAMING;
        return 0;
    }

    case VIDIOC_ENUM_FMT: {
        struct v4l2_fmtdesc *fmt = arg;
        if (fmt->index != 0 || fmt->type != V4L2_BUF_TYPE_VIDEO_CAPTURE) {
            errno = EINVAL; return -1;
        }
        fmt->flags = 0;
        strncpy((char *)fmt->description, "NV12", sizeof(fmt->description) - 1);
        fmt->pixelformat = CAM_PIXFMT;
        memset(fmt->reserved, 0, sizeof(fmt->reserved));
        return 0;
    }

    case VIDIOC_G_FMT:
    case VIDIOC_S_FMT:
    case VIDIOC_TRY_FMT: {
        struct v4l2_format *f = arg;
        if (f->type != V4L2_BUF_TYPE_VIDEO_CAPTURE) { errno = EINVAL; return -1; }
        f->fmt.pix.width        = CAM_WIDTH;
        f->fmt.pix.height       = CAM_HEIGHT;
        f->fmt.pix.pixelformat  = CAM_PIXFMT;
        f->fmt.pix.field        = V4L2_FIELD_NONE;
        f->fmt.pix.bytesperline = CAM_WIDTH;   /* Y plane stride */
        f->fmt.pix.sizeimage    = FRAME_SIZE;
        f->fmt.pix.colorspace   = V4L2_COLORSPACE_SRGB;
        return 0;
    }

    case VIDIOC_REQBUFS: {
        struct v4l2_requestbuffers *rb = arg;
        if (rb->type != V4L2_BUF_TYPE_VIDEO_CAPTURE || rb->memory != V4L2_MEMORY_MMAP) {
            errno = EINVAL; return -1;
        }
        int n = rb->count > NUM_BUFS ? NUM_BUFS : (int)rb->count;
        pthread_mutex_lock(&cam->mu);
        cam->buf_count = n;
        for (int i = 0; i < n; i++) {
            if (!cam->bufs[i]) {
                cam->bufs[i] = malloc(FRAME_SIZE);
                if (cam->bufs[i]) memset(cam->bufs[i], 0, FRAME_SIZE);
                else { rb->count = (uint32_t)i; n = i; break; }
            }
            cam->buf_state[i] = BUF_FREE;
        }
        pthread_mutex_unlock(&cam->mu);
        rb->count = (uint32_t)n;
        rb->capabilities = 0;
        return 0;
    }

    case VIDIOC_QUERYBUF: {
        struct v4l2_buffer *buf = arg;
        if (buf->type != V4L2_BUF_TYPE_VIDEO_CAPTURE || (int)buf->index >= cam->buf_count) {
            errno = EINVAL; return -1;
        }
        buf->memory   = V4L2_MEMORY_MMAP;
        buf->length   = FRAME_SIZE;
        buf->m.offset = buf->index * 4096; /* unique mmap offset per buffer */
        buf->flags    = V4L2_BUF_FLAG_MAPPED;
        buf->field    = V4L2_FIELD_NONE;
        return 0;
    }

    case VIDIOC_QBUF: {
        struct v4l2_buffer *buf = arg;
        if (buf->type != V4L2_BUF_TYPE_VIDEO_CAPTURE || (int)buf->index >= cam->buf_count) {
            errno = EINVAL; return -1;
        }
        pthread_mutex_lock(&cam->mu);
        cam->buf_state[buf->index] = BUF_QUEUED;
        pthread_mutex_unlock(&cam->mu);
        return 0;
    }

    case VIDIOC_DQBUF: {
        struct v4l2_buffer *buf = arg;
        if (!cam->streaming) { errno = EINVAL; return -1; }

        /* block until a frame signal arrives (written by reader thread to sig_fd) */
        char sig;
        ssize_t r;
        do { r = recv(cam->app_fd, &sig, 1, 0); } while (r < 0 && errno == EINTR);
        if (r <= 0) { errno = EIO; return -1; }

        pthread_mutex_lock(&cam->mu);
        int idx = -1;
        for (int i = 0; i < cam->buf_count; i++) {
            int j = (cam->next_dequeue + i) % cam->buf_count;
            if (cam->buf_state[j] == BUF_FILLED) {
                idx = j;
                cam->buf_state[j] = BUF_DEQUEUED;
                cam->next_dequeue = (j + 1) % cam->buf_count;
                break;
            }
        }
        pthread_mutex_unlock(&cam->mu);

        if (idx < 0) { errno = EIO; return -1; }

        buf->index     = (uint32_t)idx;
        buf->bytesused = FRAME_SIZE;
        buf->flags     = V4L2_BUF_FLAG_MAPPED | V4L2_BUF_FLAG_DONE;
        buf->field     = V4L2_FIELD_NONE;
        buf->memory    = V4L2_MEMORY_MMAP;
        buf->length    = FRAME_SIZE;
        buf->m.offset  = (uint32_t)idx * 4096;
        buf->sequence  = cam->buf_seq++;
        struct timespec ts;
        clock_gettime(CLOCK_MONOTONIC, &ts);
        buf->timestamp.tv_sec  = ts.tv_sec;
        buf->timestamp.tv_usec = ts.tv_nsec / 1000;
        return 0;
    }

    case VIDIOC_STREAMON: {
        if (cam->streaming) return 0;
        cam->stop      = 0;
        cam->streaming = 1;
        pthread_create(&cam->reader, NULL, reader_thread, cam);
        return 0;
    }

    case VIDIOC_STREAMOFF: {
        cam->streaming = 0;
        cam->stop      = 1;
        /* unblock reader thread via sig_fd shutdown */
        shutdown(cam->sig_fd, SHUT_RDWR);
        return 0;
    }

    case VIDIOC_ENUM_FRAMESIZES: {
        struct v4l2_frmsizeenum *fs = arg;
        if (fs->index != 0 || fs->pixel_format != CAM_PIXFMT) { errno = EINVAL; return -1; }
        fs->type             = V4L2_FRMSIZE_TYPE_DISCRETE;
        fs->discrete.width   = CAM_WIDTH;
        fs->discrete.height  = CAM_HEIGHT;
        return 0;
    }

    case VIDIOC_G_PARM: {
        struct v4l2_streamparm *p = arg;
        if (p->type != V4L2_BUF_TYPE_VIDEO_CAPTURE) { errno = EINVAL; return -1; }
        memset(&p->parm.capture, 0, sizeof(p->parm.capture));
        p->parm.capture.capability               = V4L2_CAP_TIMEPERFRAME;
        p->parm.capture.timeperframe.numerator   = 1;
        p->parm.capture.timeperframe.denominator = 30;
        return 0;
    }

    case VIDIOC_S_PARM:
        return 0; /* accept any framerate, we always deliver at camera rate */

    default:
        errno = ENOTTY;
        return -1;
    }
}

/* ---- fake stat for /dev/video* ---- */
static void fill_video_stat(struct stat *st) {
    memset(st, 0, sizeof(*st));
    st->st_mode = S_IFCHR | 0666;
    st->st_rdev = makedev(81, 0);
}

/* ---- intercepted functions ---- */

int open(const char *path, int flags, ...) {
    init_real();
    mode_t mode = 0;
    if (flags & O_CREAT) {
        va_list ap; va_start(ap, flags);
        mode = va_arg(ap, mode_t);
        va_end(ap);
    }
    if (strncmp(path, "/dev/video", 10) == 0) {
        pthread_mutex_lock(&g_cam_lock);
        if (g_cam) { pthread_mutex_unlock(&g_cam_lock); errno = EBUSY; return -1; }
        int fds[2];
        if (socketpair(AF_UNIX, SOCK_STREAM, 0, fds) < 0) {
            pthread_mutex_unlock(&g_cam_lock); return -1;
        }
        struct cam_fd *cam = calloc(1, sizeof(*cam));
        if (!cam) {
            close(fds[0]); close(fds[1]);
            pthread_mutex_unlock(&g_cam_lock); errno = ENOMEM; return -1;
        }
        cam->app_fd = fds[0];
        cam->sig_fd = fds[1];
        pthread_mutex_init(&cam->mu, NULL);
        g_cam = cam;
        pthread_mutex_unlock(&g_cam_lock);
        return fds[0];
    }
    return real_open ? real_open(path, flags, mode) : (errno = ENOSYS, -1);
}

int openat(int dirfd, const char *path, int flags, ...) {
    init_real();
    mode_t mode = 0;
    if (flags & O_CREAT) {
        va_list ap; va_start(ap, flags);
        mode = va_arg(ap, mode_t);
        va_end(ap);
    }
    if (strncmp(path, "/dev/video", 10) == 0)
        return open(path, flags, mode);
    return real_openat ? real_openat(dirfd, path, flags, mode) : (errno = ENOSYS, -1);
}

int ioctl(int fd, unsigned long req, ...) {
    init_real();
    va_list ap; va_start(ap, req);
    void *arg = va_arg(ap, void *);
    va_end(ap);
    struct cam_fd *cam = get_cam(fd);
    if (cam) return cam_ioctl(cam, req, arg);
    return real_ioctl ? real_ioctl(fd, req, arg) : (errno = ENOSYS, -1);
}

int close(int fd) {
    init_real();
    pthread_mutex_lock(&g_cam_lock);
    struct cam_fd *cam = (g_cam && g_cam->app_fd == fd) ? g_cam : NULL;
    if (cam) g_cam = NULL;
    pthread_mutex_unlock(&g_cam_lock);
    if (cam) {
        cam->stop = 1;
        real_close(cam->sig_fd);
        for (int i = 0; i < NUM_BUFS; i++) free(cam->bufs[i]);
        pthread_mutex_destroy(&cam->mu);
        free(cam);
    }
    return real_close ? real_close(fd) : (errno = ENOSYS, -1);
}

void *mmap(void *addr, size_t length, int prot, int flags, int fd, off_t offset) {
    init_real();
    struct cam_fd *cam = get_cam(fd);
    if (cam) {
        int idx = (int)((unsigned long)offset / 4096);
        if (idx >= 0 && idx < cam->buf_count && cam->bufs[idx])
            return cam->bufs[idx];
        errno = EINVAL; return MAP_FAILED;
    }
    return real_mmap ? real_mmap(addr, length, prot, flags, fd, offset) : MAP_FAILED;
}

int munmap(void *addr, size_t length) {
    init_real();
    pthread_mutex_lock(&g_cam_lock);
    struct cam_fd *cam = g_cam;
    pthread_mutex_unlock(&g_cam_lock);
    if (cam) {
        for (int i = 0; i < cam->buf_count; i++)
            if (cam->bufs[i] == addr) return 0; /* don't free our malloc'd buf */
    }
    return real_munmap ? real_munmap(addr, length) : (errno = ENOSYS, -1);
}

int fstat(int fd, struct stat *st) {
    init_real();
    struct cam_fd *cam = get_cam(fd);
    if (cam) { fill_video_stat(st); return 0; }
    return real_fstat ? real_fstat(fd, st) : (errno = ENOSYS, -1);
}

/* glibc compat wrappers */
int __fxstat(int ver, int fd, struct stat *st)   { (void)ver; return fstat(fd, st); }
int __fxstat64(int ver, int fd, struct stat *st) { (void)ver; return fstat(fd, st); }

int stat(const char *path, struct stat *st) {
    init_real();
    if (strncmp(path, "/dev/video", 10) == 0) { fill_video_stat(st); return 0; }
    return real_stat ? real_stat(path, st) : (errno = ENOSYS, -1);
}

int __xstat(int ver, const char *path, struct stat *st)   { (void)ver; return stat(path, st); }
int __xstat64(int ver, const char *path, struct stat *st) { (void)ver; return stat(path, st); }
