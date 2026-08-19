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

# curl already prints the underlying reason; this only says what to do about it.
download_failed() {
    echo "Download failed: $1" >&2
    echo "If the error above is a certificate problem, this machine does not trust the" >&2
    echo "server's issuer. Update your CA certificates, or point CURL_CA_BUNDLE at your" >&2
    echo "proxy's CA, and retry. This installer never skips certificate verification." >&2
    exit 1
}

get_latest_version() {
    local url="https://api.github.com/repos/${REPO}/releases/latest"
    local response
    response="$(curl -fsSL "$url")" || download_failed "$url"
    printf "%s" "$response" |
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
    curl -fsSL -o "$archive" "$url" || download_failed "$url"
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

    local target="${install_dir}/${BINARY_NAME}"
    local canonical_install_dir
    canonical_install_dir="$(cd -P "$install_dir" && pwd)"
    local staged_target
    staged_target="$(mktemp "${canonical_install_dir}/.${BINARY_NAME}.XXXXXX")"
    local staged_marker
    staged_marker="$(mktemp "${canonical_install_dir}/.${BINARY_NAME}.marker.XXXXXX")"

    cp "$binary" "$staged_target"
    chmod +x "$staged_target"
    mv -f "$staged_target" "$target"
    printf 'vct-release-installer-v1\n' > "$staged_marker"
    mv -f "$staged_marker" "${canonical_install_dir}/${BINARY_NAME}.vct-managed"
    ln -sf "$target" "${install_dir}/vct"

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
