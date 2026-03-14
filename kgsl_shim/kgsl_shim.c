/*
 * kgsl_shim.so - LD_PRELOAD shim for Samsung SM8750 KGSL compatibility
 *
 * Samsung's Snapdragon 8 Elite (SM8750) kernel omits IOCTL_KGSL_VERSION
 * (ioctl nr=0x03). Mesa Turnip calls this as its first initialization
 * step and returns VK_ERROR_INITIALIZATION_FAILED when it gets ENOTTY.
 *
 * This shim intercepts ioctl() and fakes a successful VERSION response
 * so Turnip can proceed to enumerate the physical device. All other
 * ioctls are passed through unchanged.
 *
 * Build (inside proot Arch):
 *   clang -O2 -shared -fPIC -fuse-ld=lld -o kgsl_shim.so kgsl_shim.c -ldl
 *
 * Use:
 *   LD_PRELOAD=/path/to/kgsl_shim.so vulkaninfo
 */
#define _GNU_SOURCE
#include <dlfcn.h>
#include <sys/ioctl.h>
#include <stdint.h>
#include <stdarg.h>
#include <stddef.h>
#include <stdio.h>
#include <unistd.h>
#include <string.h>
#include <sys/types.h>
#include <fcntl.h>

/* IOCTL_KGSL_VERSION = _IOWR(0x09, 0x03, struct kgsl_version{u32,u32}) */
#define IOCTL_KGSL_VERSION ((3u<<30)|(8u<<16)|(0x09u<<8)|0x03u)

struct kgsl_version {
    uint32_t kern_ver;
    uint32_t user_ver;
};

__attribute__((constructor))
static void shim_init(void) {
    fprintf(stderr, "[kgsl_shim] loaded into process (PID %d)\n", getpid());
}

int open(const char *pathname, int flags, ...) {
    static int (*real_open)(const char *, int, ...) = NULL;
    if (!real_open)
        real_open = dlsym(RTLD_NEXT, "open");

    mode_t mode = 0;
    if (flags & O_CREAT) {
        va_list ap;
        va_start(ap, flags);
        mode = va_arg(ap, int);
        va_end(ap);
    }

    if (pathname && strstr(pathname, "kgsl"))
        fprintf(stderr, "[kgsl_shim] open(\"%s\", 0x%x)\n", pathname, flags);

    return real_open(pathname, flags, mode);
}

int open64(const char *pathname, int flags, ...) {
    static int (*real_open64)(const char *, int, ...) = NULL;
    if (!real_open64)
        real_open64 = dlsym(RTLD_NEXT, "open64");

    mode_t mode = 0;
    if (flags & O_CREAT) {
        va_list ap;
        va_start(ap, flags);
        mode = va_arg(ap, int);
        va_end(ap);
    }

    if (pathname && strstr(pathname, "kgsl"))
        fprintf(stderr, "[kgsl_shim] open64(\"%s\", 0x%x)\n", pathname, flags);

    return real_open64(pathname, flags, mode);
}

int ioctl(int fd, unsigned long req, ...) {
    static int (*real_ioctl)(int, unsigned long, ...) = NULL;
    if (!real_ioctl)
        real_ioctl = dlsym(RTLD_NEXT, "ioctl");

    va_list ap;
    va_start(ap, req);
    void *arg = va_arg(ap, void *);
    va_end(ap);

    /* log all ioctls to kgsl fd so we can see what Turnip calls */
    unsigned type = (req >> 8) & 0xff;
    if (type == 0x09)
        fprintf(stderr, "[kgsl_shim] ioctl fd=%d req=0x%08lx nr=0x%02lx\n",
                fd, req, req & 0xff);

    if (req == IOCTL_KGSL_VERSION) {
        struct kgsl_version *v = (struct kgsl_version *)arg;
        if (v) { v->kern_ver = 3; v->user_ver = 3; }
        fprintf(stderr, "[kgsl_shim] faked VERSION -> kern=3 user=3\n");
        return 0;
    }

    return real_ioctl(fd, req, arg);
}
