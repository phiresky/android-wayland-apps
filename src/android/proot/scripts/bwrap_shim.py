#!/usr/bin/env python3
"""bwrap shim: uses nested proot for bind mounts instead of namespaces.
Handles --args FD (NUL-separated args from file descriptor) used by flatpak.
Requires _PROOT_BIN, _PROOT_LOADER, _PROOT_TMP_DIR env vars."""
import sys, os

def read_fd(fd):
    data = b''
    while True:
        chunk = os.read(fd, 4096)
        if not chunk: break
        data += chunk
    os.close(fd)
    return data

args = list(sys.argv[1:])
# Expand --args FD
i = 0
while i < len(args):
    if args[i] == '--args' and i + 1 < len(args):
        fd = int(args[i + 1])
        rest = args[i + 2:]
        decoded = read_fd(fd).decode('utf-8', errors='replace')
        extra = decoded.split('\0')
        if extra and extra[-1] == '':
            extra.pop()
        args = extra + rest
        i = 0
        continue
    i += 1

clear_env = False
chdir_path = None
cmd = []
binds = []
env_set = {}
env_unset = []
i = 0

ONE_ARG = {'--unshare-all', '--unshare-user', '--unshare-user-try', '--unshare-ipc',
    '--unshare-pid', '--unshare-net', '--unshare-uts', '--unshare-cgroup',
    '--unshare-cgroup-try', '--share-net', '--die-with-parent', '--new-session',
    '--as-pid-1', '--disable-userns', '--assert-userns-disabled'}
TWO_ARG = {'--lock-file', '--sync-fd', '--info-fd', '--json-status-fd', '--block-fd',
    '--userns-block-fd', '--size', '--perms', '--uid', '--gid', '--hostname',
    '--exec-label', '--file-label', '--cap-add', '--cap-drop',
    '--seccomp', '--userns', '--userns2', '--pidns'}

while i < len(args):
    a = args[i]
    if a == '--':
        cmd = args[i + 1:]
        break
    elif a == '--setenv' and i + 2 < len(args):
        env_set[args[i + 1]] = args[i + 2]; i += 3
    elif a == '--unsetenv' and i + 1 < len(args):
        env_unset.append(args[i + 1]); i += 2
    elif a == '--chdir' and i + 1 < len(args):
        chdir_path = args[i + 1]; i += 2
    elif a == '--clearenv':
        clear_env = True; i += 1
    elif a == '--file' and i + 2 < len(args):
        dest = args[i + 2]
        try:
            os.makedirs(os.path.dirname(dest) or '.', exist_ok=True)
            with open(dest, 'wb') as f: f.write(read_fd(int(args[i + 1])))
        except OSError: pass
        i += 3
    elif a in ('--bind-data', '--ro-bind-data') and i + 2 < len(args):
        dest = args[i + 2]
        try:
            os.makedirs(os.path.dirname(dest) or '.', exist_ok=True)
            with open(dest, 'wb') as f: f.write(read_fd(int(args[i + 1])))
        except OSError: pass
        i += 3
    elif a in ('--ro-bind', '--bind', '--ro-bind-try', '--bind-try',
               '--dev-bind', '--dev-bind-try') and i + 2 < len(args):
        src, dest = args[i + 1], args[i + 2]
        if src != dest:
            binds.append((src, dest))
        i += 3
    elif a == '--remount-ro-bind' and i + 2 < len(args):
        i += 3
    elif a == '--remount-ro' and i + 1 < len(args):
        i += 2
    elif a == '--dir' and i + 1 < len(args):
        try: os.makedirs(args[i + 1], exist_ok=True)
        except OSError: pass
        i += 2
    elif a == '--tmpfs' and i + 1 < len(args):
        try: os.makedirs(args[i + 1], exist_ok=True)
        except OSError: pass
        i += 2
    elif a == '--symlink' and i + 2 < len(args):
        try:
            os.makedirs(os.path.dirname(args[i + 2]) or '.', exist_ok=True)
            if os.path.lexists(args[i + 2]): os.unlink(args[i + 2])
            os.symlink(args[i + 1], args[i + 2])
        except OSError: pass
        i += 3
    elif a == '--chmod' and i + 2 < len(args):
        try: os.chmod(args[i + 2], int(args[i + 1], 8))
        except OSError: pass
        i += 3
    elif a in ('--dev', '--proc', '--mqueue') and i + 1 < len(args):
        i += 2
    elif a in ONE_ARG: i += 1
    elif a in TWO_ARG and i + 1 < len(args): i += 2
    elif not a.startswith('--'):
        cmd = args[i:]
        break
    else: i += 1

if not cmd:
    sys.exit(0)

# Build environment
if clear_env:
    env = {}
else:
    env = dict(os.environ)
for k, v in env_set.items():
    env[k] = v
for k in env_unset:
    env.pop(k, None)

internal_keys = ('_PROOT_BIN', '_PROOT_LOADER', '_PROOT_TMP_DIR')

# Find proot binary: prefer env var, fall back to scanning /proc
proot_bin = os.environ.get('_PROOT_BIN', '')
proot_loader = os.environ.get('_PROOT_LOADER', '')
proot_tmp = os.environ.get('_PROOT_TMP_DIR', '/tmp')
if not proot_bin:
    try:
        for entry in os.listdir('/proc'):
            if entry.isdigit():
                try:
                    exe = os.readlink(f'/proc/{entry}/exe')
                    if exe.endswith('/libproot.so'):
                        proot_bin = exe
                        proot_loader = exe.replace('libproot.so', 'libproot_loader.so')
                        break
                except OSError:
                    continue
    except OSError:
        pass

# Use nested proot for bind mounts when available
if binds and proot_bin and os.path.isfile(proot_bin):
    proot_args = [proot_bin, '-r', '/', '-L', '--link2symlink']
    for src, dest in binds:
        proot_args.append(f'--bind={src}:{dest}')
    if chdir_path:
        proot_args.extend(['-w', chdir_path])
    proot_args.extend(cmd)
    env['PROOT_LOADER'] = proot_loader
    env['PROOT_TMP_DIR'] = proot_tmp
    for k in internal_keys: env.pop(k, None)
    os.execvpe(proot_args[0], proot_args, env)
else:
    if chdir_path:
        try: os.chdir(chdir_path)
        except OSError: pass
    for k in internal_keys: env.pop(k, None)
    os.execvpe(cmd[0], cmd, env)
