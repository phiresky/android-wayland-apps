# Future Work: LD_PRELOAD + Seccomp Cookie Hybrid

## Problem

Proot uses ptrace to intercept ~80 syscalls for path translation. Each intercepted syscall costs ~29μs on x86_64 (likely 2-3x more on ARM64 mobile) due to ptrace context switches. With seccomp-BPF filtering, non-intercepted syscalls (read, write, mmap, futex, etc.) pass through at native speed, but path-related syscalls (openat, stat, access, etc.) still pay the full ptrace penalty.

## Proposal: Magic Cookie Approach

Instead of using proot or fakechroot as separate backends, use a **hybrid** where:

1. A custom **LD_PRELOAD library** intercepts glibc path functions, translates paths in userspace, and makes the raw syscall with a **magic cookie** in the unused 6th argument register (x5 on arm64).
2. A patched **seccomp-BPF filter** in proot checks for the cookie: if present → `SECCOMP_RET_ALLOW` (zero overhead), if absent → `SECCOMP_RET_TRACE` (fall through to ptrace for static binaries).

This gives **native syscall speed for dynamically linked programs** (99% of desktop Linux apps) with ptrace as a safety net for the rare static binary.

### Why It Works

On arm64, syscalls use registers x0-x5 (6 args). Every syscall proot intercepts uses **at most 5 arguments** — the 6th register is always free. The seccomp BPF filter can inspect `seccomp_data.args[5]`, so:

```
for each path syscall:
    if args[5] == MAGIC_COOKIE → ALLOW  (LD_PRELOAD already translated)
    else                       → TRACE  (need ptrace fallback)
```

The LD_PRELOAD shim sets `x5 = MAGIC_COOKIE` before every raw syscall:
```rust
// Instead of calling libc openat(), make the syscall directly with cookie
syscall(SYS_openat, dirfd, translated_path.as_ptr(), flags, mode, 0, MAGIC_COOKIE)
```

### Proot Seccomp Filter Patch

In `seccomp.c`, modify `add_trace_syscall()`:

```c
// Current: always TRACE
BPF_JUMP(BPF_JMP + BPF_JEQ + BPF_K, syscall, 0, 1),
BPF_STMT(BPF_RET + BPF_K, SECCOMP_RET_TRACE + flag)

// Patched: check cookie first
BPF_JUMP(BPF_JMP + BPF_JEQ + BPF_K, syscall, 0, 3),           // if syscall matches
BPF_STMT(BPF_LD + BPF_W + BPF_ABS, offsetof_args5),            // load args[5]
BPF_JUMP(BPF_JMP + BPF_JEQ + BPF_K, MAGIC_COOKIE, 0, 1),      // cookie present?
BPF_STMT(BPF_RET + BPF_K, SECCOMP_RET_ALLOW),                  // yes → native speed
BPF_STMT(BPF_RET + BPF_K, SECCOMP_RET_TRACE + flag)            // no → ptrace
```

## Benchmarks

### Ptrace overhead per syscall

Measured on x86_64 (ARM64 mobile estimated at 2-3x):

| | Native | Ptrace'd | Overhead |
|---|---|---|---|
| openat | 1,760 ns | 31,075 ns | ~29 μs/stop |

### Syscall reduction by workload

| Workload | Ptrace stops (current) | With cookie | Reduction | Time saved (x86) | Est. ARM64 |
|---|---|---|---|---|---|
| ls /usr/bin | 39 | 14 | 64% | 1 ms | ~2-3 ms |
| bash -c echo | 62 | 9 | 85% | 2 ms | ~4-6 ms |
| code --version | 875 | 77 | 91% | 23 ms | ~50-70 ms |
| find /usr *.so | 1,751 | 17 | 99% | 50 ms | ~100-150 ms |
| gcc hello.c | 1,397 | 106 | 92% | 37 ms | ~75-110 ms |
| pacman -Qi | 4,264 | 49 | 99% | 122 ms | ~250-360 ms |

These are small workloads. Real-world tasks (pacman -S, npm install, cargo build, VSCode with LSP + file watchers) do orders of magnitude more I/O and would see proportionally larger savings.

### What the remaining ptrace stops are

The ~1-9% of intercepted syscalls that still need ptrace are non-path syscalls:
- `brk` — heap management
- `prctl` — blocks PR_SET_DUMPABLE (prevents ptrace breakage)
- `prlimit64` — resource limit adjustments
- `ioctl` — Android termios fixups (TCSETS2 → TCSETS etc.)
- `wait4` — ptrace wait translation

Some of these could also be moved to the LD_PRELOAD shim to reduce ptrace stops further.

## Bionic/Glibc Boundary

Android uses bionic; the Arch rootfs uses glibc. The LD_PRELOAD .so must be glibc-linked. Launch via the rootfs's own dynamic linker:

```sh
$ARCH_FS_ROOT/lib/ld-linux-aarch64.so.1 \
  --library-path $ARCH_FS_ROOT/usr/lib \
  $ARCH_FS_ROOT/usr/sbin/chroot $ARCH_FS_ROOT \
  /usr/bin/env LD_PRELOAD=/usr/lib/libproot_preload.so ... sh -c "command"
```

The LD_PRELOAD shim intercepts the `chroot()` libc call in userspace — never invokes the real syscall, so no root needed.

## Implementation Plan

1. **Custom LD_PRELOAD library** (Rust, compiled for aarch64-linux-gnu):
   - ~50 function wrappers for path-taking libc functions
   - Path translation logic (rootfs prefix, bind mount mappings)
   - Raw syscall with magic cookie in arg6
   - Handles: openat, stat, access, readlink, execve, getcwd, bind, connect, etc.

2. **Proot seccomp filter patch** (`seccomp.c`):
   - Modify BPF generation to check arg6 for cookie before TRACE
   - ~20 lines of code change

3. **Integration in ArchProcess** (`src/android/proot/process.rs`):
   - Add LD_PRELOAD to env when launching GUI apps
   - Keep proot as the process supervisor (for ptrace fallback + process lifecycle)

## Limitations

- **Direct syscalls missed by LD_PRELOAD**: programs using raw `syscall()` (Go binaries, io_uring) bypass the shim. These fall through to ptrace — slower but correct.
- **Statically linked binaries**: LD_PRELOAD doesn't apply. Falls through to ptrace.
- **Fake /proc files**: proot bind-mounts fake /proc/stat etc. The LD_PRELOAD shim would need to intercept reads to those paths, or skip them (GUI apps don't need them).
- **execve boundary**: when the shimmed process exec's a new binary, LD_PRELOAD is inherited for dynamically linked targets. Static targets fall through to ptrace.

## Alternatives Considered

- **User namespaces**: `CONFIG_USER_NS` disabled in Samsung kernel
- **AVF/pKVM VM**: device supports it, but breaks Wayland buffer sharing and per-window Android Activity integration
- **eBPF path rewriting**: `bpf()` syscall denied by SELinux for app processes; also eBPF can't rewrite path strings in userspace memory
- **Root + chroot**: requires unlocked bootloader / Knox trip
- **Pure fakechroot (no ptrace fallback)**: breaks static binaries with no safety net
- **Removing `--root-id` selectively**: minimal gain since overhead is path syscalls, not uid faking
