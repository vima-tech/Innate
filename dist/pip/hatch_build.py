"""Hatch build hook — downloads the innate binary during `pip install` (wheel build)."""

import os
import sys
from pathlib import Path

# Allow disabling via env var (useful in CI / sdist builds).
if os.environ.get("INNATE_SKIP_DOWNLOAD"):
    # Provide a stub hook that does nothing.
    from hatchling.builders.hooks.plugin.interface import BuildHookInterface

    class CustomBuildHook(BuildHookInterface):
        def initialize(self, version, build_data):
            pass
else:
    sys.path.insert(0, str(Path(__file__).parent))
    from hatchling.builders.hooks.plugin.interface import BuildHookInterface

    class CustomBuildHook(BuildHookInterface):
        def initialize(self, version, build_data):
            from innate_cli._installer import download_binary, _bin_path

            dest = _bin_path()
            if not dest.exists():
                try:
                    download_binary(verbose=True)
                except Exception as e:
                    print(
                        f"innate-ai: binary download failed during build: {e}\n"
                        "The binary will be downloaded on first use instead.",
                        file=sys.stderr,
                    )
                    return

            # Include the binary in the wheel.
            build_data["force_include"][str(dest)] = f"innate_cli/bin/{dest.name}"
