#!/usr/bin/env python3
"""The parity ratchet: no client may grow a surface the others have not.

Why this exists
---------------
The core is shared, but the *clients* are not: the phone reads uniffi records,
the desktop reads JSON, and neither compiler can see the other. So when the core
grew `Item::files`, the phone rendered attachment cards and the desktop silently
had no idea the files existed — for a release, with every test green.

What it checks
--------------
1. The inventory in `core/grouse-core/src/surface.rs` is the source of truth
   (it is itself compile-guarded: an added field breaks that build). Every id in
   it must carry a disposition in `docs/PARITY.md` for every ACTIVE client.
2. An active client is discovered from the tree, not from a list kept here: a
   `clients/<name>` directory that contains sources is active, and demands a
   column. That is how `clients/cli` will join the ratchet the day it has code
   instead of a README, with no edit to this file.
3. A `yes:` disposition must cite an anchor — `file#token` — and the token must
   really appear in that file. "Implemented" is therefore evidence, not a claim.
4. An `excluded:` disposition needs a reason, and it moves a PINNED counter in
   the ledger. The count can only change by editing the number, which is a line
   a reviewer cannot miss. That is the ratchet: exclusions can shrink freely and
   grow only on purpose.
5. `LISTENER_EVENTS` in surface.rs must equal the `CoreListener` trait's
   callbacks, parsed out of `lib.rs`. The trait has defaults, so an added
   `fn on_*` compiles without touching the inventory; this is the check that
   closes that hole.

Usage
-----
    python3 scripts/check-parity.py              # check (CI)
    python3 scripts/check-parity.py --self-test  # prove the checks still bite
    python3 scripts/check-parity.py --report     # the ledger as a table

Exit codes: 0 ok, 1 violations, 2 the checker itself could not run (a parse
failure is never a pass).
"""

from __future__ import annotations

import argparse
import pathlib
import re
import sys
import tempfile

ROOT = pathlib.Path(__file__).resolve().parent.parent
SURFACE_RS = "core/grouse-core/src/surface.rs"
LIB_RS = "core/grouse-core/src/lib.rs"
LEDGER = "docs/PARITY.md"

# A client is ACTIVE when its directory holds at least one file with these
# suffixes (build/output dirs are walked past). Nothing else decides it.
CLIENT_SUFFIXES = {
    "android": (".kt",),
    "desktop": (".qml", ".cpp", ".h"),
    "cli": (".rs",),
}
CLIENT_SKIP_DIRS = {"build", "target", ".git", "node_modules", "bindings"}

MIN_REASON = 12
PLACEHOLDERS = ("todo", "tbd", "fixme", "n/a", "later", "?")


# --------------------------------------------------------------------------
# Source-of-truth parsing
# --------------------------------------------------------------------------
def read(rel: str) -> str:
    return (ROOT / rel).read_text(encoding="utf-8")


def parse_inventory(text: str) -> dict:
    """The inventory, from `surface.rs`.

    Two shapes: `LISTENER_EVENTS` is a literal `pub static … = &["listener.x"]`,
    and the record/enum groups are emitted by a macro whose CALL carries the
    field names (`record_surface!(…, "item": id, kind, …)`). The compiler sees
    the expansion; this sees the call — which is the same list the compile-time
    guard destructures, so the two cannot disagree.
    """
    groups = {}

    for name, body in re.findall(
        r"pub static ([A-Z_]+): &\[&str\] = &\[(.*?)\];", text, re.S
    ):
        if name != "LISTENER_EVENTS":
            continue  # macro-generated consts have no literal text; see below
        ids = re.findall(r'"(listener\.[a-z_]+)"', body)
        if not ids:
            raise SystemExit(f"check-parity: {name} parsed as empty in {SURFACE_RS}")
        groups[name] = ids

    for _macro, args in re.findall(
        r"\b(record_surface|struct_enum_surface|unit_enum_surface)!\s*\((.*?)\);",
        text,
        re.S,
    ):
        # Comments inside the invocation are documentation, not names.
        body = "\n".join(
            line for line in args.splitlines() if not line.strip().startswith("//")
        )
        m = re.search(r'"([a-z_]+)"\s*:', body)
        if not m:
            raise SystemExit(f"check-parity: a surface macro call has no \"prefix\": {body[:60]}")
        prefix = m.group(1)
        head, tail = body[: m.start()], body[m.end():]
        parts = [p.strip() for p in head.split(",") if p.strip()]
        if len(parts) != 2:
            raise SystemExit(f"check-parity: expected `<type>, <CONST>, \"prefix\":` in {body[:80]}")
        name = parts[1]
        names = [n for n in re.findall(r"\b([A-Za-z_][A-Za-z0-9_]*)\b", tail)]
        if not names:
            raise SystemExit(f"check-parity: {name} has no names in its macro call")
        groups[name] = [f"{prefix}.{n}" for n in names]

    if not groups:
        raise SystemExit(f"check-parity: no inventory found in {SURFACE_RS}")
    return groups


