#!/usr/bin/env bash

set -e

# Colors for output
RED='\033[0;31m'
GREEN='\033[0;32m'
YELLOW='\033[1;33m'
NC='\033[0m' # No Color

# GitHub repository information
REPO="Mai0313/VibeCodingTracker"
BINARY_NAME="vibe_coding_tracker"

# Detect OS and architecture
detect_platform() {
    local os=""
    local arch=""

    # Detect OS
    case "$(uname -s)" in
        Linux*)     os="linux" ;;
        Darwin*)    os="macos" ;;
        *)
            echo -e "${RED}Error: Unsupported operating system$(uname -s)${NC}"
            exit 1
            ;;
    esac

    # Detect architecture
    case "$(uname -m)" in
        x86_64|amd64)   arch="x64" ;;
        aarch64|arm64)  arch="arm64" ;;
        *)
            echo -e "${RED}Error: Unsupported architecture $(uname -m)${NC}"
            exit 1
            ;;
    esac

    echo "${os}-${arch}"
}

# Get latest release version
get_latest_version() {
    echo -e "${YELLOW}Fetching latest release version...${NC}"
    local version=$(curl -fsSLk "https://api.github.com/repos/${REPO}/releases/latest" | grep -o '"tag_name": "[^"]*"' | cut -d'"' -f4)

    if [ -z "$version" ]; then
        echo -e "${RED}Error: Failed to fetch latest version${NC}"
        exit 1
    fi

    echo "$version"
}

# Download and install binary
install_binary() {
    local platform="$1"
    local version="$2"
    local os=$(echo $platform | cut -d'-' -f1)
    local arch=$(echo $platform | cut -d'-' -f2)

    # Construct download URL
    local filename="${BINARY_NAME}-${version}-${os}-${arch}"
    if [ "$os" = "linux" ]; then
        filename="${filename}-gnu.tar.gz"
    else
        filename="${filename}.tar.gz"
    fi

    local download_url="https://github.com/${REPO}/releases/download/${version}/${filename}"

    echo -e "${YELLOW}Downloading ${filename}...${NC}"
    local temp_dir=$(mktemp -d)
    local archive_path="${temp_dir}/${filename}"

    if ! curl -fsSLk -o "$archive_path" "$download_url"; then
        echo -e "${RED}Error: Failed to download binary${NC}"
        rm -rf "$temp_dir"
        exit 1
    fi

    # Extract archive
    echo -e "${YELLOW}Extracting archive...${NC}"
    tar -xzf "$archive_path" -C "$temp_dir"

    # Determine install directory
    local install_dir=""
    if [ -w "/usr/local/bin" ]; then
        install_dir="/usr/local/bin"
    elif [ -d "$HOME/.local/bin" ]; then
        install_dir="$HOME/.local/bin"
    else
        mkdir -p "$HOME/.local/bin"
        install_dir="$HOME/.local/bin"
    fi

    # Install binary
    echo -e "${YELLOW}Installing to ${install_dir}...${NC}"

    # Find the binary in the extracted files
    local binary_path=$(find "$temp_dir" -name "$BINARY_NAME" -type f | head -n 1)

    if [ -z "$binary_path" ]; then
        echo -e "${RED}Error: Binary not found in archive${NC}"
        rm -rf "$temp_dir"
        exit 1
    fi

    chmod +x "$binary_path"

    if [ -w "$install_dir" ]; then
        mv "$binary_path" "${install_dir}/${BINARY_NAME}"
    else
        sudo mv "$binary_path" "${install_dir}/${BINARY_NAME}"
    fi

    # Create symlink for short alias
    if [ -w "$install_dir" ]; then
        ln -sf "${install_dir}/${BINARY_NAME}" "${install_dir}/vct"
    else
        sudo ln -sf "${install_dir}/${BINARY_NAME}" "${install_dir}/vct"
    fi

    # Clean up
    rm -rf "$temp_dir"

    echo -e "${GREEN}✓ Installation complete!${NC}"
    echo -e "${GREEN}Run 'vct --help' or 'vibe_coding_tracker --help' to get started${NC}"

    # Check if install_dir is in PATH
    if [[ ":$PATH:" != *":$install_dir:"* ]]; then
        echo -e "${YELLOW}Warning: ${install_dir} is not in your PATH${NC}"
        echo -e "${YELLOW}Add this to your shell profile (~/.bashrc, ~/.zshrc, etc.):${NC}"
        echo -e "${YELLOW}export PATH=\"\$PATH:${install_dir}\"${NC}"
    fi
}

# Main installation flow
main() {
    echo -e "${GREEN}Vibe Coding Tracker Installer${NC}"
    echo ""

    local platform=$(detect_platform)
    echo -e "${GREEN}Detected platform: ${platform}${NC}"

    local version=$(get_latest_version)
    echo -e "${GREEN}Latest version: ${version}${NC}"
    echo ""

    install_binary "$platform" "$version"
}

main
