#!/usr/bin/env python3
"""Guards the invariants that CAN be checked without a Mac.

This host is Linux, and the account has no macOS CI runners (a private repo cannot use
them for free — the job is cancelled before it starts). So the Swift itself is compiled on
the Mac by hand, and this script covers what does not need one:

1. **The bindings still generate**, and still generate Swift. `uniffi-bindgen-swift` reads
   the metadata out of a host build, so a change to the core's public surface that breaks
   Swift generation is caught here rather than on the Mac. It needs a DEBUG build (the
   release profile strips the metadata sections bindgen reads) — the caller builds it.

2. **Both listener protocols still have defaults.** Each `CoreListener` and
   `GrouseUnstableListener` event is a REQUIRED method in the generated Swift, so
   `ListenerDefaults.swift` / `UnstableListenerDefaults.swift` stub them. A new event —
   or a changed signature such as adding `owner`/`requestId` to a form callback — must be
   added to the right file or the app breaks on a Mac where no CI compiles it. SIGNATURES
   are compared, not just names: a same-named method with a different arity is exactly the
   drift that used to pass this check and fail at compile time.

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

# Generated protocol -> file whose protocol extension supplies the no-op defaults.
PROTOCOLS = {
    "CoreListener": PKG / "Sources" / "GrouseCore" / "ListenerDefaults.swift",
    "GrouseUnstableListener": PKG / "Sources" / "GrouseCore" / "UnstableListenerDefaults.swift",
}


def generate(library: Path, out: Path) -> str:
    """Run the generator; return the generated grouse_core.swift text."""
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
        sys.exit("FAIL: the generator produced no grouse_core.swift in the temp dir")
    return swift.read_text()


def _body_after(text: str, marker: str) -> str:
    """The balanced `{...}` body following `marker` (protocol or extension)."""
    start = text.find(marker)
    if start < 0:
        sys.exit(f"FAIL: no `{marker}` in the Swift being checked")
    brace = text.find("{", start)
    if brace < 0:
        sys.exit(f"FAIL: unterminated `{marker}`")
    depth = 0
    for i in range(brace, len(text)):
        if text[i] == "{":
            depth += 1
        elif text[i] == "}":
            depth -= 1
            if depth == 0:
                return text[brace + 1 : i]
    sys.exit(f"FAIL: unbalanced `{marker}`")


def methods(text: str, marker: str) -> dict[str, set[str]]:
    """`func on…(...)` declarations, as name -> SET of normalized full signatures.

    A set because a conformance may deliberately carry overloads (the form callbacks
    exist with both `String` and `SessionOwner` owner shapes while the core bindings are
    regenerated). Signatures may span lines, so collect until the parens balance.
    """
    out: dict[str, set[str]] = {}
    body = _body_after(text, marker)
    lines = iter(body.splitlines())
    for line in lines:
        match = re.match(r"^\s*func\s+(on\w+)\s*\(", line)
        if not match:
            continue
        name = match.group(1)
        chunks = [line.strip()]
        depth = line.count("(") - line.count(")")
        while depth > 0:
            try:
                chunks.append(next(lines).strip())
            except StopIteration:
                break
            depth += chunks[-1].count("(") - chunks[-1].count(")")
        out.setdefault(name, set()).add(normalize(" ".join(chunks)))
    return out


def normalize(signature: str) -> str:
    """Compare shapes, not formatting or a generator's `Swift.` type prefix."""
    text = re.sub(r"\s+", "", signature)
    text = text.replace("Swift.", "")
    text = re.sub(r"->Void$", "", text)
    text = re.sub(r"->\(\)$", "", text)
    return text


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

    failed = False
    for protocol, defaults_path in PROTOCOLS.items():
        if not defaults_path.is_file():
            print(f"FAIL: {defaults_path.relative_to(REPO)} is missing — every conformance "
                  f"of {protocol} would have to stub every event")
            failed = True
            continue
        required = methods(generated, f"public protocol {protocol}")
        supplied = methods(defaults_path.read_text(), f"public extension {protocol}")
        missing = sorted(set(required) - set(supplied))
        stale = sorted(set(supplied) - set(required))
        wrong = sorted(name for name in set(required) & set(supplied)
                       if not (required[name] & supplied[name]))

        print(f"{protocol}: {len(required)} requirements, {len(supplied)} defaults")
        if missing:
            print(f"\nFAIL — a conformance of {protocol} would not compile. Add these to")
            print(f"  {defaults_path.relative_to(REPO)}:")
            for name in missing:
                for signature in sorted(required[name]):
                    print(f"    {signature}")
            failed = True
        if wrong:
            print(f"\nFAIL — signature drift in {defaults_path.relative_to(REPO)}:")
            for name in wrong:
                for signature in sorted(required[name]):
                    print(f"    generated: {signature}")
                for signature in sorted(supplied[name]):
                    print(f"    default:   {signature}")
            failed = True
        if stale:
            # Not a failure: an extra default is dead code, not a broken build. Say so,
            # because the next reader will wonder whether it is still used.
            print(f"\nnote — {protocol} defaults with no matching requirement "
                  "(dead, safe to delete):")
            for name in stale:
                for signature in sorted(supplied[name]):
                    print(f"    {signature}")

    if failed:
        return 1
    print("\nOK — the bindings generate and every listener event has a matching default.")
    return 0


if __name__ == "__main__":
    sys.exit(main())
