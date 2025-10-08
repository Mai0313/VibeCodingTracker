import os
import platform
import subprocess
import sys
from pathlib import Path


def get_platform_info():
    """Determine platform-specific directory and binary name."""
    system = platform.system().lower()
    machine = platform.machine().lower()

    # Normalize architecture names
    if machine in ("x86_64", "amd64"):
        arch = "x64"
    elif machine in ("aarch64", "arm64"):
        arch = "arm64"
    else:
        print(f"Unsupported architecture: {machine}", file=sys.stderr)
        sys.exit(1)

    # Map to platform-specific directory and binary name
    platform_map = {
        "darwin": {
            "x64": {"dir": "macos-x64", "binary": "vibe_coding_tracker"},
            "arm64": {"dir": "macos-arm64", "binary": "vibe_coding_tracker"},
        },
        "linux": {
            "x64": {"dir": "linux-x64-gnu", "binary": "vibe_coding_tracker"},
            "arm64": {"dir": "linux-arm64-gnu", "binary": "vibe_coding_tracker"},
        },
        "windows": {
            "x64": {"dir": "windows-x64", "binary": "vibe_coding_tracker.exe"},
            "arm64": {"dir": "windows-arm64", "binary": "vibe_coding_tracker.exe"},
        },
    }

    if system not in platform_map or arch not in platform_map[system]:
        print(f"Unsupported platform: {system}-{arch}", file=sys.stderr)
        sys.exit(1)

    return platform_map[system][arch]


def find_binary():
    """Find the binary for current platform."""
    platform_info = get_platform_info()

    # Get package root directory
    package_root = Path(__file__).parent
    binaries_dir = package_root / "binaries"

    if not binaries_dir.exists():
        print("Error: Binaries directory not found.", file=sys.stderr)
        print("Please reinstall the package.", file=sys.stderr)
        sys.exit(1)

    # Look for the binary in platform-specific subdirectory
    platform_dir = binaries_dir / platform_info["dir"]
    binary_path = platform_dir / platform_info["binary"]

    if not binary_path.exists():
        print(
            f"Error: Binary not found for your platform: {platform_info['dir']}/{platform_info['binary']}",
            file=sys.stderr,
        )
        print("Please reinstall the package.", file=sys.stderr)
        sys.exit(1)

    # Make binary executable on Unix-like systems
    if platform.system() != "Windows":
        try:
            os.chmod(binary_path, 0o755)
        except OSError:
            # Ignore error if already executable
            pass

    return binary_path


def main():
    """Main entry point that forwards all arguments to the binary."""
    binary_path = find_binary()

    # Forward all arguments to the binary
    args = sys.argv[1:]

    try:
        result = subprocess.run(
            [str(binary_path)] + args,
            check=False,
        )
        sys.exit(result.returncode)
    except Exception as err:
        print(f"Failed to start binary: {err}", file=sys.stderr)
        sys.exit(1)


if __name__ == "__main__":
    main()
