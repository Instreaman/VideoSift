"""Start the isolated Worker using the connection owner's monotonic deadline."""

import os
import runpy
import sys
import time
from pathlib import Path
from threading import Timer


def main() -> int:
    if len(sys.argv) != 2:
        return 2
    now = time.monotonic_ns()
    arg = sys.argv[1]
    if arg == "--supervised":
        remaining = 5_000_000_000  # The Rust owner enforces the earlier connection deadline.
    elif arg.isascii() and arg.isdigit() and len(arg) <= 20:
        remaining = int(arg) - now
    else:
        return 2
    if not 0 < remaining <= 5_000_000_000:
        return 2
    timer = Timer(remaining / 1e9, os._exit, args=(2,))
    timer.start()
    try:
        protocol = runpy.run_path(str(Path(__file__).with_name("protocol.py")))
        return protocol["serve"](timer)
    except OSError:
        return 2
    finally:
        timer.cancel()


if __name__ == "__main__":
    sys.exit(main())
