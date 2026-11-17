#!/usr/bin/env python3
"""Fail if a match arm label appears twice in the same match.

Rust already rejects unreachable arms, but only when the arm bodies are shaped so the
compiler can see it — and only after the compile that the author is trying to avoid
running. Three edits in a row left duplicate `fill` / `insert-column` / `delete-where`
arms in serve.rs; this catches that in the sweep, in a second.
"""
import os
import re
import sys

# Resolve against the repository, not the caller's working directory: the sweep runs
# from a temporary directory, and a relative path there points at nothing.
ROOT = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
TARGET = os.path.join(ROOT, "crates/cli/src/serve.rs")
ARM = re.compile(r'^        ("[a-z0-9\-]+")( \| ("[a-z0-9\-]+"))? =>')

def main() -> int:
    text = open(TARGET, encoding="utf-8").read()
    lines = text.split("\n")
    problems = []
    start = None
    for index, line in enumerate(lines):
        if line.startswith("    let op = match ") or line.startswith("    match action.as_str()"):
            start = index
            seen = {}
            for i in range(index, len(lines)):
                candidate = ARM.match(lines[i])
                if candidate:
                    label = lines[i].split("=>")[0].strip()
                    seen.setdefault(label, []).append(i + 1)
                if lines[i].strip() == "};" and i > index + 5:
                    break
            for label, at in seen.items():
                if len(at) > 1:
                    problems.append(
                        f"{os.path.relpath(TARGET, ROOT)}: duplicate arm {label} at lines {at}"
                    )
    if problems:
        print("\n".join(problems))
        return 1
    print("  ok   no duplicate match arms in serve.rs")
    return 0

if __name__ == "__main__":
    sys.exit(main())
