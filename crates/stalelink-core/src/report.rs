use std::io::{self, Write};

use crate::{
    model::{Finding, Location},
    scan::ScanReport,
};
pub trait ReportSink {
    fn emit(&mut self, report: &ScanReport) -> io::Result<()>;
}
pub struct TableSink<W>(pub W);
impl<W: Write> ReportSink for TableSink<W> {
    fn emit(&mut self, report: &ScanReport) -> io::Result<()> {
        let mut findings = report.findings.clone();
        findings.sort_by(|a, b| {
            b.verdict
                .confidence
                .cmp(&a.verdict.confidence)
                .then_with(|| a.source.path.cmp(&b.source.path))
        });
        for finding in findings {
            writeln!(
                self.0,
                "{:13}  {}  {}  {}",
                confidence(finding.verdict.confidence),
                finding.url,
                source(&finding),
                reason(&finding)
            )?;
        }
        Ok(())
    }
}
fn confidence(value: crate::model::Confidence) -> &'static str {
    match value {
        crate::model::Confidence::DeadCertain => "DEAD-CERTAIN",
        crate::model::Confidence::LikelyDead => "LIKELY-DEAD",
        crate::model::Confidence::AuthWalled => "AUTH-WALLED",
        crate::model::Confidence::Outdated => "OUTDATED",
        crate::model::Confidence::Suspect => "SUSPECT",
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
        crate::model::Reason::HttpStatus(status) => format!("HTTP {status}"),
        ref reason => format!("{reason:?}"),
    }
}
