"""Smoke test for the lagom Python binding (SPEC.md §7.2).

Run against a wheel built by maturin and installed into the active environment
(see the `just py` recipe). Proves the binding imports and that the headline
transform works end-to-end through Python: a sealed policy drops a tool and a
pin prunes an argument from the projected schema.
"""

import json

import lagom


def test_import_exposes_surface():
    """The module exposes the four engine ops, the minter, the builder, and the
    shipped-skill helpers."""
    for name in (
        "project",
        "rewrite",
        "merge",
        "validate",
        "mint",
        "refire",
        "mint_stdio_server",
        "shipped_skills",
        "emit_skills",
    ):
        assert hasattr(lagom, name), f"missing {name}"
    assert hasattr(lagom, "PolicyBuilder")
    assert hasattr(lagom, "Guard")


def test_every_public_symbol_has_a_docstring():
    """Every public function and class — and every Guard/PolicyBuilder method —
    surfaces a non-empty Python __doc__ (so `help(lagom.project)` works). PyO3
    propagates the Rust `///` doc comments to __doc__; this guards against a
    future symbol shipping undocumented (docs full + current, NO DRIFT)."""
    functions = (
        "project",
        "rewrite",
        "merge",
        "validate",
        "mint",
        "refire",
        "mint_stdio_server",
        "shipped_skills",
        "emit_skills",
    )
    for name in functions:
        doc = getattr(lagom, name).__doc__
        assert doc and doc.strip(), f"{name} has no docstring"

    for cls_name, methods in (
        ("Guard", ("slim_defs", "gate", "policy")),
        (
            "PolicyBuilder",
            (
                "sealed",
                "keep",
                "drop_tool",
                "rename",
                "describe",
                "pin",
                "default",
                "constrain_enum",
                "constrain_range",
                "constrain_pattern",
                "build",
            ),
        ),
    ):
        cls = getattr(lagom, cls_name)
        assert cls.__doc__ and cls.__doc__.strip(), f"{cls_name} class has no docstring"
        for m in methods:
            doc = getattr(cls, m).__doc__
            assert doc and doc.strip(), f"{cls_name}.{m} has no docstring"


def test_mint_then_refire_round_trips():
    """mint narrows a base by a per-agent dynamic overlay into a persistable
    record; refire reproduces the recorded resolved projection (SPEC.md §8.2)."""
    base = json.dumps(
        {"default_presence": "keep", "tools": {"search": {"presence": "keep"}}}
    )
    dynamic = json.dumps(
        {"default_presence": "keep", "tools": {"search": {"presence": "drop"}}}
    )
    record = json.loads(
        lagom.mint("agent-7", base, dynamic, command="srv", args=["-y"])
    )
    assert record["run_id"] == "agent-7"
    assert record["resolved"]["policy"]["tools"]["search"]["presence"] == "drop"

    refired = json.loads(lagom.refire(json.dumps(record)))
    assert refired == record["resolved"], "refire reproduces the recorded projection"

    # A widening dynamic overlay is rejected (the sandbox enforcement).
    widening = json.dumps(
        {"default_presence": "keep", "tools": {"search": {"presence": "keep"}}}
    )
    sealed_base = json.dumps(
        {"default_presence": "keep", "tools": {"search": {"presence": "drop"}}}
    )
    try:
        lagom.mint("r", sealed_base, widening, command="srv")
    except ValueError:
        pass
    else:
        raise AssertionError("a widening dynamic overlay must raise")


def test_shipped_skills_bundled(tmp_path):
    """The binding bundles both shipped skills (SPEC.md §12) and emit_skills
    writes them with their `.skill.md` names and naming headings."""
    bundled = dict(lagom.shipped_skills())
    assert set(bundled) == {
        "lagom-slim-docs.skill.md",
        "lagom-dynamic-mint.skill.md",
    }

    written = lagom.emit_skills(str(tmp_path))
    assert len(written) == 2

    slim = (tmp_path / "lagom-slim-docs.skill.md").read_text()
    mint = (tmp_path / "lagom-dynamic-mint.skill.md").read_text()
    assert slim.startswith("# lagom-slim-docs")
    assert mint.startswith("# lagom-dynamic-mint")


