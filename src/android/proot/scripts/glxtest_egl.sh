#!/bin/sh
# EGL-based GPU probe replacement for Firefox's glxtest (which fails in proot).
# Firefox opens fd 3 as a pipe before launching this. Write GPU info there.
info=$(eglinfo -B 2>/dev/null)
vendor=$(echo "$info" | grep "OpenGL core profile vendor:" | head -1 | sed 's/.*: //')
renderer=$(echo "$info" | grep "OpenGL core profile renderer:" | head -1 | sed 's/.*: //')
version=$(echo "$info" | grep "OpenGL core profile version:" | head -1 | sed 's/.*: //')
if [ -n "$renderer" ]; then
    printf "VENDOR\n%s\nRENDERER\n%s\nVERSION\n%s\nTFP\nEGL\n" "$vendor" "$renderer" "$version" >&3
fi
