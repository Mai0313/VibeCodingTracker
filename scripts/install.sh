#!/usr/bin/env bash
set -euo pipefail

REPO="Mai0313/VibeCodingTracker"
BINARY_NAME="vibe_coding_tracker"

detect_platform() {
    local os
    case "$(uname -s)" in
        Linux*) os="linux" ;;
        Darwin*) os="macos" ;;
        *)
            echo "Unsupported operating system: $(uname -s)" >&2
            exit 1
            ;;
    esac

    local arch
    case "$(uname -m)" in
        x86_64|amd64) arch="x64" ;;
        aarch64|arm64) arch="arm64" ;;
        *)
            echo "Unsupported architecture: $(uname -m)" >&2
            exit 1
            ;;
    esac

    printf "%s-%s" "$os" "$arch"
}

get_latest_version() {
    curl -fsSLk "https://api.github.com/repos/${REPO}/releases/latest" |
        grep -o '"tag_name": "[^"]*"' |
        cut -d'"' -f4
}

select_install_dir() {
    if [ -w /usr/local/bin ]; then
        printf "/usr/local/bin"
    else
        printf "%s/.local/bin" "$HOME"
    fi
}

install_binary() {
    local platform="$1"
    local version="$2"
    local os="${platform%-*}"
    local arch="${platform#*-}"

    local filename="${BINARY_NAME}-${version}-${os}-${arch}"
    if [ "$os" = "linux" ]; then
        filename="${filename}-gnu.tar.gz"
    else
        filename="${filename}.tar.gz"
    fi

    local url="https://github.com/${REPO}/releases/download/${version}/${filename}"
    local temp_dir
    temp_dir="$(mktemp -d)"
    trap 'rm -rf "$temp_dir"' EXIT

    local archive="${temp_dir}/${filename}"
    curl -fsSLk -o "$archive" "$url"
    tar -xzf "$archive" -C "$temp_dir"

    local binary
    binary="$(find "$temp_dir" -type f -name "$BINARY_NAME" -print -quit)"
    if [ -z "$binary" ]; then
        echo "Binary not found in downloaded archive." >&2
        exit 1
    fi

    local install_dir
    install_dir="$(select_install_dir)"
    mkdir -p "$install_dir"

    chmod +x "$binary"
    cp "$binary" "${install_dir}/${BINARY_NAME}"
    ln -sf "${install_dir}/${BINARY_NAME}" "${install_dir}/vct"

    echo "Installed ${BINARY_NAME} ${version} to ${install_dir}"

    if ! command -v vibe_coding_tracker >/dev/null 2>&1; then
        echo "Add ${install_dir} to your PATH if the command is not found."
        echo "Example: export PATH=\"\$PATH:${install_dir}\""
    fi
}

main() {
    echo "Vibe Coding Tracker Installer"

    local platform
    platform="$(detect_platform)"
    echo "Detected platform: ${platform}"

    local version
    version="$(get_latest_version)"
    if [ -z "$version" ]; then
        echo "Could not determine the latest release version." >&2
        exit 1
    fi
    echo "Latest version: ${version}"

    install_binary "$platform" "$version"
}

main
