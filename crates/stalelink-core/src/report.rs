use std::io::{self, Write};

use unicode_width::UnicodeWidthStr;

use crate::{
    model::{Confidence, Finding, Location, Reason},
    scan::ScanReport,
};
pub trait ReportSink {
    fn emit(&mut self, report: &ScanReport) -> io::Result<()>;
}
pub struct TableSink<W>(pub W);
impl<W: Write> ReportSink for TableSink<W> {
    fn emit(&mut self, report: &ScanReport) -> io::Result<()> {
        if report.findings.is_empty() {
            return Ok(());
        }
        // Deterministic order: arrival order is nondeterministic (concurrent
        // checks over a HashMap), so sort by the fully rendered row after the
        // confidence rank. Sorting the complete row tuple guarantees a total,
        // stable order even when two findings share confidence/path/span/url.
        let mut rows: Vec<(u8, [String; 4])> = report
            .findings
            .iter()
            .map(|finding| {
                (
                    confidence_rank(finding.verdict.confidence),
                    [
                        confidence(finding.verdict.confidence).to_owned(),
                        finding.url.clone(),
                        source(finding),
                        reason(finding),
                    ],
                )
            })
            .collect();
        rows.sort_by(|a, b| a.0.cmp(&b.0).then_with(|| a.1.cmp(&b.1)));
        let rows: Vec<[String; 4]> = rows.into_iter().map(|(_, row)| row).collect();
        let headers = ["CONFIDENCE", "URL", "SOURCE", "REASON"];
        let mut widths = headers.map(UnicodeWidthStr::width);
        for row in &rows {
            for (width, cell) in widths.iter_mut().zip(row) {
                *width = (*width).max(cell.width());
            }
        }
        write_row(&mut self.0, &headers.map(String::from), &widths)?;
        for row in &rows {
            write_row(&mut self.0, row, &widths)?;
        }
        Ok(())
    }
}
fn write_row<W: Write>(writer: &mut W, row: &[String; 4], widths: &[usize; 4]) -> io::Result<()> {
    // Pad by display width, not byte length, so multibyte cells stay aligned.
    writeln!(
        writer,
        "{}{}  {}{}  {}{}  {}",
        row[0],
        pad(&row[0], widths[0]),
        row[1],
        pad(&row[1], widths[1]),
        row[2],
        pad(&row[2], widths[2]),
        row[3],
    )
}
fn pad(cell: &str, width: usize) -> String {
    " ".repeat(width.saturating_sub(cell.width()))
}
// Ascending sort key with the most severe confidence first (rank 0), matching
// the previous descending-by-severity ordering.
fn confidence_rank(value: Confidence) -> u8 {
    match value {
        Confidence::DeadCertain => 0,
        Confidence::LikelyDead => 1,
        Confidence::Outdated => 2,
        Confidence::AuthWalled => 3,
        Confidence::Suspect => 4,
    }
}
fn confidence(value: Confidence) -> &'static str {
    match value {
        Confidence::DeadCertain => "DEAD-CERTAIN",
        Confidence::LikelyDead => "LIKELY-DEAD",
        Confidence::AuthWalled => "AUTH-WALLED",
        Confidence::Outdated => "OUTDATED",
        Confidence::Suspect => "SUSPECT",
    }
}
fn source(finding: &Finding) -> String {
    match finding.source.location {
        Location::Text { line, column } => {
            format!("{}:{line}:{column}", finding.source.path.display())
        }
        _ => finding.source.path.display().to_string(),
    }
}
fn reason(finding: &Finding) -> String {
    match finding.verdict.reason {
        Reason::HttpStatus(status) => format!("HTTP {status}"),
        ref reason => format!("{reason:?}"),
    }
}
