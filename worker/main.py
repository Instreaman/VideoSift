"""VideoSift's stdio JSONL worker entry point."""

import json
import math
import os
import re
import sys
from threading import Timer
from typing import Annotated, Literal

from pydantic import BaseModel, ConfigDict, Field, ValidationError

VERSION = "0.1.0"
MAX_FRAME = 1_048_576
MAX_IDS = 4096
ID = re.compile(r"[A-Za-z0-9_-]{1,128}")
Capability = Annotated[str, Field(max_length=128, pattern=r"^[a-z][a-z0-9_.]*$")]


class Handshake(BaseModel):
    model_config = ConfigDict(extra="forbid", strict=True)

    protocol_version: str
    task_id: None
    client_name: Literal["desktop", "cli", "core", "worker"]
    capabilities: Annotated[list[Capability], Field(max_length=64)]


def finite_float(raw: str) -> float:
    value = float(raw)
    if not math.isfinite(value):
        raise ValueError("Non-finite number")
    return value


def reply(request_id, **payload):
    frame = {"jsonrpc": "2.0", "id": request_id, **payload}
    sys.stdout.buffer.write(json.dumps(frame, separators=(",", ":")).encode("ascii") + b"\n")
    sys.stdout.buffer.flush()


def reject(request_id, code, message, **extra):
    reply(request_id, error={"code": code, "message": message, **extra})


def serve(deadline: Timer) -> int:
    ready = False
    seen = set()
    while line := sys.stdin.buffer.readline(MAX_FRAME + 1):
        if len(line) > MAX_FRAME or not line.endswith(b"\n"):
            return 2
        try:
            frame = json.loads(
                line.decode("utf-8"), parse_float=finite_float, parse_constant=finite_float
            )
        except (ValueError, RecursionError):
            reject(None, -32700, "Invalid JSON")
            continue
        if not isinstance(frame, dict):
            reject(None, -32600, "Invalid request")
            continue
        request_id = frame.get("id")
        if not isinstance(request_id, str) or not ID.fullmatch(request_id):
            if (
                set(frame) == {"jsonrpc", "method", "params"}
                and frame["jsonrpc"] == "2.0"
                and isinstance(frame["method"], str)
                and isinstance(frame["params"], dict)
            ):
                continue
            reject(None, -32600, "Invalid request")
            continue
        if (
            set(frame) != {"jsonrpc", "id", "method", "params"}
            or frame["jsonrpc"] != "2.0"
            or not isinstance(frame["method"], str)
            or request_id in seen
        ):
            reject(request_id, -32600, "Invalid or duplicate request")
            continue
        if len(seen) >= MAX_IDS:
            reject(request_id, -32600, "Connection request limit reached")
            return 2
        seen.add(request_id)
        if frame["method"] != "system.handshake":
            if ready:
                reject(request_id, -32601, "Unknown worker method")
            else:
                reject(request_id, -32600, "Handshake required")
            continue
        if ready:
            reject(request_id, -32600, "Handshake already completed")
            continue
        try:
            params = Handshake.model_validate(frame["params"])
        except ValidationError:
            reject(request_id, -32602, "Invalid handshake parameters")
            continue
        if len(set(params.capabilities)) != len(params.capabilities):
            reject(request_id, -32602, "Duplicate capabilities")
            continue
        if params.protocol_version != VERSION:
            message = "Incompatible protocol version"
            reject(
                request_id,
                1009,
                message,
                data={
                    "code": "configuration",
                    "message": message,
                    "protocol_version": VERSION,
                    "task_id": None,
                    "recoverable": False,
                    "details": {"reason": "protocol_version_mismatch"},
                },
            )
            return 2
        reply(
            request_id,
            result={
                "protocol_version": VERSION,
                "server_name": "worker",
                "task_id": None,
                "capabilities": [],
            },
        )
        ready = True
        deadline.cancel()
    return 0 if ready else 2


if __name__ == "__main__":
    # A partial frame can block readline on either OS. Expire the entire worker.
    timer = Timer(5, os._exit, args=(2,))
    timer.start()
    try:
        sys.exit(serve(timer))
    except (BrokenPipeError, OSError):
        sys.exit(2)
    finally:
        timer.cancel()
