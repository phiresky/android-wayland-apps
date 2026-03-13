#!/bin/bash
# Take a screenshot from the device.
# The -d flag is required on multi-display devices (e.g. Samsung Fold) because
# without it, screencap prints a warning to stdout before the PNG data, which
# corrupts the file (it's no longer valid PNG, just "data").
DISPLAY_ID="${1:-4630947200649055635}"
mkdir -p screenshot
FILENAME="screenshot/$(date '+%Y-%m-%d_%H-%M-%S').png"
adb exec-out screencap -p -d "$DISPLAY_ID" > "$FILENAME"
echo "Saved $FILENAME"
