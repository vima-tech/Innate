"""Downloads the innate binary from GitHub releases on first use."""

import hashlib
import os
import stat
import sys
import tempfile
import urllib.request
from pathlib import Path

from ._platform import get_target, get_binary_name, get_exe_suffix

__version__ = "0.1.8"
_REPO = "vima-tech/Innate"


def _bin_dir() -> Path:
    """Package-local directory where the binary is cached."""
    return Path(__file__).parent / "bin"


def _bin_path() -> Path:
    return _bin_dir() / get_binary_name()


def _base_url() -> str:
    return f"https://github.com/{_REPO}/releases/download/v{__version__}"


def _download(url: str, dest: Path, show_progress: bool = True) -> None:
    req = urllib.request.Request(
        url, headers={"User-Agent": f"innate-ai/{__version__} Python/{sys.version_info[0]}"}
    )
    with urllib.request.urlopen(req) as resp:
        total = int(resp.headers.get("Content-Length", 0))
        received = 0
        chunk = 64 * 1024
        with dest.open("wb") as f:
            while True:
                data = resp.read(chunk)
                if not data:
                    break
                f.write(data)
                received += len(data)
                if show_progress and total and sys.stderr.isatty():
                    pct = received * 100 // total
                    print(f"\r  Downloading... {pct}%", end="", file=sys.stderr, flush=True)
    if show_progress and total and sys.stderr.isatty():
        print(file=sys.stderr)


def _verify_checksum(file_path: Path, sum_url: str) -> None:
    with tempfile.NamedTemporaryFile(delete=False, suffix=".sha256") as tmp:
        tmp_path = Path(tmp.name)
    try:
        _download(sum_url, tmp_path, show_progress=False)
        expected = tmp_path.read_text().strip().split()[0]
        actual = hashlib.sha256(file_path.read_bytes()).hexdigest()
        if expected != actual:
            raise RuntimeError(
                f"Checksum mismatch:\n  expected {expected}\n  got      {actual}"
            )
        print("  ✓ Checksum verified", file=sys.stderr)
    except urllib.error.HTTPError:
        print("  ⚠ Checksum file not available — skipping", file=sys.stderr)
    finally:
        tmp_path.unlink(missing_ok=True)


def download_binary(verbose: bool = True) -> Path:
    """Download the platform binary; return its path."""
    target = get_target()
    ext    = get_exe_suffix()
    base   = _base_url()
    bin_url = f"{base}/innate-{target}{ext}"
    sum_url = f"{base}/innate-{target}{ext}.sha256"

    bin_dir = _bin_dir()
    bin_dir.mkdir(parents=True, exist_ok=True)
    dest = _bin_path()

    if verbose:
        print(f"innate-ai: downloading v{__version__} ({target})", file=sys.stderr)

    with tempfile.NamedTemporaryFile(delete=False, suffix=ext) as tmp:
        tmp_path = Path(tmp.name)
    try:
        _download(bin_url, tmp_path, show_progress=verbose)
        _verify_checksum(tmp_path, sum_url)
        tmp_path.chmod(tmp_path.stat().st_mode | stat.S_IEXEC | stat.S_IXGRP | stat.S_IXOTH)
        tmp_path.rename(dest)
    except Exception:
        tmp_path.unlink(missing_ok=True)
        raise

    if verbose:
        print(f"  ✓ Installed to {dest}", file=sys.stderr)
    return dest


def ensure_binary() -> Path:
    """Return path to binary, downloading if necessary."""
    dest = _bin_path()
    if dest.exists():
        return dest
    return download_binary()
