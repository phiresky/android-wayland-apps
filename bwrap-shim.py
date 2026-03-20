#!/usr/bin/env python3
"""bwrap shim: runs the command unsandboxed (proot can't do namespaces).
Parses all bwrap options, performs filesystem ops, execs the command after --.
"""
import os
import sys

args = sys.argv[1:]

clear_env = False
chdir_path = None
cmd = []
env_set = {}
env_unset = []
next_perms = None
i = 0

# Two-arg options that are just skipped
TWO_ARG = {
    '--lock-file', '--sync-fd', '--info-fd', '--json-status-fd', '--block-fd',
    '--userns-block-fd', '--size', '--uid', '--gid', '--hostname',
    '--exec-label', '--file-label', '--cap-add', '--cap-drop',
    '--seccomp', '--userns', '--userns2', '--pidns',
}
# One-arg options that are just skipped
ONE_ARG = {
    '--dev', '--proc', '--mqueue',
}
# Zero-arg options that are just skipped
ZERO_ARG = {
    '--unshare-user', '--unshare-user-try', '--unshare-ipc', '--unshare-pid',
    '--unshare-net', '--unshare-uts', '--unshare-cgroup', '--unshare-cgroup-try',
    '--unshare-all', '--share-net', '--die-with-parent', '--new-session',
    '--as-pid-1', '--disable-userns', '--assert-userns-disabled',
}

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
        fd, path = int(args[i + 1]), args[i + 2]
        try:
            os.makedirs(os.path.dirname(path), exist_ok=True)
            with os.fdopen(os.dup(fd), 'r') as src, open(path, 'w') as dst:
                dst.write(src.read())
        except OSError:
            pass
        i += 3
    elif a in ('--bind-data', '--ro-bind-data') and i + 2 < len(args):
        fd, path = int(args[i + 1]), args[i + 2]
        try:
            os.makedirs(os.path.dirname(path), exist_ok=True)
            with os.fdopen(os.dup(fd), 'rb') as src, open(path, 'wb') as dst:
                dst.write(src.read())
        except OSError:
            pass
        i += 3
    elif a == '--perms' and i + 1 < len(args):
        next_perms = int(args[i + 1], 8); i += 2
    elif a == '--dir' and i + 1 < len(args):
        try:
            os.makedirs(args[i + 1], exist_ok=True)
            if next_perms is not None:
                os.chmod(args[i + 1], next_perms)
        except OSError:
            pass
        next_perms = None; i += 2
    elif a == '--tmpfs' and i + 1 < len(args):
        try:
            os.makedirs(args[i + 1], exist_ok=True)
            if next_perms is not None:
                os.chmod(args[i + 1], next_perms)
        except OSError:
            pass
        next_perms = None; i += 2
    elif a == '--symlink' and i + 2 < len(args):
        try:
            os.symlink(args[i + 1], args[i + 2])
        except OSError:
            pass
        i += 3
    elif a == '--chmod' and i + 2 < len(args):
        try:
            os.chmod(args[i + 2], int(args[i + 1], 8))
        except OSError:
            pass
        i += 3
    elif a in ('--ro-bind', '--bind', '--ro-bind-try', '--bind-try',
               '--dev-bind', '--dev-bind-try') and i + 2 < len(args):
        src, dst = args[i + 1], args[i + 2]
        # Can't do real bind mounts in proot — symlink if src != dest and dest doesn't exist
        if src != dst and not os.path.exists(dst):
            try:
                os.makedirs(os.path.dirname(dst), exist_ok=True)
                os.symlink(src, dst)
            except OSError:
                pass
        i += 3
    elif a == '--remount-ro-bind' and i + 2 < len(args):
        i += 3
    elif a in TWO_ARG and i + 1 < len(args):
        i += 2
    elif a in ONE_ARG and i + 1 < len(args):
        i += 2
    elif a in ZERO_ARG:
        i += 1
    else:
        i += 1

# Apply environment
if clear_env:
    env = {}
else:
    env = dict(os.environ)

for k, v in env_set.items():
    env[k] = v
for k in env_unset:
    env.pop(k, None)

if chdir_path:
    try:
        os.chdir(chdir_path)
    except OSError:
        pass

if cmd:
    os.execvpe(cmd[0], cmd, env)
else:
    sys.exit(1)
