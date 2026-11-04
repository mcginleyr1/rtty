# rtty's build and install shortcuts. `just` with no arguments lists them.

default:
    @just --list

# rtty.app in target/: the release binary, Info.plist at Cargo.toml's version, the icon; ad-hoc signed
bundle:
    #!/bin/sh
    set -eu
    cargo build --release
    contents=target/rtty.app/Contents
    rm -rf target/rtty.app
    mkdir -p "$contents/MacOS" "$contents/Resources"
    cp target/release/rtty "$contents/MacOS/"
    cp bundle/rtty.icns "$contents/Resources/"
    cp bundle/Info.plist "$contents/"
    id=$(cargo pkgid)
    plutil -replace CFBundleShortVersionString -string "${id##*[#@]}" "$contents/Info.plist"
    plutil -replace CFBundleVersion -string "${id##*[#@]}" "$contents/Info.plist"
    codesign --force --sign - target/rtty.app

# Put rtty.app in DEST, replacing the one there, and link BIN/rtty to its binary for `rtty cli`
install dest="/Applications" bin=(env_var("HOME") / ".local/bin"): bundle
    rm -rf "{{dest}}/rtty.app"
    ditto target/rtty.app "{{dest}}/rtty.app"
    mkdir -p "{{bin}}"
    ln -sf "{{dest}}/rtty.app/Contents/MacOS/rtty" "{{bin}}/rtty"

# Remove DEST/rtty.app, and BIN/rtty when it is the link install made
uninstall dest="/Applications" bin=(env_var("HOME") / ".local/bin"):
    rm -rf "{{dest}}/rtty.app"
    if [ "$(readlink "{{bin}}/rtty")" = "{{dest}}/rtty.app/Contents/MacOS/rtty" ]; then rm "{{bin}}/rtty"; fi

# Redraw bundle/rtty.icns from bundle/icon.py (needs uv); target/icon/icon-sizes.png shows it small
icon:
    #!/bin/sh
    set -eu
    rm -rf target/icon
    mkdir -p target/icon/rtty.iconset
    uv run --with pillow bundle/icon.py target/icon
    for size in 16 32 128 256 512; do
        sips -z $size $size target/icon/icon-1024.png --out target/icon/rtty.iconset/icon_${size}x${size}.png >/dev/null
        sips -z $((size * 2)) $((size * 2)) target/icon/icon-1024.png --out target/icon/rtty.iconset/icon_${size}x${size}@2x.png >/dev/null
    done
    iconutil -c icns target/icon/rtty.iconset -o bundle/rtty.icns
