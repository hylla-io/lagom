"""Parity runner -- Python (PyO3) face of the cross-binding NO-DRIFT guard.

Reads the shared fixture (parity/fixtures/cases.json), runs every case through
the compiled `lagom` extension module, and prints a deterministic result map to
stdout:

    {"<case>": {"status": "ok"|"err", "payload": <json-string|null>}, ...}

`payload` for an ok case is the exact JSON string the binding returned (the byte
string every face must match); for an err case it is null (the binding raises
`ValueError`, whose message framing legitimately differs from the other faces,
so the guard compares only the ok/err classification for errors). The comparator
diffs this against the Rust reference.

Usage: python run.py <fixture-path>
"""

import json
import sys

import lagom


def is_null(value):
    """A `None`/absent or JSON-null overlay means no overlay (parity)."""
    return value is None


def mint_record(case):
    """Mint a record from a mint case via the binding, returning its JSON.

    Mirrors the other faces: base + optional dynamic overlay + upstream command.
    """
    dynamic = case.get("dynamic")
    dynamic_json = None if is_null(dynamic) else json.dumps(dynamic)
    cmd = case["upstream_command"]
    return lagom.mint(
        case["run_id"],
        json.dumps(case["base"]),
        dynamic_json,
        cmd.get("command", ""),
        cmd.get("args", []),
        cmd.get("env", []),
    )


def run_case(case, by_name, upstream_json):
    """Run one fixture case through the Python face -> a result dict."""
    op = case["op"]
    try:
        if op == "project":
            return ok(lagom.project(upstream_json, json.dumps(case["policy"])))
        if op == "rewrite":
            return ok(lagom.rewrite(json.dumps(case["call"]), json.dumps(case["policy"])))
        if op == "merge":
            return ok(lagom.merge(json.dumps(case["base"]), json.dumps(case["overlay"])))
        if op == "validate":
            lagom.validate(json.dumps(case["policy"]), upstream_json)
            # Match the canonical face: validate ok serializes as the JSON null.
            return ok("null")
        if op == "mint":
            return ok(mint_record(case))
        if op == "refire":
            # An inline `record` is the on-disk shape refire reads back, passed
            # whole so a refusal on the mint shapes is compared; `mint_of`
            # reuses a named mint case's record, which is always well-formed.
            if case.get("record") is not None:
                record = json.dumps(case["record"])
            else:
                record = mint_record(by_name[case["mint_of"]])
            return ok(lagom.refire(record))
    except ValueError:
        return {"status": "err", "payload": None}
    raise SystemExit(f"unknown op {op!r}")


def ok(payload):
    return {"status": "ok", "payload": payload}


def main():
    path = sys.argv[1] if len(sys.argv) > 1 else "parity/fixtures/cases.json"
    with open(path, encoding="utf-8") as fh:
        fixture = json.load(fh)

    upstream_json = json.dumps(fixture["upstream"])
    cases = fixture["cases"]
    by_name = {c["name"]: c for c in cases}

    out = {c["name"]: run_case(c, by_name, upstream_json) for c in cases}
    # Sorted keys so the layout matches the other faces (the comparator parses
    # structurally, but a stable order keeps a manual diff readable).
    print(json.dumps(out, indent=2, sort_keys=True))


if __name__ == "__main__":
    main()
