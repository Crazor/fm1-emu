#!/bin/sh
# Build the BlackBox CoreMIDI driver: ./build.sh [install | uninstall]
# install copies it to ~/Library/Audio/MIDI Drivers and restarts MIDIServer, which loads it.
set -e
here=$(cd "$(dirname "$0")" && pwd)
out=${BLACKBOX_MIDI_OUT:-$here/build}
bundle="$out/BlackBoxMIDI.plugin"
drivers="$HOME/Library/Audio/MIDI Drivers"

if [ "$1" = uninstall ]; then
    rm -rf "$drivers/BlackBoxMIDI.plugin"
    killall MIDIServer 2>/dev/null || true
    echo "removed $drivers/BlackBoxMIDI.plugin"
    exit 0
fi

rm -rf "$bundle"
mkdir -p "$bundle/Contents/MacOS"
cp "$here/Info.plist" "$bundle/Contents/Info.plist"
clang -O2 -Wall -bundle -arch arm64 -arch x86_64 -mmacosx-version-min=11.0 -fvisibility=hidden \
    -framework CoreMIDI -framework CoreFoundation \
    -o "$bundle/Contents/MacOS/BlackBoxMIDI" "$here/BlackBoxMIDI.c"
codesign --force --sign - "$bundle"
echo "built $bundle"

if [ "$1" = install ]; then
    mkdir -p "$drivers"
    rm -rf "$drivers/BlackBoxMIDI.plugin"
    cp -R "$bundle" "$drivers/"
    killall MIDIServer 2>/dev/null || true
    echo "installed to $drivers; MIDIServer restarted"
fi
