"""Vibe Coding Tracker.

The CLI is a native binary that the wheel installs directly into the
environment's script directory, so there is no Python launcher to run. This
module exists only to give the distribution an importable name and to expose
the installed version.
"""

from importlib.metadata import PackageNotFoundError, version

try:
    __version__ = version("vibe_coding_tracker")
except PackageNotFoundError:  # Running from a source checkout.
    __version__ = "0.0.0"

__all__ = ["__version__"]