def parse_listener_trait(text: str) -> list:
    """The `on_*` callbacks declared by `pub trait CoreListener`."""
    start = text.find("pub trait CoreListener")
    if start < 0:
        raise SystemExit(f"check-parity: `pub trait CoreListener` not found in {LIB_RS}")
    end = text.find("\n}\n", start)
    if end < 0:
        raise SystemExit("check-parity: unterminated CoreListener trait")
    body = text[start:end]
    return re.findall(r"\n\s*fn (on_[a-z_]+)\s*\(", body)


# --------------------------------------------------------------------------
# Ledger parsing — docs/PARITY.md
# --------------------------------------------------------------------------
CELL_YES = "yes:"
CELL_EXCLUDED = "excluded:"


def parse_cell(text: str) -> dict:
    raw = text.strip().strip("`").strip()
    if not raw or raw == "-":
        return {"kind": "missing"}
    if raw.startswith(CELL_YES):
        anchors = []
        for part in raw[len(CELL_YES):].split("+"):
            part = part.strip()
            if not part:
                continue
            if "#" not in part:
                return {"kind": "bad-anchor", "raw": raw}
            path, token = part.split("#", 1)
            anchors.append((path.strip(), token.strip()))
        if not anchors:
            return {"kind": "bad-anchor", "raw": raw}
        return {"kind": "yes", "anchors": anchors}
    if raw.startswith(CELL_EXCLUDED):
        return {"kind": "excluded", "reason": raw[len(CELL_EXCLUDED):].strip()}
    return {"kind": "bad-syntax", "raw": raw}


def parse_ledger(text: str) -> dict:
    pins = {k: int(v) for k, v in re.findall(r"^([a-z_]+) = (\d+)\s*$", text, re.M)}
    roots = dict(re.findall(r"^([a-z]+) root = (.+?)\s*$", text, re.M))

    columns, rows = [], []
    seen_header = False
    for line in text.splitlines():
        s = line.strip()
        if not s.startswith("|"):
            continue
        cells = [c for c in s.strip("|").split("|")]
        if not seen_header:
            columns = [c.strip().strip("`") for c in cells]
            seen_header = True
            continue
        if all(re.fullmatch(r":?-{2,}:?", c.strip()) for c in cells):
            continue
        if len(cells) != len(columns):
            raise SystemExit(
                f"check-parity: {LEDGER} row has {len(cells)} cells, header {len(columns)}:"
                f" {s[:80]}"
            )
        row = {"id": cells[0].strip().strip("`"), "raw": s}
        for client, cell in zip(columns[1:], cells[1:]):
            row[client] = parse_cell(cell)
        rows.append(row)
    if not columns or columns[0] != "surface":
        raise SystemExit(f"check-parity: no `| surface |` header table in {LEDGER}")
    return {"pins": pins, "roots": roots, "clients": columns[1:], "rows": rows}


def active_clients() -> dict:
    """`clients/<name>` → [(relative path, …)] for every directory holding sources."""
    out = {}
    base = ROOT / "clients"
    for client_dir in sorted(p for p in base.iterdir() if p.is_dir()):
        suffixes = CLIENT_SUFFIXES.get(client_dir.name)
        if not suffixes:
            continue
        files = []
        for path in client_dir.rglob("*"):
            if not path.is_file() or path.suffix not in suffixes:
                continue
            if CLIENT_SKIP_DIRS & set(path.relative_to(client_dir).parts):
                continue
            files.append(str(path.relative_to(ROOT)))
        if files:
            out[client_dir.name] = files
    return out


