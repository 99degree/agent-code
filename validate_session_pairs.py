#!/usr/bin/env python3
"""
Focused session validator: role-pair + tool-pair integrity.

Starts counting/validating from the LAST compact-summary (compact_boundary)
system message and skips everything before it. Reports:
  - role-pair (adjacent role transition) integrity
  - tool-pair integrity (every tool_use answered by a tool_result by id)
"""

import json
import sys
from pathlib import Path


def find_last_compact_boundary(msgs):
    last = -1
    for i, m in enumerate(msgs):
        if m.get("type") == "system" and m.get("subtype") == "compact_boundary":
            last = i
    return last


def iter_content_blocks(m):
    c = m.get("content")
    if isinstance(c, list):
        for b in c:
            if isinstance(b, dict):
                yield b


def validate_role_pairs(seq):
    """Return list of (rel_index, prev_role, role, status) for invalid/warn pairs.

    Strict agent alternation after the leading system: user <-> assistant.
    A bare `system` in the middle is allowed but flagged as a WARN (injected note).
    """
    VALID = {("system", "user"), ("user", "assistant"), ("assistant", "user"), ("system", "assistant")}
    WARN = {("user", "system")}
    issues = []
    for i in range(1, len(seq)):
        pair = (seq[i - 1], seq[i])
        if pair in VALID:
            status = "OK"
        elif pair in WARN:
            status = "WARN"
        else:
            status = "INVALID"
        if status != "OK":
            issues.append((i, seq[i - 1], seq[i], status))
    return issues


def validate_tool_pairs(msgs):
    uses = []      # (rel_index, id)
    results = []   # (rel_index, id)
    for i, m in enumerate(msgs):
        for b in iter_content_blocks(m):
            if b.get("type") == "tool_use":
                uses.append((i, b.get("id")))
            elif b.get("type") == "tool_result":
                results.append((i, b.get("tool_use_id")))

    use_ids = {i for _, i in uses}
    res_ids = {i for _, i in results}
    unmatched_uses = use_ids - res_ids
    orphan_results = res_ids - use_ids

    # ordering: every result must come after its use
    order_issues = []
    first_use = {i: idx for idx, i in uses}
    for idx, rid in results:
        if rid in first_use and idx < first_use[rid]:
            order_issues.append((rid, first_use[rid], idx))

    return {
        "use_count": len(uses),
        "result_count": len(results),
        "unmatched_uses": sorted(unmatched_uses),
        "orphan_results": sorted(orphan_results),
        "order_issues": order_issues,
    }


def main():
    if len(sys.argv) < 2:
        print("Usage: python validate_session_pairs.py <session.json> [cut:last_compact|none]")
        return
    path = Path(sys.argv[1]).expanduser()
    cut_mode = sys.argv[2] if len(sys.argv) > 2 else "last_compact"

    s = json.load(open(path))
    msgs = s["messages"]

    if cut_mode == "none":
        start = 0
    else:
        start = find_last_compact_boundary(msgs)
        if start < 0:
            print("No compact_boundary found; validating whole session.")
            start = 0

    tail = msgs[start:]
    seq = [m.get("type") for m in tail]

    print("=" * 72)
    print(f"Session : {s.get('id')}")
    print(f"Cut     : start at message index {start} (last compact summary)")
    print(f"Counted : {len(tail)} messages from cut point")
    print("=" * 72)

    # --- Role pairs ---
    role_issues = validate_role_pairs(seq)
    invalid = [r for r in role_issues if r[3] == "INVALID"]
    warn = [r for r in role_issues if r[3] == "WARN"]
    print(f"\nROLE PAIRS: {len(seq)-1} adjacent pairs | "
          f"INVALID={len(invalid)} WARN={len(warn)}")
    for rel, prev, role, status in role_issues:
        print(f"  [{status}] rel#{rel} {prev} -> {role}")
    if not role_issues:
        print("  (all adjacent role transitions valid)")

    # --- Tool pairs ---
    tp = validate_tool_pairs(tail)
    print(f"\nTOOL PAIRS: uses={tp['use_count']} results={tp['result_count']}")
    print(f"  unmatched tool_use (no result): {len(tp['unmatched_uses'])}")
    for uid in tp["unmatched_uses"]:
        print(f"    - {uid}")
    print(f"  orphan tool_result (no use)   : {len(tp['orphan_results'])}")
    for rid in tp["orphan_results"]:
        print(f"    - {rid}")
    print(f"  ordering violations (result before use): {len(tp['order_issues'])}")
    for rid, use_idx, res_idx in tp["order_issues"]:
        print(f"    - {rid} use@{use_idx} result@{res_idx}")

    # --- Verdict ---
    ok = (not invalid) and (not tp["unmatched_uses"]) and (not tp["orphan_results"]) and (not tp["order_issues"])
    print("\n" + "=" * 72)
    print(f"VERDICT: {'PASS' if ok else 'FAIL'}"
          + ("" if not warn else f" (with {len(warn)} WARN)"))
    print("=" * 72)


if __name__ == "__main__":
    main()
