#!/usr/bin/env bash
# Builds calibre-rust.app from the cr-gui crate.
#
#   crates/cr-gui/packaging/macos/bundle.sh [--debug] [--redraw-icon] [--open]
#
#   --debug         bundle a debug build instead of an optimised release build
#   --redraw-icon   re-render icon-1024.png from icon.svg first (needs rsvg-convert)
#   --open          launch the app when the bundle is ready
#
# The bundle is written to <target dir>/<profile>/bundle/calibre-rust.app and
# signed ad hoc, which is enough to run it on this Mac. Distributing it to
# other Macs needs a Developer ID signature and notarization.
set -euo pipefail

profile=release
redraw_icon=0
open_app=0
for arg in "$@"; do
    case "$arg" in
        --debug) profile=debug ;;
        --redraw-icon) redraw_icon=1 ;;
        --open) open_app=1 ;;
        -h | --help)
            sed -n '2,13p' "$0" | sed 's/^# \{0,1\}//'
            exit 0
            ;;
        *)
            echo "unknown option: $arg" >&2
            exit 2
            ;;
    esac
done

if [[ "$(uname -s)" != "Darwin" ]]; then
    echo "bundle.sh builds a macOS .app and must run on macOS." >&2
    exit 1
fi

here="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
crate="$(cd "$here/../.." && pwd)"
manifest="$crate/Cargo.toml"
version="$(sed -n 's/^version = "\(.*\)"/\1/p' "$manifest" | head -n 1)"
if [[ -z "$version" ]]; then
    echo "could not read the version from $manifest" >&2
    exit 1
fi

# --- Icon -------------------------------------------------------------------
if [[ $redraw_icon -eq 1 || ! -f "$here/icon-1024.png" ]]; then
    echo "==> Drawing the app icon"
    if ! command -v rsvg-convert >/dev/null; then
        echo "rendering the icon needs rsvg-convert (brew install librsvg)" >&2
        exit 1
    fi
    rsvg-convert -w 1024 -h 1024 "$here/icon.svg" -o "$here/icon-1024.png"
fi

# --- Binary -----------------------------------------------------------------
echo "==> Building calibre-rust ($profile)"
target_dir=${CARGO_TARGET_DIR:-"$crate/target"}
cargo_args=(build --locked --manifest-path "$manifest" --bin calibre-rust --target-dir "$target_dir")
[[ $profile == release ]] && cargo_args+=(--release)
cargo "${cargo_args[@]}"

binary="$target_dir/$profile/calibre-rust"
if [[ ! -x "$binary" ]]; then
    echo "expected a binary at $binary" >&2
    exit 1
fi

# --- Bundle -----------------------------------------------------------------
out_dir="$target_dir/$profile/bundle"
app="$out_dir/calibre-rust.app"
echo "==> Assembling $app"
rm -rf "$app"
mkdir -p "$app/Contents/MacOS" "$app/Contents/Resources"

cp "$binary" "$app/Contents/MacOS/calibre-rust"
sed "s/@VERSION@/$version/g" "$here/Info.plist" >"$app/Contents/Info.plist"
printf 'APPL????' >"$app/Contents/PkgInfo"
plutil -lint -s "$app/Contents/Info.plist"

iconset="$(mktemp -d)/AppIcon.iconset"
mkdir -p "$iconset"
for size in 16 32 128 256 512; do
    sips -z "$size" "$size" "$here/icon-1024.png" --out "$iconset/icon_${size}x${size}.png" >/dev/null
    double=$((size * 2))
    sips -z "$double" "$double" "$here/icon-1024.png" --out "$iconset/icon_${size}x${size}@2x.png" >/dev/null
done
iconutil -c icns "$iconset" -o "$app/Contents/Resources/AppIcon.icns"
rm -rf "$(dirname "$iconset")"

# --- Sign -------------------------------------------------------------------
echo "==> Signing (ad hoc)"
codesign --force --sign - --timestamp=none "$app"
codesign --verify --strict "$app"

# Make Finder and the Dock pick up the new icon and document types.
lsregister=/System/Library/Frameworks/CoreServices.framework/Frameworks/LaunchServices.framework/Support/lsregister
[[ -x "$lsregister" ]] && "$lsregister" -f "$app" >/dev/null 2>&1 || true
touch "$app"

size="$(du -sh "$app" | cut -f1)"
echo "==> Built $app ($size, version $version)"

if [[ $open_app -eq 1 ]]; then
    open "$app"
fi
