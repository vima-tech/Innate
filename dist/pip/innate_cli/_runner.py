"""Finds the innate binary and delegates execution to it."""

import os
import shutil
import subprocess
import sys
from pathlib import Path

from ._installer import ensure_binary, _bin_path
from ._platform import get_binary_name


def find_binary() -> Path:
    """Locate innate binary: package-local first, then PATH."""
    local = _bin_path()
    if local.exists():
        return local

    sys_bin = shutil.which(get_binary_name())
    if sys_bin:
        return Path(sys_bin)

    # Binary not yet downloaded — try to fetch it.
    try:
        return ensure_binary()
    except Exception as e:
        print(
            f"innate-cli: binary not found and download failed: {e}\n"
            "Install manually: https://github.com/vima-tech/Innate/releases",
            file=sys.stderr,
        )
        sys.exit(1)


def main() -> None:
    """Entry point for the `innate` console script."""
    binary = find_binary()
    result = subprocess.run([str(binary)] + sys.argv[1:])
    sys.exit(result.returncode)
