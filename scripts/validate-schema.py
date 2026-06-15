#!/usr/bin/env python3
"""Validate the repo's config files against the published JSON Schema.

A drift guard the Rust suite cannot cheaply provide: it proves the committed
`schema/allowlister.schema.json` actually accepts every config the project ships
(the examples, the recommended profiles, and the repo's own dogfood config) and
is itself a valid draft 2020-12 schema. A schema that grows too strict — say an
`additionalProperties: false` that rejects a field the loader accepts — fails
here. Run by CI; locally: `pip install jsonschema && python3 scripts/validate-schema.py`.

Exits non-zero on the first schema or instance error.
"""

from __future__ import annotations

import json
import sys
from pathlib import Path

try:
    from jsonschema import Draft202012Validator
except ImportError:  # pragma: no cover - surfaced as a clear CI message
    sys.exit("error: this check needs the `jsonschema` package (pip install jsonschema)")

ROOT = Path(__file__).resolve().parent.parent
SCHEMA_PATH = ROOT / "schema" / "allowlister.schema.json"


def strip_jsonc_comments(text: str) -> str:
    """Blank `//` and `/* */` comments, leaving comment markers inside strings
    untouched. Mirrors the loader's own pre-parse step so a commented config
    validates as the JSON it really is."""
    out: list[str] = []
    i, n = 0, len(text)
    in_str = esc = False
    while i < n:
        c = text[i]
        if in_str:
            out.append(c)
            if esc:
                esc = False
            elif c == "\\":
                esc = True
            elif c == '"':
                in_str = False
            i += 1
        elif c == '"':
            in_str = True
            out.append(c)
            i += 1
        elif c == "/" and i + 1 < n and text[i + 1] == "/":
            while i < n and text[i] != "\n":
                i += 1
        elif c == "/" and i + 1 < n and text[i + 1] == "*":
            i += 2
            while i + 1 < n and not (text[i] == "*" and text[i + 1] == "/"):
                i += 1
            i += 2
        else:
            out.append(c)
            i += 1
    return "".join(out)


def config_files() -> list[Path]:
    """Every config the project ships, plus its own dogfood config — all of
    which the published schema must accept."""
    files = sorted(ROOT.glob("examples/**/*.json")) + sorted(
        ROOT.glob("examples/**/*.jsonc")
    )
    dogfood = ROOT / ".allowlister.jsonc"
    if dogfood.exists():
        files.append(dogfood)
    return files


def main() -> int:
    schema = json.loads(SCHEMA_PATH.read_text())
    Draft202012Validator.check_schema(schema)
    print(f"ok   schema is a valid draft 2020-12 schema ({SCHEMA_PATH.name})")
    validator = Draft202012Validator(schema)

    failed = False
    for path in config_files():
        data = json.loads(strip_jsonc_comments(path.read_text()))
        errors = sorted(validator.iter_errors(data), key=lambda e: list(e.path))
        rel = path.relative_to(ROOT)
        if errors:
            failed = True
            print(f"FAIL {rel}")
            for err in errors[:10]:
                where = "/".join(str(p) for p in err.path) or "<root>"
                print(f"     at {where}: {err.message}")
        else:
            print(f"ok   {rel}")

    if failed:
        print("\nSchema drift: a shipped config no longer validates against the schema.")
        return 1
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
