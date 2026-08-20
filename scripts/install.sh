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

# Removes what this run created and nothing else, so a failure at any point leaves the install
# directory as it was found. STAGE_DIR and NEW_MARKER are tested rather than passed straight to rm:
# BSD `rm -r` hands its whole operand list to fts_open(), which rejects a zero-length name outright,
# so one empty argument would silently spare every other operand as well. Each removal is kept from
# failing because under `set -e` a failing trap decides the script's exit status.
cleanup() {
    rm -rf "$TEMP_DIR" || true
    if [ -n "$STAGE_DIR" ]; then
        rm -rf "$STAGE_DIR" || true
    fi
    if [ -n "$NEW_MARKER" ]; then
        rm -f "$NEW_MARKER" || true
    fi
    return 0
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
    # Not locals: the EXIT trap outlives this function, and under `set -u` an out-of-scope name
    # makes the trap fail instead of cleaning up. The other two stay empty until the install
    # directory has been chosen; the trap is armed after mktemp so TEMP_DIR is never unset.
    STAGE_DIR=""
    NEW_MARKER=""
    TEMP_DIR="$(mktemp -d)"
    trap cleanup EXIT

    local archive="${TEMP_DIR}/${filename}"
    curl -fsSL -o "$archive" "$url" || download_failed "$url"
    tar -xzf "$archive" -C "$TEMP_DIR"

    local binary
    binary="$(find "$TEMP_DIR" -type f -name "$BINARY_NAME" -print -quit)"
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
    # Staged beside the targets so each move into place is a same-filesystem rename, and inside one
    # directory of its own so a single `rm -rf` in the trap reaps whatever a failed run staged.
    STAGE_DIR="$(mktemp -d "${canonical_install_dir}/.${BINARY_NAME}.XXXXXX")"

    local staged_target="${STAGE_DIR}/${BINARY_NAME}"
    cp "$binary" "$staged_target"
    chmod +x "$staged_target"

    local marker="${canonical_install_dir}/${BINARY_NAME}.vct-managed"
    local staged_marker="${STAGE_DIR}/${BINARY_NAME}.vct-managed"
    printf 'vct-release-installer-v1\n' > "$staged_marker"
    # Readable only by the installing user, which for a root install into /usr/local/bin is the only
    # user who could act on it: the marker is what lets the startup auto-update replace the binary,
    # and every other user would reach that path only to fail on the same directory it cannot write.
    chmod 600 "$staged_marker"

    # Both files are ready before either lands, and the marker lands first. Two renames cannot be
    # made atomic, so this is the order whose half-done state is survivable: a binary installed
    # without its marker is one the startup auto-update silently never fires for again, whereas an
    # unaccompanied marker is undone below. Only one this run put down is undone, since a marker
    # that was already there belongs to the install that is already there.
    if [ ! -e "$marker" ]; then
        NEW_MARKER="$marker"
    fi
    mv -f "$staged_marker" "$marker"
    mv -f "$staged_target" "$target"
    # The binary the marker claims is in place, so the marker is no longer this run's to undo.
    NEW_MARKER=""
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
