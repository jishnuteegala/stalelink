---
name: formal-verify-stalelink
description: "Verify stalelink's enumerable severity + edit contracts (stalelink-core model.rs Confidence/Reason, fix.rs apply_edits) - the 5x5 confidence order, Reason::confidence over all 65536 statuses, and edit acceptance/splicing against an independent oracle. Run after any change to Confidence, Reason, apply_edits, or the fix pipeline."
---

# Formal verify: stalelink contracts

## What it proves

Two user-harm contracts. First, severity: `--fail-on` is a release gate - `DeadCertain > LikelyDead > Outdated > AuthWalled > Suspect` ordered wrong means CI blocks on a suspect redirect or passes a dead link. A transposition of two adjacent ranks is a one-line typo that inverts the gate. Second, `apply_edits`: the bytes a `fix --write` produces must equal the spans it was given - out-of-bounds or overlapping edits silently corrupting a document is unrecoverable.

`crates/stalelink-core/tests/formal.rs` runs the **real** code against independently stated references: the documented severity list (5x5 cmp matrix), the status table over all 65536 `u16` values (`{401,403}->AuthWalled`, `{404,410,451}->DeadCertain`, else `Suspect`), every `Reason` variant, and `apply_edits` over all span sets of size <=2 on documents of length 0/1/4/8 - acceptance checked against interval-overlap math, output checked against a left-to-right splice oracle (a genuinely different implementation from the right-to-left in-place splice), plus order invariance for accepted sets.

## Run

```bash
cargo test -p stalelink-core --test formal
```

No fixtures, no network - pure function checks, <1s after build. The test file lives in `crates/stalelink-core/tests/` because `cargo` only discovers integration tests there; this SKILL.md is its index entry.

## Coverage record

| Covered | Items | Oracle |
| --- | --- | --- |
| `stalelink-core/src/model.rs` | `Confidence::rank`/`Ord`, `Reason::confidence` | documented severity table, full u16 status domain |
| `stalelink-core/src/fix.rs` | `apply_edits` | forward splice + interval math, all small span sets |

**Sync commit:** `5caad4e` (committed 2026-10-10; verified in sync on 2026-10-24). Move this forward only after re-running the check and confirming the contract describes the code at the new commit.

Not covered, on purpose: the `--fail-on` decision itself (`findings.any(|f| f.verdict.confidence >= fail_on)` at `crates/stalelink/src/main.rs:378`) - it lives in the bin crate and is not importable; the enum contract it depends on is proven here and the CLI behavior stays with `verify-stalelink`'s live drives. `sarif_level`/`sarif_rank` (bin crate, same reason), `write_in_place` restore-on-failure (core crate but already covered by its own unit tests; this check exists for contracts with no other guard), and the masking question - no URL masking code exists in this repo yet; if a `mask_url`-style function is added, it becomes an excellent new formal target. Also: `apply_edits` conservatively rejects ambiguous equal-start sets (an empty insertion sharing another span's start - an insert at a span's *end* boundary is accepted by both sides) with an order-dependent verdict - the check pins "Ok implies oracle-equal output", "true overlaps always rejected", and "valid unambiguous sets always accepted" - only the one ambiguous shape (empty insert at a shared start, ordered before the non-empty span in input) may be rejected; flag it if callers start generating such sets.

## Reading a failure

`Outdated.cmp(AuthWalled) = Less, want Greater` style lines name the inverted pair - check `rank()` in `model.rs`. `HttpStatus(N) -> X, want Y` names the miscategorized status. `accepted invalid edit set` means bounds/overlap validation has a hole; `got {bytes} oracle {bytes}` means splicing order or offset math regressed - compare `apply_edits`' sort/splice against the oracle's left-to-right walk.

## Updating the verification

When a change is intended (a new `Confidence` tier, a new `Reason` variant, different overlap policy - evidence in the change: tests, commit message, docs), update all of: the `severity` list, the `want_status_confidence` table, the `fixed` variant list, and `oracle_valid`/`oracle_apply` if edit semantics change. Then break the code the way the contract guards against (swap two adjacent `rank` values; drop the `Reverse` sort; delete the overlap window check) and confirm a failure appears before restoring. Record the new sync commit above.
