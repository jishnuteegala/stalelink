//! Formal check for stalelink-core's enumerable contracts.
//! Runs the real `Confidence` ordering, `Reason::confidence`, and
//! `apply_edits` against independently stated references:
//!   - severity order DeadCertain > LikelyDead > Outdated > AuthWalled > Suspect
//!   - status table {401,403}->AuthWalled {404,410,451}->DeadCertain else->Suspect
//!   - a forward left-to-right splice oracle plus interval-overlap math

use stalelink_core::fix::{Edit, apply_edits};
use stalelink_core::model::{Confidence, NetKind, Reason};

// --- S1: Confidence is a strict total order with the documented severity ---

#[test]
fn confidence_total_order() {
    use Confidence::*;
    let severity = [DeadCertain, LikelyDead, Outdated, AuthWalled, Suspect];
    let mut bad = Vec::new();
    for (i, a) in severity.iter().enumerate() {
        for (j, b) in severity.iter().enumerate() {
            let got = a.cmp(b);
            let want = j.cmp(&i); // earlier in the list = more severe
            if got != want {
                bad.push(format!("{a:?}.cmp({b:?}) = {got:?}, want {want:?}"));
            }
        }
    }
    assert!(bad.is_empty(), "{}", bad.join("; "));
}

// --- S2: Reason::confidence over the whole domain -------------------------

fn want_status_confidence(status: u16) -> Confidence {
    match status {
        401 | 403 => Confidence::AuthWalled,
        404 | 410 | 451 => Confidence::DeadCertain,
        _ => Confidence::Suspect,
    }
}

#[test]
fn reason_confidence_is_total() {
    use Confidence::*;
    let mut bad = Vec::new();
    for status in 0u16..=u16::MAX {
        let got = Reason::HttpStatus(status).confidence();
        let want = want_status_confidence(status);
        if got != want {
            bad.push(format!("HttpStatus({status}) -> {got:?}, want {want:?}"));
        }
    }
    let fixed: [(Reason, Confidence); 14] = [
        (Reason::NetworkError(NetKind::Dns), LikelyDead),
        (Reason::NetworkError(NetKind::Tls), LikelyDead),
        (Reason::NetworkError(NetKind::Timeout), LikelyDead),
        (Reason::NetworkError(NetKind::ConnRefused), LikelyDead),
        (Reason::NetworkError(NetKind::Other), LikelyDead),
        (Reason::Soft404, LikelyDead),
        (Reason::LoginWall, AuthWalled),
        (Reason::PermanentRedirect, Outdated),
        (Reason::StalenessBanner, Outdated),
        (Reason::VersionDrift, Outdated),
        (Reason::FarPastLastModified, Suspect),
        (Reason::AnomalousResponse, Suspect),
        (Reason::LocalMissing, DeadCertain),
        (Reason::SyntaxInvalid, DeadCertain),
    ];
    // Exhaustive match with no wildcard: adding a Reason or NetKind variant
    // fails compilation here instead of being silently skipped by `fixed`.
    // The arm list is the coverage contract: every arm returning `true` must
    // have a `fixed` entry (returning `true` without one would pass silently,
    // so a new variant still needs a human to write its confidence row).
    fn coverable_in_fixed(r: &Reason) -> bool {
        match r {
            Reason::HttpStatus(_) => false, // covered by the status loop above
            Reason::NetworkError(NetKind::Dns)
            | Reason::NetworkError(NetKind::Tls)
            | Reason::NetworkError(NetKind::Timeout)
            | Reason::NetworkError(NetKind::ConnRefused)
            | Reason::NetworkError(NetKind::Other)
            | Reason::Soft404
            | Reason::LoginWall
            | Reason::PermanentRedirect
            | Reason::StalenessBanner
            | Reason::VersionDrift
            | Reason::FarPastLastModified
            | Reason::AnomalousResponse
            | Reason::LocalMissing
            | Reason::SyntaxInvalid => true,
        }
    }
    assert!(fixed.iter().all(|(r, _)| coverable_in_fixed(r)));
    for (reason, want) in fixed {
        let got = reason.confidence();
        if got != want {
            bad.push(format!("{reason:?} -> {got:?}, want {want:?}"));
        }
    }
    assert!(bad.is_empty(), "{}", bad.join("; "));
}

// --- S3: apply_edits acceptance + output against an independent oracle ----

