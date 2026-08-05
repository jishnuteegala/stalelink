use std::io::{self, Write};

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
        let mut findings = report.findings.clone();
        findings.sort_by(|a, b| {
            b.verdict
                .confidence
                .cmp(&a.verdict.confidence)
                .then_with(|| a.source.path.cmp(&b.source.path))
        });
        let rows: Vec<[String; 4]> = findings
            .iter()
            .map(|finding| {
                [
                    confidence(finding.verdict.confidence).to_owned(),
                    finding.url.clone(),
                    source(finding),
                    reason(finding),
                ]
            })
            .collect();
        let headers = ["CONFIDENCE", "URL", "SOURCE", "REASON"];
        let mut widths = headers.map(str::len);
        for row in &rows {
            for (width, cell) in widths.iter_mut().zip(row) {
                *width = (*width).max(cell.len());
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
    writeln!(
        writer,
        "{:<w0$}  {:<w1$}  {:<w2$}  {}",
        row[0],
        row[1],
        row[2],
        row[3],
        w0 = widths[0],
        w1 = widths[1],
        w2 = widths[2],
    )
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
