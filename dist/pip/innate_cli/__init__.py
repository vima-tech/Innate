"""innate-cli — thin Python wrapper around the innate Rust binary."""

import sys
from ._runner import main, find_binary

__version__ = "0.1.0"
__all__ = ["main", "find_binary"]
