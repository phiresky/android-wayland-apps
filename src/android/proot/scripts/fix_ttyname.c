#define _GNU_SOURCE
#include <unistd.h>
#include <stdio.h>
#include <string.h>
#include <errno.h>
#include <sys/stat.h>

/* Override ttyname_r to use /proc/self/fd instead of scanning /dev/pts/.
 * On Android, SELinux blocks readdir on /dev/pts for untrusted_app,
 * causing ttyname_r to fail with EACCES. */
int ttyname_r(int fd, char *buf, size_t buflen) {
    struct stat st;
    if (fstat(fd, &st) != 0 || !S_ISCHR(st.st_mode)) {
        errno = ENOTTY;
        return ENOTTY;
    }
    char proc_path[64];
    snprintf(proc_path, sizeof(proc_path), "/proc/self/fd/%d", fd);
    ssize_t len = readlink(proc_path, buf, buflen - 1);
    if (len == -1) return errno;
    buf[len] = '\0';
    return 0;
}

char *ttyname(int fd) {
    static char buf[256];
    if (ttyname_r(fd, buf, sizeof(buf)) != 0) return NULL;
    return buf;
}