// Left-to-right splice: walk spans in ascending order, tracking the byte
// delta introduced so far. A genuinely different implementation from the
// verified code's right-to-left in-place splice.
fn oracle_apply(original: &[u8], edits: &[Edit]) -> Vec<u8> {
    let mut sorted = edits.to_vec();
    sorted.sort_by_key(|e| (e.span.start, e.span.end));
    let mut out = Vec::with_capacity(original.len());
    let mut cursor = 0usize;
    for edit in &sorted {
        out.extend_from_slice(&original[cursor..edit.span.start]);
        out.extend_from_slice(&edit.replacement);
        cursor = edit.span.end;
    }
    out.extend_from_slice(&original[cursor..]);
    out
}

fn oracle_valid(original: &[u8], edits: &[Edit]) -> bool {
    let mut sorted: Vec<&Edit> = edits.iter().collect();
    sorted.sort_by_key(|e| (e.span.start, e.span.end));
    if sorted
        .iter()
        .any(|e| e.span.start > e.span.end || e.span.end > original.len())
    {
        return false;
    }
    // Adjacent-pair check is complete for intervals sorted by start.
    !sorted.windows(2).any(|w| {
        let (a, b) = (&w[0].span, &w[1].span);
        a.end > b.start || (a.is_empty() && b.is_empty() && a.start == b.start)
    })
}

fn span_variants(len: usize) -> Vec<Edit> {
    let mut spans = Vec::new();
    for start in 0..=len + 1 {
        for end in start..=len + 2 {
            for replacement in [Vec::new(), vec![b'X']] {
                spans.push(Edit {
                    span: start..end,
                    replacement,
                });
            }
        }
    }
    // Malformed ranges (start > end): valid Range values the impl must reject.
    spans.push(Edit {
        span: len + 1..0,
        replacement: vec![b'X'],
    });
    spans.push(Edit {
        span: len + 2..1,
        replacement: Vec::new(),
    });
    spans
}

// The implementation legitimately rejects one ambiguous shape the oracle
// accepts: an empty insert at position s that appears before a non-empty
// span also starting at s in the input order. Rejecting anything else is a
// liveness failure - the bytes a `fix --write` produces would silently stop.
fn is_ambiguous(set: &[Edit]) -> bool {
    for (i, a) in set.iter().enumerate() {
        for b in &set[i + 1..] {
            if a.span.is_empty() && a.span.start == b.span.start && !b.span.is_empty() {
                return true;
            }
        }
    }
    false
}

#[test]
fn apply_edits_matches_oracle() {
    let mut bad = Vec::new();
    for len in [0usize, 1, 4, 8] {
        let original: Vec<u8> = (0..len as u8).map(|b| b + b'a').collect();
        let edits = span_variants(len);
        let mut sets: Vec<Vec<Edit>> = vec![Vec::new()];
        sets.extend(edits.iter().map(|e| vec![e.clone()]));
        // all ordered 2-edit sets - the (a, b) loop already covers both orders
        for a in &edits {
            for b in &edits {
                sets.push(vec![a.clone(), b.clone()]);
            }
        }
        for set in sets {
            let got = apply_edits(&original, &set);
            let want_valid = oracle_valid(&original, &set);
            match (got, want_valid) {
                (Ok(out), true) => {
                    // Any accepted set must produce exactly the oracle output.
                    let want = oracle_apply(&original, &set);
                    if out != want {
                        bad.push(format!("len={len} {set:?}: got {out:?}, oracle {want:?}"));
                    }
                }
                // The implementation may conservatively reject ambiguous
                // equal-start sets (empty insert at a span boundary), but it
                // must NEVER accept a set the oracle calls invalid, and must
                // not reject a valid unambiguous one.
                (Ok(out), false) => bad.push(format!(
                    "len={len} {set:?}: accepted invalid edit set -> {out:?}"
                )),
                (Err(e), true) if !is_ambiguous(&set) => bad.push(format!(
                    "len={len} {set:?}: rejected a valid unambiguous set -> {e:?}"
                )),
                (Err(_), _) => {}
            }
        }
    }
    assert!(bad.is_empty(), "{}", bad.join("; "));
}

// Order invariance: for sets the implementation accepts in both argument
// orders, both orders must produce identical output.
#[test]
fn apply_edits_order_invariant_when_accepted() {
    let mut bad = Vec::new();
    for len in [0usize, 1, 4] {
        let original: Vec<u8> = (0..len as u8).map(|b| b + b'a').collect();
        let edits = span_variants(len);
        for a in &edits {
            for b in &edits {
                let forward = apply_edits(&original, &[a.clone(), b.clone()]);
                let reverse = apply_edits(&original, &[b.clone(), a.clone()]);
                if let (Ok(f), Ok(r)) = (forward, reverse)
                    && f != r
                {
                    bad.push(format!("len={len} {a:?}+{b:?}: order changed output"));
                }
            }
        }
    }
    assert!(bad.is_empty(), "{}", bad.join("; "));
}
