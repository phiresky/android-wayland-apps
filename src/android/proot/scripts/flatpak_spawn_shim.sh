#!/bin/sh
# flatpak-spawn shim: runs the command unsandboxed (proot can't do namespaces).
# Strips all flatpak-spawn options and execs the trailing command.
dir=""
while [ $# -gt 0 ]; do
    case "$1" in
        --sandbox|--watch-bus|--latest-version|--no-network|--clear-env|--host|--verbose)
            shift ;;
        --directory=*)
            dir="${1#--directory=}"; shift ;;
        --forward-fd=*|--env=*)
            shift ;;
        -*)
            shift ;;
        *)
            break ;;
    esac
done
if [ -n "$dir" ]; then
    cd "$dir" 2>/dev/null || true
fi
exec "$@"
