"""Platform detection — maps Python platform info to Rust target triples."""

import platform
import sys

_TARGETS = {
    ("linux", "x86_64"):  "x86_64-unknown-linux-musl",
    ("linux", "aarch64"): "aarch64-unknown-linux-musl",
    ("linux", "armv7l"):  "armv7-unknown-linux-musleabihf",
    ("darwin", "x86_64"): "x86_64-apple-darwin",
    ("darwin", "arm64"):  "aarch64-apple-darwin",
    ("windows", "amd64"): "x86_64-pc-windows-msvc",
    ("windows", "x86_64"):"x86_64-pc-windows-msvc",
}


def get_target() -> str:
    system = platform.system().lower()
    machine = platform.machine().lower()
    # Normalise aliases
    if machine in ("amd64",):
        machine = "x86_64"
    key = (system, machine)
    target = _TARGETS.get(key)
    if not target:
        raise RuntimeError(
            f"innate-cli: unsupported platform {system}/{machine}.\n"
            "Install manually: https://github.com/innate-rs/innate/releases"
        )
    return target


def get_exe_suffix() -> str:
    return ".exe" if sys.platform == "win32" else ""


def get_binary_name() -> str:
    return "innate" + get_exe_suffix()
