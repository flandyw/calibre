#!/usr/bin/env bash
# Install calibre-rust for the current Linux user without changing default apps.
set -euo pipefail

profile=release
binary=
uninstall=false
while (( $# )); do
    case "$1" in
        --debug) profile=debug ;;
        --binary)
            if (( $# < 2 )); then
                echo "install.sh: --binary needs a path" >&2
                exit 2
            fi
            binary=$2
            shift
            ;;
        --uninstall) uninstall=true ;;
        -h|--help)
            echo "Usage: install.sh [--debug | --binary PATH | --uninstall]"
            exit 0
            ;;
        *) echo "install.sh: unknown option: $1" >&2; exit 2 ;;
    esac
    shift
done

if [[ $(uname -s) != Linux ]]; then
    echo "install.sh is for Linux." >&2
    exit 1
fi

here=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)
crate=$(cd -- "$here/../.." && pwd)
desktop_data=${XDG_DATA_HOME:-"$HOME/.local/share"}
[[ $desktop_data = /* ]] || desktop_data="$HOME/.local/share"
bin_path="$HOME/.local/bin/calibre-rust"
applications="$desktop_data/applications"
mime="$desktop_data/mime"
icon_png="$desktop_data/icons/hicolor/1024x1024/apps/calibre-rust.png"
icon_svg="$desktop_data/icons/hicolor/scalable/apps/calibre-rust.svg"
metainfo="$desktop_data/metainfo/io.github.flandyw.calibre.metainfo.xml"

if $uninstall; then
    rm -f -- "$bin_path" "$applications/calibre-rust.desktop" \
        "$mime/packages/calibre-rust-mime.xml" "$icon_png" "$icon_svg" "$metainfo"
else
    if [[ -z $binary ]]; then
        target=${CARGO_TARGET_DIR:-"$crate/target"}
        build_args=(--locked --manifest-path "$crate/Cargo.toml" --bin calibre-rust --target-dir "$target")
        [[ $profile != release ]] || build_args+=(--release)
        cargo build "${build_args[@]}"
        binary="$target/$profile/calibre-rust"
    fi
    if [[ ! -f $binary || ! -x $binary ]]; then
        echo "No executable calibre-rust binary at $binary" >&2
        exit 1
    fi
    if [[ ! $binary -ef $bin_path ]]; then
        install -Dm755 -- "$binary" "$bin_path"
    fi
    install -Dm644 -- "$here/../macos/icon-1024.png" "$icon_png"
    install -Dm644 -- "$here/../macos/icon.svg" "$icon_svg"
    install -Dm644 -- "$here/calibre-rust-mime.xml" "$mime/packages/calibre-rust-mime.xml"
    install -Dm644 -- "$here/io.github.flandyw.calibre.metainfo.xml" "$metainfo"
    install -Dm644 -- "$here/calibre-rust.desktop" "$applications/calibre-rust.desktop"
fi

if [[ -d $mime ]] && command -v update-mime-database >/dev/null; then
    update-mime-database "$mime"
fi
if [[ -d $applications ]] && command -v update-desktop-database >/dev/null; then
    update-desktop-database "$applications"
fi

if $uninstall; then
    echo "Removed calibre-rust desktop integration."
else
    echo "Installed calibre-rust to $bin_path."
fi