# --------------------------------------------------------------------------
# The checks
# --------------------------------------------------------------------------
def validate(core_ids: list, clients: list, rows: list, pins: dict) -> list:
    """Every structural rule, on data alone (no filesystem) — so --self-test
    can feed it a broken ledger and prove the rule still fires."""
    errors = []
    core_set = set(core_ids)
    seen = set()

    for row in rows:
        rid = row["id"]
        if rid in seen:
            errors.append(f"duplicate ledger row: {rid}")
        seen.add(rid)
        # `ui.*` rows are hand-declared, free-form, and never required; every
        # other id must exist in the core inventory, or the ledger has rotted.
        if not rid.startswith("ui.") and rid not in core_set:
            errors.append(f"ledger row is not in the core surface: {rid}")
        for client in clients:
            cell = row.get(client, {"kind": "missing"})
            kind = cell["kind"]
            if kind == "missing":
                errors.append(f"{rid}: no disposition for {client}")
            elif kind == "bad-syntax":
                errors.append(
                    f"{rid}/{client}: `{cell['raw']}` — use `yes: file#token`,"
                    f" `yes: a#t + b#t` or `excluded: reason`"
                )
            elif kind == "bad-anchor":
                errors.append(f"{rid}/{client}: `{cell['raw']}` is not `file#token`")
            elif kind == "excluded":
                reason = cell["reason"]
                low = reason.lower()
                first = re.split(r"[\s,:;.\u2014-]+", low, maxsplit=1)[0]
                if len(reason) < MIN_REASON:
                    errors.append(f"{rid}/{client}: exclusion needs a real reason (>={MIN_REASON} chars)")
                elif first in PLACEHOLDERS:
                    errors.append(f"{rid}/{client}: placeholder exclusion `{reason}`")

    for cid in sorted(core_set - seen):
        errors.append(f"core surface has no ledger row: {cid}")

    total_excluded = sum(
        1 for row in rows for client in clients if row.get(client, {}).get("kind") == "excluded"
    )
    pinned = pins.get("excluded")
    if pinned is None:
        errors.append(f"{LEDGER} is missing the `excluded = N` pin")
    elif pinned != total_excluded:
        verb = "grew" if total_excluded > pinned else "shrank"
        errors.append(
            f"the exclusion count {verb}: {pinned} pinned, {total_excluded} present."
            f" That is the ratchet — {'justify it and bump `excluded = {total_excluded}`' if total_excluded > pinned else 'good news: lower the pin to %d' % total_excluded}"
        )
    return errors


def check_anchors(rows: list, clients: list, roots: dict, root: pathlib.Path) -> list:
    errors = []
    for row in rows:
        for client in clients:
            cell = row.get(client, {})
            if cell.get("kind") != "yes":
                continue
            base = root / roots.get(client, f"clients/{client}")
            for path, token in cell["anchors"]:
                target = base / path
                if not target.is_file():
                    errors.append(f"{row['id']}/{client}: no such file {path} (root {base})")
                    continue
                if token not in target.read_text(encoding="utf-8", errors="replace"):
                    errors.append(f"{row['id']}/{client}: `{token}` not found in {path}")
    return errors


def check_listener_parity(groups: dict, lib_text: str) -> list:
    declared = [i.split(".", 1)[1] for i in groups.get("LISTENER_EVENTS", [])]
    trait = parse_listener_trait(lib_text)
    errors = []
    for missing in sorted(set(trait) - set(declared)):
        errors.append(
            f"CoreListener::{missing} is not in surface.rs LISTENER_EVENTS"
            f" — add it there and give it a ledger row"
        )
    for stale in sorted(set(declared) - set(trait)):
        errors.append(f"surface.rs LISTENER_EVENTS lists {stale}, which CoreListener no longer has")
    return errors


