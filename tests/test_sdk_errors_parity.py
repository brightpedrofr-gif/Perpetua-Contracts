"""Parity: tools/sdk/errors.ts and the golden fixture match contracts/stream/src/error.rs."""
import json
import re
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent


def _rust():
    src = (ROOT / "contracts/stream/src/error.rs").read_text()
    return {int(n): name for name, n in re.findall(r"^\s+([A-Za-z]+) = (\d+),", src, re.M)}


def _ts():
    src = (ROOT / "tools/sdk/errors.ts").read_text()
    return {int(n): m for n, m in re.findall(r'^\s+(\d+):\s+"([A-Za-z]+):', src, re.M)}


def test_ts_matches_rust():
    assert _ts() == _rust()


def test_fixture_matches_rust():
    fx = json.loads((ROOT / "tests/fixtures/errors/stream_errors.json").read_text())
    assert {int(k): v for k, v in fx["errors"].items()} == _rust()
    assert fx["max_code"] == max(_rust())
