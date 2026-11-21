"""Validate Phase 0 contracts and repository text without contacting external services."""

from __future__ import annotations

import json
import math
import re
import subprocess
import sys
import tomllib
import xml.etree.ElementTree as element_tree
from pathlib import Path
from typing import Any

from jsonschema import Draft202012Validator
from referencing import Registry, Resource
from referencing.exceptions import NoSuchResource

ROOT = Path(__file__).resolve().parents[1]
SCHEMA_NAMES = ("domain.schema.json", "rpc.schema.json")
SKIP_DIRECTORIES = {
    ".git",
    ".venv",
    ".tools",
    ".artifacts",
    ".tmp",
    ".cache",
    "__pycache__",
    ".ruff_cache",
    ".pytest_cache",
    "target",
}
TEXT_SUFFIXES = {".md", ".json", ".py", ".rs", ".toml", ".txt", ".yml", ".props"}
TEXT_NAMES = {
    ".editorconfig",
    ".gitattributes",
    ".gitignore",
    ".clang-format",
    "Cargo.lock",
    "LICENSE",
}
MARKDOWN_LINK = re.compile(r"\[[^\]]*\]\(([^\s)]+)\)")


class ContractError(ValueError):
    """Report a stable validation category without echoing untrusted content."""

    def __init__(self, category: str, message: str) -> None:
        self.category = category
        super().__init__(message)


def reject_duplicate_keys(pairs: list[tuple[str, Any]]) -> dict[str, Any]:
    result: dict[str, Any] = {}
    for key, value in pairs:
        if key in result:
            raise ContractError("json", "Duplicate JSON key")
        result[key] = value
    return result


def reject_non_finite(value: str) -> None:
    raise ContractError("json", "Non-finite JSON number")


def parse_finite_float(value: str) -> float:
    number = float(value)
    if not math.isfinite(number):
        raise ContractError("json", "JSON number exceeds finite range")
    return number


def read_json(path: Path) -> Any:
    return json.loads(
        path.read_text(encoding="utf-8"),
        object_pairs_hook=reject_duplicate_keys,
        parse_constant=reject_non_finite,
        parse_float=parse_finite_float,
    )


def deny_remote_reference(uri: str) -> Resource:
    raise NoSuchResource(ref=uri)


def load_validators(root: Path = ROOT) -> dict[str, Draft202012Validator]:
    schemas = {name: read_json(root / "contracts" / name) for name in SCHEMA_NAMES}
    registry = Registry(retrieve=deny_remote_reference).with_resources(
        (schema["$id"], Resource.from_contents(schema)) for schema in schemas.values()
    )
    validators = {}
    for name, schema in schemas.items():
        Draft202012Validator.check_schema(schema)
        validators[name] = Draft202012Validator(schema, registry=registry)
    return validators


def require(condition: bool, message: str) -> None:
    if not condition:
        raise ContractError("semantic", message)


def validate_semantics(value: Any) -> None:
    """Check cross-field constraints that portable JSON Schema cannot express."""
    if isinstance(value, list):
        for item in value:
            validate_semantics(item)
        return
    if not isinstance(value, dict):
        return

    if "start_ms" in value and "end_ms" in value:
        require(value["start_ms"] < value["end_ms"], "Time range must be nonempty and ordered")
    if "segments" in value:
        identifiers = [segment["evidence_id"] for segment in value["segments"]]
        require(len(identifiers) == len(set(identifiers)), "Duplicate transcript evidence ID")
        starts = [segment["start_ms"] for segment in value["segments"]]
        require(starts == sorted(starts), "Transcript segments must be time ordered")
    if "items" in value and "evidence_ids" in value:
        evidence = set(value["evidence_ids"])
        identifiers = [item["knowledge_item_id"] for item in value["items"]]
        require(len(identifiers) == len(set(identifiers)), "Duplicate knowledge item ID")
        for item in value["items"]:
            require(set(item["evidence_ids"]) <= evidence, "Unknown evidence reference")
    if "completed" in value and "total" in value and value["total"] is not None:
        require(value["completed"] <= value["total"], "Progress exceeds total")
    if "state" in value and "error" in value:
        error = value["error"]
        if value["state"] in {"failed", "waiting_provider"}:
            require(error is not None, "Task state requires an error")
        if error is not None:
            require(error["task_id"] == value["task_id"], "Task error ID mismatch")
        if value["state"] == "waiting_provider":
            require(error["code"] == "rate_limit", "Waiting provider requires a rate limit")
    if "jsonrpc" in value and "error" in value:
        error = value["error"]
        if "data" in error:
            require(error["message"] == error["data"]["message"], "Error message mismatch")
    for child in value.values():
        validate_semantics(child)


