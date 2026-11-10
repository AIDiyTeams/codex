"""Assemble and verify the environment's static catalog using the release executable."""

import argparse
import hashlib
import json
import os
import re
import subprocess
import tempfile
from pathlib import Path

from probe import verify_threads

SOURCES = Path(__file__).resolve().parent


def unique_pairs(pairs):
    result = {}
    for key, value in pairs:
        if key in result:
            raise ValueError(f"duplicate JSON key: {key}")
        result[key] = value
    return result


def load(path):
    return json.loads(path.read_bytes(), object_pairs_hook=unique_pairs)


def by_slug(payload):
    rows = payload["models"]
    result = {row["slug"]: row for row in rows}
    if not result or len(result) != len(rows) or not all(result):
        raise ValueError("catalog requires unique nonempty slugs")
    return result


def digest(path):
    checksum = hashlib.sha256()
    with path.open("rb") as stream:
        for block in iter(lambda: stream.read(1024 * 1024), b""):
            checksum.update(block)
    return checksum.hexdigest()


def dump_binary(binary, catalog=None):
    args = [str(binary)]
    if catalog:
        args.extend(["-c", "model_catalog_json=" + json.dumps(str(catalog.resolve()))])
    with tempfile.TemporaryDirectory() as home:
        env = {**os.environ, "CODEX_HOME": home}
        args.extend(["debug", "models"])
        if not catalog:
            args.append("--bundled")
        return by_slug(
            json.loads(subprocess.check_output(args, env=env, cwd=home, timeout=45))
        )


def assemble(bundled, sources, aliases, routes):
    models = dict(bundled)
    for alias, canonical in aliases.items():
        descriptor = bundled.get(canonical) or sources.get(canonical)
        if not alias or not descriptor:
            raise ValueError(f"missing descriptor for {alias!r} -> {canonical!r}")
        entry = {**descriptor, "slug": alias}
        if alias in bundled and bundled[alias] != entry:
            raise ValueError(f"alias collides with bundled model: {alias}")
        models[alias] = entry
    missing = sorted(set(routes) - models.keys())
    if missing:
        raise ValueError(
            "selectable routes without exact metadata: " + ", ".join(missing)
        )
    return models


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--codex", type=Path, required=True)
    parser.add_argument("--commit", required=True)
    parser.add_argument("--routes", type=Path, required=True)
    parser.add_argument(
        "--output",
        type=Path,
        required=True,
        help="catalog JSON; receipt is written alongside with .receipt suffix",
    )
    args = parser.parse_args()
    if not re.fullmatch("[a-f0-9]{40}", args.commit):
        parser.error("exact source commit required")
    routes = load(args.routes)
    if (
        not isinstance(routes, list)
        or not routes
        or not all(isinstance(r, str) and r.strip() == r and r for r in routes)
        or len(set(routes)) != len(routes)
    ):
        raise ValueError("invalid route inventory")
    sources = {}
    for name in ("openai", "deepseek", "kimi"):
        snapshot = load(SOURCES / (name + ".json"))
        entries = by_slug(snapshot)
        if not snapshot["source"] or sources.keys() & entries.keys():
            raise ValueError(f"missing provenance or overlapping descriptors: {name}")
        sources.update(entries)
    binary = args.codex.resolve()
    bundled = dump_binary(binary)
    models = assemble(bundled, sources, load(SOURCES / "aliases.json"), routes)
    args.output.write_text(
        json.dumps({"models": list(models.values())}, ensure_ascii=False) + "\n"
    )
    parsed = dump_binary(binary, args.output)
    if models.keys() != parsed.keys() or any(
        parsed[r].get("used_fallback_model_metadata") for r in routes
    ):
        raise ValueError("release executable could not load the complete catalog")
    # Codex may discard unrecognised fields from a newer upstream snapshot.
    # Compare aliases to the same descriptor as interpreted by this binary.
    for alias, canonical in load(SOURCES / "aliases.json").items():
        if canonical in parsed and parsed[alias] != {
            **parsed[canonical],
            "slug": alias,
        }:
            raise ValueError(f"parsed alias differs from canonical model: {alias}")
    resolution = verify_threads(binary, args.output, routes)
    receipt = {
        "resolution": resolution,
        "commit": args.commit,
        "binarySHA256": digest(binary),
        "catalogSHA256": digest(args.output),
        "bundled": len(bundled),
        "total": len(parsed),
        "routes": sorted(routes),
    }
    Path(str(args.output) + ".receipt").write_text(json.dumps(receipt, indent=2) + "\n")
    print(json.dumps(receipt))


if __name__ == "__main__":
    main()
