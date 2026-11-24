import json
import subprocess
import sys
from pathlib import Path
from threading import Timer

import pytest

ROOT = Path(__file__).resolve().parents[2]
COMMAND = [sys.executable, "-I", str(ROOT / "worker/main.py")]
FIXTURES = json.loads((ROOT / "contracts/examples/valid.json").read_text(encoding="utf-8"))
REQUEST = next(item["instance"] for item in FIXTURES if item["name"] == "handshake_request")


def line(value):
    return json.dumps(value).encode() + b"\n"


def exchange(data, exit_code):
    result = subprocess.run(COMMAND, input=data, capture_output=True, timeout=10)
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
    frames = [business, REQUEST, REQUEST, notice, {**business, "id": "after"}]
    before, success, duplicate, after = exchange(b"".join(map(line, frames)), 0)
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
    with subprocess.Popen(COMMAND, stdin=subprocess.PIPE, stdout=subprocess.PIPE) as worker:
        worker.stdin.write(b"{")
        worker.stdin.flush()
        assert worker.wait(timeout=10) == 2
        assert worker.stdout.read() == b""


def test_abnormal_exit_without_reply():
    with subprocess.Popen(COMMAND, stdin=subprocess.PIPE, stdout=subprocess.PIPE) as worker:
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