def validate_instance(
    schema_name: str, instance: Any, validators: dict[str, Draft202012Validator]
) -> None:
    validator = validators[schema_name]
    if next(validator.iter_errors(instance), None) is not None:
        raise ContractError("schema", f"Instance does not match {schema_name}")
    validate_semantics(instance)


def validate_fixtures(validators: dict[str, Draft202012Validator]) -> tuple[int, int]:
    valid = read_json(ROOT / "contracts/examples/valid.json")
    invalid = read_json(ROOT / "contracts/examples/invalid.json")
    names: set[str] = set()
    for fixture in valid + invalid:
        require(fixture["name"] not in names, "Duplicate fixture name")
        names.add(fixture["name"])
    for fixture in valid:
        validate_instance(fixture["schema"], fixture["instance"], validators)
    for fixture in invalid:
        try:
            validate_instance(fixture["schema"], fixture["instance"], validators)
        except ContractError as error:
            require(error.category == fixture["expected_error"], "Unexpected rejection category")
        else:
            raise ContractError("fixture", f"Invalid fixture accepted: {fixture['name']}")
    return len(valid), len(invalid)


def source_files(directory: Path = ROOT):
    if (directory / ".git").exists():
        result = subprocess.run(
            ["git", "ls-files", "-z", "--cached", "--others", "--exclude-standard"],
            cwd=directory,
            check=True,
            capture_output=True,
        )
        for name in sorted(set(result.stdout.decode("utf-8").split("\0")) - {""}):
            path = directory / name
            if path.is_symlink() or any(parent.is_symlink() for parent in path.parents):
                raise ValueError("Repository checks do not follow symlinks")
            if path.is_file() and (path.suffix in TEXT_SUFFIXES or path.name in TEXT_NAMES):
                yield path
        return
    for path in sorted(directory.iterdir()):
        if path.is_symlink():
            raise ValueError("Repository checks do not follow symlinks")
        if path.is_dir():
            if path.name not in SKIP_DIRECTORIES:
                yield from source_files(path)
        elif path.suffix in TEXT_SUFFIXES or path.name in TEXT_NAMES:
            yield path


def validate_text_file(path: Path, root: Path = ROOT) -> None:
    data = path.read_bytes()
    if data.startswith(b"\xef\xbb\xbf") or b"\r" in data or not data.endswith(b"\n"):
        raise ValueError(f"Expected UTF-8 without BOM, LF and final newline: {path.name}")
    text = data.decode("utf-8")
    if any(line != line.rstrip() for line in text.splitlines()):
        raise ValueError(f"Trailing whitespace: {path.name}")
    if path.suffix == ".md":
        for target in MARKDOWN_LINK.findall(text):
            if "://" in target or target.startswith("#"):
                continue
            destination = (path.parent / target.split("#", 1)[0]).resolve()
            if not destination.is_relative_to(root.resolve()) or not destination.is_file():
                raise ValueError(f"Broken or external local link in {path.name}")
    elif path.suffix == ".json":
        read_json(path)
    elif path.suffix == ".toml" or path.name == "Cargo.lock":
        tomllib.loads(text)
    elif path.suffix == ".props":
        element_tree.fromstring(text)


def main() -> int:
    try:
        validators = load_validators()
        valid_count, invalid_count = validate_fixtures(validators)
        files = list(source_files())
        for path in files:
            validate_text_file(path)
    except (ValueError, OSError) as error:
        print(f"Validation failed: {error}", file=sys.stderr)
        return 1
    print(f"PASS: schemas={len(validators)}, valid={valid_count}, invalid={invalid_count}")
    print(f"PASS: {len(files)} text files; encoding, whitespace, links and structured files")
    print("Runtime IPC, cross-language compatibility and application performance are not tested.")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
