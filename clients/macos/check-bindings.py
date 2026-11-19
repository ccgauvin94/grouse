#!/usr/bin/env python3
"""Guards the two macOS invariants that CAN be checked without a Mac.

This host is Linux, and the account has no macOS CI runners (a private repo cannot use
them for free — the job is cancelled before it starts). So the Swift itself is compiled on
the Mac by hand, and this script covers what does not need one:

1. **The bindings still generate**, and still generate Swift. `uniffi-bindgen-swift` reads
   the metadata out of a host build, so a change to the core's public surface that breaks
   Swift generation is caught here rather than on the Mac. It needs a DEBUG build (the
   release profile strips the metadata sections bindgen reads) — the caller builds it.

2. **`ListenerDefaults.swift` keeps up with the generated protocol.** `CoreListener` has a
   requirement per core event and no defaults in the generated Swift, so
   `Sources/GrouseCore/ListenerDefaults.swift` stubs each one. Add an event to
   `CoreListener`, forget the default, and the failure shows up as a build error in an app
   that only runs on a Mac — with no CI anywhere to blame. This is that CI.

Usage (from anywhere in the repo):
    clients/macos/check-bindings.py <path-to-libgrouse_core.dylib|.so>

Exit 0 clean; 1 on a broken invariant. Prints what it found either way.
"""

import re
import subprocess
import sys
import tempfile
from pathlib import Path

PKG = Path(__file__).resolve().parent
REPO = PKG.parents[1]
DEFAULTS = PKG / "Sources" / "GrouseCore" / "ListenerDefaults.swift"


def generate(library: Path, out: Path) -> Path:
    """Run the generator; return the generated grouse_core.swift."""
    subprocess.run(
        [
            "uniffi-bindgen-swift",
            "--swift-sources",
            str(library),
            str(out),
        ],
        check=True,
        cwd=REPO / "core",
    )
    swift = out / "grouse_core.swift"
    if not swift.is_file():
        sys.exit(f"FAIL: the generator produced no grouse_core.swift in {out}")
    return swift


def protocol_methods(swift: Path) -> dict[str, str]:
    """The `func on…` requirements of the generated `CoreListener` protocol, as
    name → its full signature (so a failure can print the line to paste)."""
    text = swift.read_text()
    start = text.find("public protocol CoreListener")
    if start < 0:
        sys.exit("FAIL: no `public protocol CoreListener` in the generated bindings")
    # The protocol body ends at the first line that is exactly "}"
    body_end = text.find("\n}", start)
    if body_end < 0:
        sys.exit("FAIL: unterminated CoreListener protocol")
    body = text[start:body_end]
    out: dict[str, str] = {}
    for line in body.splitlines():
        match = re.match(r"^\s*func (on\w+\([^\n]*\))", line)
        if match:
            name = re.match(r"(on\w+)\(", match.group(1)).group(1)
            out[name] = match.group(1).strip()
    return out


def defaults_methods(path: Path) -> set[str]:
    """The `func on…` bodies supplied by the protocol extension."""
    if not path.is_file():
        sys.exit(f"FAIL: {path} is missing — every conformance would have to stub all 23")
    return set(re.findall(r"^\s*func (on\w+)\(", path.read_text(), re.MULTILINE))


def main() -> int:
    if len(sys.argv) != 2:
        sys.exit(__doc__)
    library = Path(sys.argv[1]).resolve()
    if not library.is_file():
        sys.exit(f"FAIL: no library at {library} (build one: cargo build -p grouse-core)")

    # The temporary dir is gone by the time anything prints, so everything the report
    # needs is captured here.
    with tempfile.TemporaryDirectory() as tmp:
        generated = generate(library, Path(tmp))
        required = protocol_methods(generated)

    supplied = defaults_methods(DEFAULTS)
    missing = sorted(set(required) - supplied)
    stale = sorted(supplied - set(required))

    print(f"generated Swift: grouse_core.swift (in a temp dir, discarded)")
    print(f"CoreListener requirements: {len(required)}")
    print(f"ListenerDefaults.swift supplies: {len(supplied)}")

    if missing:
        print("\nFAIL — a conformance in the app would not compile. Add these to")
        print(f"  {DEFAULTS.relative_to(REPO)}:")
        for name in missing:
            print(f"    func {required[name]}")
        return 1

    if stale:
        # Not a failure: an extra default is dead code, not a broken build. Say so, because
        # the next person to read the file will wonder whether it is still used.
        print("\nnote — defaults with no matching requirement (dead, safe to delete):")
        for name in stale:
            print(f"    {name}")

    print("\nOK — the bindings generate and every listener event has a default.")
    return 0


if __name__ == "__main__":
    sys.exit(main())
