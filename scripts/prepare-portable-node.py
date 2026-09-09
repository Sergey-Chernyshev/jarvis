#!/usr/bin/env python3
"""Materialize the installer's embedded node package, without contacting a host."""
import argparse
import pathlib
import re


def prepare(repo, output):
    if output.exists() and any(output.iterdir()):
        raise ValueError("portable output must be an empty directory")
    source = repo / "src-tauri/src/install/remote.rs"
    text = source.read_text()
    match = re.search(r"const NODE_SRC: \[\(&str, &str\); (\d+)\] = \[(.*?)\n\];", text, re.S)
    if match is None:
        raise ValueError("embedded node source inventory was not found")
    entries = re.findall(r'\("([^"]+)", include_str!\("([^"]+)"\)\)', match[2])
    if len(entries) != int(match[1]):
        raise ValueError("embedded node inventory contains unsupported entries")
    seen = set()
    for relative, include in entries:
        path = pathlib.PurePosixPath(relative)
        if path.is_absolute() or ".." in path.parts or relative in seen:
            raise ValueError("unsafe or duplicate portable path")
        seen.add(relative)
        original = (source.parent / include).resolve(strict=True)
        original.relative_to(repo.resolve())
        body = original.read_text()
        if relative == "Cargo.toml":
            old = 'jarvis-node-shared = { path = "../shared" }'
            if body.count(old) != 1:
                raise ValueError("node shared dependency layout changed")
            body = body.replace(old, 'jarvis-node-shared = { path = "shared" }')
        target = output / relative
        target.parent.mkdir(parents=True, exist_ok=True)
        target.write_text(body)


if __name__ == "__main__":
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--out", type=pathlib.Path, required=True)
    args = parser.parse_args()
    prepare(pathlib.Path(__file__).resolve().parent.parent, args.out)