# --------------------------------------------------------------------------
# --self-test: a checker that stopped biting must fail loudly
# --------------------------------------------------------------------------
def self_test() -> int:
    ok = True

    def expect(name, errors, needle, should_fail=True):
        nonlocal ok
        hit = any(needle in e for e in errors)
        if hit != should_fail:
            ok = False
            print(f"FAIL  self-test `{name}`: expected {needle!r}"
                  f" {'in' if should_fail else 'absent from'} {errors}")
        else:
            print(f"ok    self-test `{name}`")

    def cell(spec):
        return parse_cell(spec)

    clients = ["android", "desktop"]
    core = ["item.files", "item.id"]
    good = [
        {"id": "item.files", "android": cell("yes: A.kt#item.files"),
         "desktop": cell("yes: m.cpp#value(\"files\")")},
        {"id": "item.id", "android": cell("yes: A.kt#item.id"),
         "desktop": cell("excluded: the desktop keys rows by id already")},
    ]
    expect("a clean ledger has no complaints", validate(core, clients, good, {"excluded": 1}), "", False)

    missing = [dict(good[0]), {"id": "item.id", "android": cell("yes: A.kt#item.id"),
                               "desktop": cell("-")}]
    expect("an undispositioned client", validate(core, clients, missing, {"excluded": 0}), "no disposition")

    stale = good + [{"id": "item.gone", "android": cell("excluded: x" * 1),
                     "desktop": cell("excluded: not there on desktop either")}]
    expect("a rotted ledger row", validate(core, clients, stale, {"excluded": 2}), "not in the core surface")

    absent = [good[0]]
    expect("a new core surface with no row", validate(core, clients, absent, {"excluded": 0}),
           "core surface has no ledger row")

    grew = [good[0], dict(good[1], desktop=cell("excluded: still not drawn on the desktop"))]
    expect("an exclusion added without the pin", validate(core, clients, grew, {"excluded": 0}),
           "grew")

    shrank = [good[0], dict(good[1], desktop=cell("yes: m.cpp#id")),
              {"id": "ui.card", "android": cell("yes: A.kt#x"),
               "desktop": cell("excluded: desktop card is coming later")}]
    expect("a pin nobody lowered", validate(core, clients, shrank, {"excluded": 5}), "shrank")

    short = [good[0], dict(good[1], desktop=cell("excluded: TODO, once the desktop grows a card"))]
    expect("a placeholder exclusion", validate(core, clients, short, {"excluded": 1}), "placeholder")

    syntax = [good[0], dict(good[1], desktop=cell("probably fine"))]
    expect("an unparseable cell", validate(core, clients, syntax, {"excluded": 0}), "use `yes:")

    bad_anchor = [good[0], dict(good[1], desktop=cell("yes: just-a-file"))]
    expect("an anchor with no token", validate(core, clients, bad_anchor, {"excluded": 0}),
           "is not `file#token`")

    # Anchor resolution, against a throwaway tree.
    with tempfile.TemporaryDirectory() as tmp:
        base = pathlib.Path(tmp) / "clients" / "android"
        base.mkdir(parents=True)
        (base / "A.kt").write_text("val x = item.files\n", encoding="utf-8")
        rows = [{"id": "item.files", "android": cell("yes: A.kt#item.files"),
                 "desktop": cell("excluded: the desktop draws no card yet")}]
        expect("a live anchor", check_anchors(rows, ["android"], {}, pathlib.Path(tmp)), "", False)
        dead = [{"id": "item.files", "android": cell("yes: A.kt#item.attachments"),
                 "desktop": cell("excluded: the desktop draws no card yet")}]
        expect("a token that is not there", check_anchors(dead, ["android"], {}, pathlib.Path(tmp)),
               "not found in A.kt")
        gone = [{"id": "item.files", "android": cell("yes: B.kt#item.files"),
                 "desktop": cell("excluded: the desktop draws no card yet")}]
        expect("a file that is not there", check_anchors(gone, ["android"], {}, pathlib.Path(tmp)),
               "no such file")

    # A listener the trait gained and the inventory missed.
    fake_lib = "pub trait CoreListener: Send + Sync {\n    fn on_status(&self);\n    fn on_brand_new(&self);\n}\n"
    errors = check_listener_parity({"LISTENER_EVENTS": ["listener.on_status"]}, fake_lib)
    expect("a trait callback missing from the inventory", errors, "on_brand_new")

    print("\nself-test:", "PASS" if ok else "FAIL")
    return 0 if ok else 1


# --------------------------------------------------------------------------
def main(argv=None) -> int:
    ap = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    ap.add_argument("--self-test", action="store_true", help="prove the checks still bite")
    ap.add_argument("--report", action="store_true", help="print the ledger as a table")
    args = ap.parse_args(argv)

    if args.self_test:
        return self_test()

    groups = parse_inventory(read(SURFACE_RS))
    core_ids = [i for g in groups.values() for i in g]
    ledger = parse_ledger(read(LEDGER))
    active = active_clients()

    errors = []
    errors += check_listener_parity(groups, read(LIB_RS))

    ledger_clients = ledger["clients"]
    discovered = sorted(active)
    if sorted(ledger_clients) != discovered:
        errors.append(
            f"{LEDGER} has columns {ledger_clients} but the tree has active clients"
            f" {discovered} — a client with sources must be ratcheted, not skipped"
        )

    errors += validate(core_ids, ledger_clients, ledger["rows"], ledger["pins"])
    errors += check_anchors(ledger["rows"], ledger_clients, ledger["roots"], ROOT)

    if args.report:
        print(f"{'surface':32} " + " ".join(f"{c:>14}" for c in ledger_clients))
        for row in ledger["rows"]:
            print(
                f"{row['id']:32} "
                + " ".join(f"{row.get(c, {}).get('kind', '?'):>14}" for c in ledger_clients)
            )
        print()

    excluded = sum(
        1 for row in ledger["rows"] for c in ledger_clients
        if row.get(c, {}).get("kind") == "excluded"
    )
    print(
        f"parity: {len(core_ids)} core surfaces, {len(ledger['rows'])} ledger rows,"
        f" {excluded} exclusions (pinned {ledger['pins'].get('excluded', '?')}),"
        f" clients {ledger_clients}"
    )
    if errors:
        print(f"\n{len(errors)} violation(s):\n", file=sys.stderr)
        for e in errors:
            print(f"  - {e}", file=sys.stderr)
        return 1
    print("ok — every client has dispositioned every surface")
    return 0


if __name__ == "__main__":
    sys.exit(main())
