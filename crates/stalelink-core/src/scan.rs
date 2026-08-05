use std::{
    collections::HashMap,
    path::PathBuf,
    sync::Arc,
    time::{Duration, Instant},
};

use futures::{StreamExt, stream};
use tokio::sync::Semaphore;
use url::Url;

use crate::{
    check::Checker,
    extract::{SourceDocument, extract},
    model::{Finding, FixOrigin, Fixability, FoundLink, SuggestedFix, Verdict},
    walk::{WalkOptions, detect_format, walk},
};

pub struct ScanInput {
    pub paths: Vec<PathBuf>,
    pub walk: WalkOptions,
    pub max_concurrency: usize,
    pub exclude_urls: Vec<regex::Regex>,
    pub exclude_domains: Vec<String>,
}
pub struct ScanReport {
    pub findings: Vec<Finding>,
    pub files_scanned: usize,
    pub links_checked: usize,
    pub links_unique: usize,
    pub duration: Duration,
}
pub trait Progress {
    fn files_walked(&self, _: usize) {}
    fn links_found(&self, _: usize) {}
    fn checks_done(&self, _: usize) {}
}
pub struct NoProgress;
impl Progress for NoProgress {}

pub async fn scan(
    input: ScanInput,
    checker: &dyn Checker,
    progress: &impl Progress,
) -> Result<ScanReport, String> {
    let started = Instant::now();
    if input.max_concurrency == 0 {
        return Err("max_concurrency must be at least 1".into());
    }
    // The walk, file reads, UTF-8 decode, and parser extraction are blocking
    // CPU/IO work; run them on a blocking thread so a large corpus cannot
    // starve the async checking runtime (see architecture decision in #8).
    let paths_input = input.paths.clone();
    let walk_opts = input.walk.clone();
    let (paths, mut links) =
        tokio::task::spawn_blocking(move || collect_links(&paths_input, &walk_opts))
            .await
            .map_err(|e| e.to_string())??;
    progress.files_walked(paths.len());
    links.retain(|link| allowed(link, &input));
    progress.links_found(links.len());
    let mut grouped: HashMap<String, Vec<FoundLink>> = HashMap::new();
    for link in links {
        grouped.entry(link.url.clone()).or_default().push(link);
    }
    let semaphore = Arc::new(Semaphore::new(input.max_concurrency));
    let unique = grouped.len();
    let checks = stream::iter(grouped)
        .map(|(url, occurrences)| {
            let semaphore = semaphore.clone();
            async move {
                let permit = semaphore.acquire_owned().await.map_err(|e| e.to_string())?;
                let parsed = Url::parse(&url).map_err(|e| e.to_string())?;
                let verdict = checker.check(parsed).await;
                drop(permit);
                Ok::<_, String>((occurrences, verdict))
            }
        })
        .buffer_unordered(input.max_concurrency);
    let mut findings = Vec::new();
    let mut checked = 0;
    let mut checks = std::pin::pin!(checks);
    while let Some(result) = checks.next().await {
        let (occurrences, verdict) = result?;
        checked += 1;
        progress.checks_done(checked);
        if let Some(verdict) = verdict {
            findings.extend(occurrences.into_iter().map(|link| Finding {
                url: link.url,
                resolved_url: None,
                source: link.source,
                verdict: verdict.clone(),
                fix: suggested_fix(&verdict),
            }));
        }
    }
    Ok(ScanReport {
        findings,
        files_scanned: paths.len(),
        links_checked: checked,
        links_unique: unique,
        duration: started.elapsed(),
    })
}
fn suggested_fix(verdict: &Verdict) -> Option<SuggestedFix> {
    let (kind, origin) = match verdict.reason {
        crate::model::Reason::PermanentRedirect => ("redirect-target", FixOrigin::RedirectTarget),
        crate::model::Reason::VersionDrift => ("version-upgrade", FixOrigin::VersionUpgrade),
        _ => return None,
    };
    let replacement_url = verdict
        .evidence
        .iter()
        .find(|evidence| evidence.kind == kind)?
        .detail
        .parse::<Url>()
        .ok()
        .filter(|url| matches!(url.scheme(), "http" | "https"))?
        .to_string();
    Some(SuggestedFix {
        replacement_url,
        origin,
        fixable: Fixability::Auto,
    })
}
fn collect_links(
    paths_input: &[PathBuf],
    walk_opts: &WalkOptions,
) -> Result<(Vec<PathBuf>, Vec<FoundLink>), String> {
    let paths = walk(paths_input, walk_opts)?;
    let mut links = Vec::new();
    for path in &paths {
        let bytes = std::fs::read(path).map_err(|e| format!("{}: {e}", path.display()))?;
        let format = detect_format(path, &bytes);
        links.extend(
            extract(&SourceDocument {
                path: path.clone(),
                format,
                bytes,
            })
            .map_err(|e| e.0)?,
        );
    }
    Ok((paths, links))
}
fn allowed(link: &FoundLink, input: &ScanInput) -> bool {
    if input
        .exclude_urls
        .iter()
        .any(|regex| regex.is_match(&link.url))
    {
        return false;
    }
    Url::parse(&link.url)
        .ok()
        .and_then(|url| url.host_str().map(str::to_ascii_lowercase))
        .is_none_or(|host| {
            !input
                .exclude_domains
                .iter()
                .any(|domain| host == *domain || host.ends_with(&format!(".{domain}")))
        })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        check::{CheckFuture, Checker},
        model::{Confidence, Evidence, Reason, Verdict},
    };
    use chrono::Utc;
    use std::io::Write;
    use std::sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    };

    struct Fake(Arc<AtomicUsize>);
    impl Checker for Fake {
        fn check(&self, _: Url) -> CheckFuture<'_> {
            self.0.fetch_add(1, Ordering::SeqCst);
            Box::pin(async {
                Some(Verdict {
                    confidence: Confidence::DeadCertain,
                    reason: Reason::HttpStatus(404),
                    evidence: vec![Evidence {
                        kind: "status".into(),
                        detail: "404".into(),
                    }],
                    checked_at: Utc::now(),
                    tier: 1,
                })
            })
        }
    }

    struct VerdictChecker(Verdict);
    impl Checker for VerdictChecker {
        fn check(&self, _: Url) -> CheckFuture<'_> {
            let verdict = self.0.clone();
            Box::pin(async move { Some(verdict) })
        }
    }

    #[tokio::test]
    async fn deduplicates_checks_but_reports_each_occurrence() {
        let file = tempfile::NamedTempFile::with_suffix(".txt").unwrap();
        std::fs::write(file.path(), "https://bad.test/x https://bad.test/x").unwrap();
        let calls = Arc::new(AtomicUsize::new(0));
        let report = scan(
            ScanInput {
                paths: vec![file.path().into()],
                walk: WalkOptions::default(),
                max_concurrency: 2,
                exclude_urls: vec![],
                exclude_domains: vec![],
            },
            &Fake(calls.clone()),
            &NoProgress,
        )
        .await
        .unwrap();
        assert_eq!(calls.load(Ordering::SeqCst), 1);
        assert_eq!(report.findings.len(), 2);
        assert_eq!(report.links_unique, 1);
    }

    #[tokio::test]
    async fn scan_creates_complete_automatic_fixes_for_valid_evidence() {
        let redirect = Verdict {
            confidence: Confidence::Outdated,
            reason: Reason::PermanentRedirect,
            evidence: vec![Evidence {
                kind: "redirect-target".into(),
                detail: "https://example.test/current".into(),
            }],
            checked_at: Utc::now(),
            tier: 1,
        };
        let version = Verdict {
            confidence: Confidence::Outdated,
            reason: Reason::VersionDrift,
            evidence: vec![Evidence {
                kind: "version-upgrade".into(),
                detail: "https://example.test/v2/items".into(),
            }],
            checked_at: Utc::now(),
            tier: 1,
        };
        let file = tempfile::NamedTempFile::with_suffix(".txt").unwrap();
        std::fs::write(file.path(), "https://example.test/old").unwrap();
        for (verdict, replacement_url, origin) in [
            (
                redirect,
                "https://example.test/current",
                FixOrigin::RedirectTarget,
            ),
            (
                version,
                "https://example.test/v2/items",
                FixOrigin::VersionUpgrade,
            ),
        ] {
            let report = scan(
                ScanInput {
                    paths: vec![file.path().into()],
                    walk: WalkOptions::default(),
                    max_concurrency: 1,
                    exclude_urls: vec![],
                    exclude_domains: vec![],
                },
                &VerdictChecker(verdict),
                &NoProgress,
            )
            .await
            .unwrap();
            assert_eq!(
                report.findings[0].fix,
                Some(SuggestedFix {
                    replacement_url: replacement_url.into(),
                    origin,
                    fixable: Fixability::Auto,
                })
            );
        }
    }

    #[test]
    fn malformed_or_non_http_fix_evidence_is_rejected() {
        for detail in ["not a url", "file:///local/path"] {
            let verdict = Verdict {
                confidence: Confidence::Outdated,
                reason: Reason::PermanentRedirect,
                evidence: vec![Evidence {
                    kind: "redirect-target".into(),
                    detail: detail.into(),
                }],
                checked_at: Utc::now(),
                tier: 1,
            };
            assert!(suggested_fix(&verdict).is_none());
        }
        let no_fix = Verdict {
            confidence: Confidence::Outdated,
            reason: Reason::StalenessBanner,
            evidence: vec![Evidence {
                kind: "staleness-phrase".into(),
                detail: "deprecated".into(),
            }],
            checked_at: Utc::now(),
            tier: 1,
        };
        assert!(suggested_fix(&no_fix).is_none());
    }

    #[tokio::test]
    async fn mixed_corpus_scan_handles_all_document_formats() {
        let directory = tempfile::tempdir().unwrap();
        for (name, contents) in [
            ("one.md", "[x](https://mixed.test/markdown)"),
            ("two.html", "<a href=\"https://mixed.test/html\">x</a>"),
            ("three.txt", "https://mixed.test/text"),
        ] {
            std::fs::write(directory.path().join(name), contents).unwrap();
        }
        let mut zip = zip::ZipWriter::new(std::io::Cursor::new(Vec::new()));
        zip.start_file(
            "word/document.xml",
            zip::write::SimpleFileOptions::default(),
        )
        .unwrap();
        zip.write_all(br#"<w:document xmlns:w="w"><w:body><w:p><w:r><w:instrText>HYPERLINK &quot;https://mixed.test/docx&quot;</w:instrText></w:r></w:p></w:body></w:document>"#).unwrap();
        zip.start_file(
            "word/_rels/document.xml.rels",
            zip::write::SimpleFileOptions::default(),
        )
        .unwrap();
        zip.write_all(b"<Relationships/>").unwrap();
        let docx = zip.finish().unwrap().into_inner();
        std::fs::write(directory.path().join("four.bin"), docx).unwrap();
        let mut zip = zip::ZipWriter::new(std::io::Cursor::new(Vec::new()));
        for (name, xml) in [
            (
                "xl/workbook.xml",
                r#"<workbook xmlns:r="r"><sheets><sheet name="S" r:id="rId1"/></sheets></workbook>"#,
            ),
            (
                "xl/_rels/workbook.xml.rels",
                r#"<Relationships><Relationship Id="rId1" Target="worksheets/sheet1.xml"/></Relationships>"#,
            ),
            (
                "xl/worksheets/sheet1.xml",
                r#"<worksheet><sheetData><row><c r="A1"><f>HYPERLINK(&quot;https://mixed.test/xlsx&quot;)</f></c></row></sheetData></worksheet>"#,
            ),
        ] {
            zip.start_file(name, zip::write::SimpleFileOptions::default())
                .unwrap();
            zip.write_all(xml.as_bytes()).unwrap();
        }
        std::fs::write(
            directory.path().join("five.wrong"),
            zip.finish().unwrap().into_inner(),
        )
        .unwrap();
        let mut zip = zip::ZipWriter::new(std::io::Cursor::new(Vec::new()));
        for (name, xml) in [
            (
                "ppt/slides/slide1.xml",
                r#"<p:sld xmlns:p="p" xmlns:r="r"><p:cNvPr><a:hlinkClick r:id="rId1"/></p:cNvPr></p:sld>"#,
            ),
            (
                "ppt/slides/_rels/slide1.xml.rels",
                r#"<Relationships><Relationship Id="rId1" TargetMode="External" Target="https://mixed.test/pptx"/></Relationships>"#,
            ),
        ] {
            zip.start_file(name, zip::write::SimpleFileOptions::default())
                .unwrap();
            zip.write_all(xml.as_bytes()).unwrap();
        }
        std::fs::write(
            directory.path().join("six.pptx"),
            zip.finish().unwrap().into_inner(),
        )
        .unwrap();
        let stream = "BT /F1 12 Tf 72 720 Td (https://mixed.test/pdf) Tj ET\n";
        let objects = [
            "<< /Type /Catalog /Pages 2 0 R >>".to_owned(),
            "<< /Type /Pages /Kids [3 0 R] /Count 1 >>".to_owned(),
            "<< /Type /Page /Parent 2 0 R /Resources << /Font << /F1 5 0 R >> >> /Contents 4 0 R >>".to_owned(),
            format!("<< /Length {} >>\nstream\n{stream}endstream", stream.len()),
            "<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica >>".to_owned(),
        ];
        let mut pdf = b"%PDF-1.4\n".to_vec();
        let mut offsets = vec![0];
        for (number, object) in objects.iter().enumerate() {
            offsets.push(pdf.len());
            pdf.extend_from_slice(format!("{} 0 obj\n{object}\nendobj\n", number + 1).as_bytes());
        }
        let xref = pdf.len();
        pdf.extend_from_slice(
            format!("xref\n0 {}\n0000000000 65535 f \n", objects.len() + 1).as_bytes(),
        );
        for offset in offsets.iter().skip(1) {
            pdf.extend_from_slice(format!("{offset:010} 00000 n \n").as_bytes());
        }
        pdf.extend_from_slice(
            format!(
                "trailer\n<< /Size {} /Root 1 0 R >>\nstartxref\n{xref}\n%%EOF\n",
                objects.len() + 1
            )
            .as_bytes(),
        );
        std::fs::write(directory.path().join("seven.txt"), pdf).unwrap();
        let calls = Arc::new(AtomicUsize::new(0));
        let report = scan(
            ScanInput {
                paths: vec![directory.path().into()],
                walk: WalkOptions::default(),
                max_concurrency: 2,
                exclude_urls: vec![],
                exclude_domains: vec![],
            },
            &Fake(calls.clone()),
            &NoProgress,
        )
        .await
        .unwrap();
        assert_eq!(report.files_scanned, 7);
        assert_eq!(report.findings.len(), 7, "{:#?}", report.findings);
        assert_eq!(calls.load(Ordering::SeqCst), 7);
    }
}
