"""Start the isolated Worker using the connection owner's monotonic deadline."""

import os
import runpy
import sys
import time
from pathlib import Path
from threading import Timer


def main() -> int:
    if len(sys.argv) != 2 or not sys.argv[1].isascii() or not sys.argv[1].isdigit():
        return 2
    now = time.monotonic_ns()
    if len(sys.argv[1]) > 20:
        return 2
    remaining = int(sys.argv[1]) - now
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