def _upstream():
    return json.dumps(
        [
            {
                "name": "search",
                "description": "Search.",
                "input_schema": {
                    "type": "object",
                    "properties": {
                        "artifact": {"type": "string"},
                        "query": {"type": "string"},
                    },
                    "required": ["artifact", "query"],
                },
            },
            {
                "name": "write_file",
                "description": "Write a file.",
                "input_schema": {"type": "object", "properties": {}},
            },
        ]
    )


def test_project_drops_tool_and_pins_arg():
    """A sealed builder keeping `search` and pinning `artifact` drops
    `write_file` and removes the pinned property from the projected schema."""
    b = lagom.PolicyBuilder.sealed()
    b.keep("search")
    b.pin("search", "artifact", json.dumps("hylla"))
    policy = b.build()

    projected = json.loads(lagom.project(_upstream(), policy))

    names = [d["name"] for d in projected]
    assert names == ["search"], "write_file must be dropped"

    props = projected[0]["input_schema"]["properties"]
    assert "artifact" not in props, "pinned arg must be pruned from the schema"
    assert "query" in props, "unpinned arg must survive"


def test_rewrite_injects_pin():
    """rewrite injects the pinned value the agent never saw."""
    b = lagom.PolicyBuilder()
    b.pin("search", "artifact", json.dumps("hylla"))
    policy = b.build()

    upstream_call = json.loads(
        lagom.rewrite(
            json.dumps({"name": "search", "arguments": {"query": "x"}}), policy
        )
    )
    assert upstream_call["arguments"]["artifact"] == "hylla"


def test_merge_rejects_widening():
    """A widening overlay onto a sealed base raises (sandbox enforcement)."""
    base = lagom.PolicyBuilder.sealed().build()
    over = lagom.PolicyBuilder()
    over.keep("search")
    try:
        lagom.merge(base, over.build())
    except ValueError:
        return
    raise AssertionError("widening overlay must raise ValueError")


def test_validate_flags_drift():
    """validate fails loud on a reference to a tool the upstream lacks."""
    b = lagom.PolicyBuilder()
    b.pin("ghost", "x", "1")
    try:
        lagom.validate(b.build(), _upstream())
    except ValueError:
        return
    raise AssertionError("drift must raise ValueError")


_BRANDED_POLICY = json.dumps(
    {
        "default_presence": "drop",
        "tools": {
            "search": {
                "presence": "keep",
                "rename": "find",
                "description": {"override": "App find."},
                "args": {"artifact": {"pin": "hylla"}},
            }
        },
    }
)


def test_guard_one_call_helper():
    """The brandable Guard wires a slim MCP in two calls: slim_defs() carries the
    app's OWN tool name + description (lagom invisible) with the rest dropped and
    the pin hidden, and gate() maps the branded name back, injects the pin, and
    rejects a dropped tool — without the app touching project/rewrite."""
    g = lagom.Guard(_upstream(), _BRANDED_POLICY)

    defs = json.loads(g.slim_defs())
    assert [d["name"] for d in defs] == ["find"], "branded name, rest dropped"
    assert defs[0]["description"] == "App find.", "app's own description"
    props = defs[0]["input_schema"]["properties"]
    assert "artifact" not in props, "pinned arg hidden"
    assert "query" in props

    gated = json.loads(g.gate(json.dumps({"name": "find", "arguments": {"query": "x"}})))
    assert gated["name"] == "search", "branded name maps back to upstream"
    assert gated["arguments"]["artifact"] == "hylla", "pin injected"

    for bad in ("write_file", "search"):
        try:
            g.gate(json.dumps({"name": bad, "arguments": {}}))
        except ValueError:
            continue
        raise AssertionError(f"gate must reject {bad!r}")


def test_mint_stdio_server_accepts_optional_audit(tmp_path):
    """mint_stdio_server exposes the optional `audit`/`run_id` keyword args
    (mirroring the CLI's --audit/--run-id). Opening a log under a missing
    directory must raise ValueError before any upstream is spawned, proving the
    audit path is wired and fails loud (SPEC.md §8.2, §9.3)."""
    policy = lagom.PolicyBuilder().build()
    missing = tmp_path / "no-such-dir" / "audit.jsonl"
    assert not missing.parent.exists()
    try:
        lagom.mint_stdio_server(
            policy,
            "this-upstream-is-never-spawned",
            audit=str(missing),
            run_id="smoke-run",
        )
    except ValueError:
        return
    raise AssertionError("opening an audit log under a missing dir must raise")
