import json
import subprocess
import sys
import time
from contextlib import contextmanager
from pathlib import Path
from threading import Timer

import pytest

ROOT = Path(__file__).resolve().parents[2]
ENTRY = str(ROOT / "worker/main.py")
FIXTURES = json.loads((ROOT / "contracts/examples/valid.json").read_text(encoding="utf-8"))
REQUEST = next(item["instance"] for item in FIXTURES if item["name"] == "handshake_request")


def command(delay=0, *, deadline=None):
    if deadline is None:
        deadline = time.monotonic_ns() + 5_000_000_000
    deadline = str(deadline)
    if delay:
        # Hold up the interpreter before the production entry point executes.
        code = (
            "import runpy,sys,time; time.sleep(float(sys.argv.pop(1))); "
            "sys.argv.pop(0); runpy.run_path(sys.argv[0],run_name='__main__')"
        )
        return [sys.executable, "-I", "-c", code, str(delay), ENTRY, deadline]
    return [sys.executable, "-I", ENTRY, deadline]


def line(value):
    return json.dumps(value).encode() + b"\n"


def exchange(data, exit_code):
    result = subprocess.run(command(), input=data, capture_output=True, timeout=10)
    assert result.stderr == b""
    assert result.returncode == exit_code
    return [json.loads(frame) for frame in result.stdout.splitlines()]


def test_mock_core_session():
    business = {**REQUEST, "id": "before", "method": "inference.transcribe"}
    notice = {
        "jsonrpc": "2.0",
        "method": "process.exiting",
        "params": {
            "protocol_version": "0.1.0",
            "task_id": None,
            "exit_code": 0,
        },
    }
    missing = {k: v for k, v in REQUEST.items() if k != "id"}
    frames = [missing, business, REQUEST, REQUEST, notice, {**business, "id": "after"}]
    invalid, before, success, duplicate, after = exchange(b"".join(map(line, frames)), 0)
    assert invalid["id"] is None and invalid["error"]["code"] == -32600
    expected = next(item["instance"] for item in FIXTURES if item["name"] == "handshake_response")
    assert success == {**expected, "result": {**expected["result"], "capabilities": []}}
    assert before["error"]["code"] == duplicate["error"]["code"] == -32600
    assert after["error"]["code"] == -32601


def test_mismatch_closes_without_processing_next_request():
    bad = {**REQUEST, "params": {**REQUEST["params"], "protocol_version": "0.2.0"}}
    (response,) = exchange(line(bad) + line(REQUEST), 2)
    assert response["id"] == REQUEST["id"]
    assert response["error"]["code"] == 1009
    assert response["error"]["data"]["details"]["reason"] == "protocol_version_mismatch"
    # Use the same offline schema registry as the repository validator.
    from scripts.validate_repository import load_validators

    load_validators()["rpc.schema.json"].validate(response)


@pytest.mark.parametrize("raw", [b"\xff\n", b"\n", b"NaN\n", b"1e999\n", b"\xef\xbb\xbf{}\n"])
def test_parse_errors(raw):
    error, success = exchange(raw + line(REQUEST), 0)
    assert error["id"] is None and error["error"]["code"] == -32700
    assert success["id"] == REQUEST["id"]


def test_frame_boundaries():
    raw = line(REQUEST)
    assert exchange(b" " * (1048576 - len(raw)) + raw, 0)[0]["id"] == REQUEST["id"]
    assert exchange(b" " * (1048577 - len(raw)) + raw, 2) == []
    assert exchange(raw.rstrip(), 2) == []


@pytest.mark.parametrize(
    "changes",
    [
        {"capabilities": ["asr", "asr"]},
        {"capabilities": ["asr\n"]},
        {"client_name": "other"},
        {"extra": True},
        {"task_id": "task"},
    ],
)
def test_invalid_params_leave_handshake_pending(changes):
    invalid = {**REQUEST, "id": "bad", "params": {**REQUEST["params"], **changes}}
    error, success = exchange(line(invalid) + line(REQUEST), 0)
    assert error["id"] == "bad" and error["error"]["code"] == -32602
    assert success["result"]["server_name"] == "worker"


def test_partial_handshake_expires_with_open_pipe():
    start = time.monotonic()
    with subprocess.Popen(command(3), stdin=subprocess.PIPE, stdout=subprocess.PIPE) as worker:
        worker.stdin.write(b"{")
        worker.stdin.flush()
        assert worker.wait(timeout=7) == 2
        assert 4.8 <= time.monotonic() - start < 7
        assert worker.stdout.read() == b""


def test_abnormal_exit_without_reply():
    with subprocess.Popen(command(), stdin=subprocess.PIPE, stdout=subprocess.PIPE) as worker:
        worker.stdin.write(line(REQUEST))
        worker.stdin.flush()
        timeout = Timer(10, worker.kill)
        timeout.start()
        try:
            assert json.loads(worker.stdout.readline())["id"] == REQUEST["id"]
        finally:
            timeout.cancel()
        worker.stdin.write(b'{"id":"pending"')
        worker.stdin.flush()
        worker.kill()
        assert worker.wait(timeout=5) != 0
        assert worker.stdout.read() == b""


def test_delayed_start_completes_partial_handshake_within_budget():
    raw = line(REQUEST)
    with subprocess.Popen(command(1), stdin=subprocess.PIPE, stdout=subprocess.PIPE) as worker:
        worker.stdin.write(raw[:1])
        worker.stdin.flush()
        time.sleep(1.5)
        output, _ = worker.communicate(raw[1:], timeout=5)
        assert worker.returncode == 0
        assert json.loads(output)["result"]["server_name"] == "worker"


@contextmanager
def mock_core_connection(delay):
    # The owner can close even when the child has not executed any Python yet.
    deadline = time.monotonic_ns() + 5_000_000_000
    with subprocess.Popen(
        command(delay, deadline=deadline), stdin=subprocess.PIPE, stdout=subprocess.PIPE
    ) as worker:
        guard = Timer(max(0, deadline - time.monotonic_ns()) / 1e9, worker.kill)
        guard.start()
        try:
            yield worker, guard
        finally:
            guard.cancel()
            guard.join()
            if worker.poll() is None:
                worker.kill()


def test_connection_owner_expires_stalled_start():
    start = time.monotonic()
    with mock_core_connection(30) as (worker, _):
        worker.stdin.write(line(REQUEST))
        worker.stdin.flush()
        assert worker.wait(timeout=7) != 0
        assert worker.stdout.read() == b""
        assert 4.8 <= time.monotonic() - start < 7
