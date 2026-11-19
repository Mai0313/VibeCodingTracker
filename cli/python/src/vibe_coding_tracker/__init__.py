"""Wrapper package that locates and runs the bundled `vibe_coding_tracker` binary."""

import os
import platform
import subprocess
import sys
from pathlib import Path


def get_platform_info():
    """Returns a mapping with `dir` and `binary` keys for the current OS and architecture.

    Raises:
        SystemExit: The host architecture or platform is unsupported.
    """
    system = platform.system().lower()
    machine = platform.machine().lower()

    if machine in ("x86_64", "amd64"):
        arch = "x64"
    elif machine in ("aarch64", "arm64"):
        arch = "arm64"
    else:
        print(f"Unsupported architecture: {machine}", file=sys.stderr)
        sys.exit(1)

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
    """Returns the `Path` to the bundled platform binary, ensuring it is executable on non-Windows systems.

    Raises:
        SystemExit: The host platform is unsupported or the bundled binary is missing.
    """
    platform_info = get_platform_info()

    package_root = Path(__file__).parent
    binaries_dir = package_root / "binaries"

    if not binaries_dir.exists():
        print("Error: Binaries directory not found.", file=sys.stderr)
        print("Please reinstall the package.", file=sys.stderr)
        sys.exit(1)

    platform_dir = binaries_dir / platform_info["dir"]
    binary_path = platform_dir / platform_info["binary"]

    if not binary_path.exists():
        print(
            f"Error: Binary not found for your platform: {platform_info['dir']}/{platform_info['binary']}",
            file=sys.stderr,
        )
        print("Please reinstall the package.", file=sys.stderr)
        sys.exit(1)

    if platform.system() != "Windows":
        try:
            os.chmod(binary_path, 0o755)
        except OSError:
            # Ignore chmod errors (e.g. read-only install where the binary is already executable).
            pass

    return binary_path


def main():
    """Runs the bundled binary with `sys.argv[1:]` and exits with its return code.

    Raises:
        SystemExit: Always exits with the child return code, or 1 if the binary cannot be found or started.
    """
    binary_path = find_binary()

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
