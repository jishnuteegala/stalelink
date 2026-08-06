use std::{
    collections::{HashMap, HashSet},
    io::Read,
    path::{Path, PathBuf},
    sync::Arc,
    time::{Duration, Instant},
};

use chrono::Utc;
use futures::{StreamExt, stream};
use html5tokenizer::{NaiveParser, Token, TracingEmitter, offset::PosTrackingReader};
use pulldown_cmark::{Event, Parser, Tag, TagEnd};
use tokio::sync::Semaphore;
use url::Url;

use crate::{
    check::Checker,
    extract::{SourceDocument, extract},
    model::{
        DocFormat, Evidence, Finding, FixOrigin, Fixability, FoundLink, Reason, SuggestedFix,
        Verdict,
    },
    walk::{WalkOptions, detect_format, extension_format, walk},
};

pub struct ScanInput {
    pub paths: Vec<PathBuf>,
    pub walk: WalkOptions,
    pub max_concurrency: usize,
    pub exclude_urls: Vec<regex::Regex>,
    pub exclude_domains: Vec<String>,
    pub check_local: bool,
}
pub struct ScanReport {
    pub findings: Vec<Finding>,
    /// The exact paths selected by the walk before extraction and checking.
    pub resolved_paths: Vec<PathBuf>,
    pub files_scanned: usize,
    pub links_checked: usize,
    pub links_unique: usize,
    pub duration: Duration,
}
pub trait Progress {
    fn files_walked(&self, _: usize) {}
    fn links_found(&self, _: usize) {}
    fn checks_done(&self, _: usize) {}
    fn checking(&self, _: &Url) {}
    fn checked(&self, _: &Url, _: Option<&Verdict>) {}
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
    let mut findings = Vec::new();
    let mut local_checked = 0;
    let mut local_unique = 0;
    if input.check_local {
        let mut local_links: HashMap<(PathBuf, String), Vec<FoundLink>> = HashMap::new();
        for link in links.iter().filter(|link| is_local_or_contact(&link.url)) {
            local_links
                .entry((link.source.path.clone(), link.url.clone()))
                .or_default()
                .push(link.clone());
        }
        local_unique = local_links.len();
        for (_, occurrences) in local_links {
            local_checked += 1;
            if let Some(verdict) = local_verdict(&occurrences[0]) {
                findings.extend(occurrences.into_iter().map(|link| Finding {
                    url: link.url,
                    resolved_url: None,
                    source: link.source,
                    verdict: verdict.clone(),
                    fix: None,
                }));
            }
        }
    }
    let mut grouped: HashMap<String, Vec<FoundLink>> = HashMap::new();
    for link in links {
        if is_http(&link.url) {
            grouped.entry(link.url.clone()).or_default().push(link);
        }
    }
    let semaphore = Arc::new(Semaphore::new(input.max_concurrency));
    let unique = grouped.len();
    let checks = stream::iter(grouped)
        .map(|(url, occurrences)| {
            let semaphore = semaphore.clone();
            async move {
                let permit = semaphore.acquire_owned().await.map_err(|e| e.to_string())?;
                let parsed = Url::parse(&url).map_err(|e| e.to_string())?;
                progress.checking(&parsed);
                let verdict = checker.check(parsed.clone()).await;
                drop(permit);
                Ok::<_, String>((parsed, occurrences, verdict))
            }
        })
        .buffer_unordered(input.max_concurrency);
    let mut checked = local_checked;
    let mut checks = std::pin::pin!(checks);
    while let Some(result) = checks.next().await {
        let (url, occurrences, verdict) = result?;
        checked += 1;
        progress.checks_done(checked);
        progress.checked(&url, verdict.as_ref());
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
        resolved_paths: paths,
        links_checked: checked,
        links_unique: unique + local_unique,
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

fn is_http(url: &str) -> bool {
    matches!(Url::parse(url).map(|parsed| parsed.scheme().to_owned()), Ok(scheme) if scheme == "http" || scheme == "https")
}

fn is_local_or_contact(url: &str) -> bool {
    contact_scheme(url).is_some() || url.starts_with('#') || Url::parse(url).is_err()
}

fn local_verdict(link: &FoundLink) -> Option<Verdict> {
    let (reason, detail) = match contact_scheme(&link.url) {
        Some(("mailto", address)) => validate_mailto(address)
            .err()
            .map(|detail| (Reason::SyntaxInvalid, detail))?,
        Some(("tel", number)) => validate_tel(number)
            .err()
            .map(|detail| (Reason::SyntaxInvalid, detail))?,
        _ => validate_local_link(link)?,
    };
    Some(Verdict {
        confidence: reason.confidence(),
        reason,
        evidence: vec![Evidence {
            kind: "local-link".into(),
            detail,
        }],
        checked_at: Utc::now(),
        tier: 0,
    })
}

fn contact_scheme(url: &str) -> Option<(&str, &str)> {
    let (scheme, remainder) = url.split_once(':')?;
    if scheme.eq_ignore_ascii_case("mailto") {
        Some(("mailto", remainder))
    } else if scheme.eq_ignore_ascii_case("tel") {
        Some(("tel", remainder))
    } else {
        None
    }
}

fn validate_mailto(address: &str) -> Result<(), String> {
    let valid = !address.is_empty()
        && !address.contains([' ', '?', '#'])
        && address.split('@').count() == 2
        && address.split_once('@').is_some_and(|(local, domain)| {
            !local.is_empty()
                && domain.contains('.')
                && !domain.starts_with('.')
                && !domain.ends_with('.')
        });
    valid
        .then_some(())
        .ok_or_else(|| "mailto address must be a plausible addr-spec".into())
}

fn validate_tel(number: &str) -> Result<(), String> {
    let valid = !number.is_empty()
        && number.chars().any(|character| character.is_ascii_digit())
        && number.chars().all(|character| {
            character.is_ascii_digit() || matches!(character, '+' | '-' | '(' | ')' | ' ')
        });
    valid
        .then_some(())
        .ok_or_else(|| "tel number may contain only digits, +, -, parentheses, and spaces".into())
}

fn validate_local_link(link: &FoundLink) -> Option<(Reason, String)> {
    let (path_and_query, raw_anchor) = link
        .url
        .split_once('#')
        .map_or((&*link.url, None), |(path, anchor)| (path, Some(anchor)));
    let path_part = path_and_query
        .split_once('?')
        .map_or(path_and_query, |(path, _)| path);
    let path_part = match percent_decode(path_part) {
        Ok(path) => path,
        Err(detail) => {
            return Some((
                Reason::SyntaxInvalid,
                format!("invalid percent escape in path: {detail}"),
            ));
        }
    };
    // Decode syntax before consulting the filesystem so malformed paths and
    // fragments have the same verdict regardless of target state.
    let anchor = match raw_anchor.map(percent_decode).transpose() {
        Ok(anchor) => anchor,
        Err(detail) => {
            return Some((
                Reason::SyntaxInvalid,
                format!("invalid percent escape in fragment: {detail}"),
            ));
        }
    };
    let target = if path_part.is_empty() {
        link.source.path.clone()
    } else {
        resolve_local_path(&link.source.path, &path_part)
    };
    if !target.exists() {
        return Some((
            Reason::LocalMissing,
            format!("resolved path does not exist: {}", target.display()),
        ));
    }
    let anchor = anchor?;
    if target.is_dir() {
        return Some((
            Reason::LocalMissing,
            format!(
                "anchors cannot be checked in directory: {}",
                target.display()
            ),
        ));
    }
    if anchor_exists(&target, &anchor) {
        None
    } else {
        Some((
            Reason::LocalMissing,
            format!("anchor not found: #{anchor} in {}", target.display()),
        ))
    }
}

fn resolve_local_path(source: &Path, raw_path: &str) -> PathBuf {
    let path = Path::new(raw_path);
    if path.is_absolute() || path.has_root() {
        path.to_path_buf()
    } else {
        source.parent().unwrap_or_else(|| Path::new(".")).join(path)
    }
}

fn percent_decode(value: &str) -> Result<String, String> {
    let mut bytes = Vec::with_capacity(value.len());
    let source = value.as_bytes();
    let mut index = 0;
    while index < source.len() {
        if source[index] == b'%' {
            let Some((&high, &low)) = source.get(index + 1).zip(source.get(index + 2)) else {
                return Err("incomplete escape".into());
            };
            let (Some(high), Some(low)) = (hex(high), hex(low)) else {
                return Err("non-hexadecimal escape".into());
            };
            bytes.push(high * 16 + low);
            index += 3;
            continue;
        }
        bytes.push(source[index]);
        index += 1;
    }
    String::from_utf8(bytes).map_err(|_| "invalid UTF-8".into())
}

fn hex(byte: u8) -> Option<u8> {
    match byte {
        b'0'..=b'9' => Some(byte - b'0'),
        b'a'..=b'f' => Some(byte - b'a' + 10),
        b'A'..=b'F' => Some(byte - b'A' + 10),
        _ => None,
    }
}

fn anchor_exists(path: &Path, anchor: &str) -> bool {
    let Ok(bytes) = std::fs::read(path) else {
        return false;
    };
    let Ok(text) = std::str::from_utf8(&bytes) else {
        return false;
    };
    match detect_format(path, &bytes) {
        Some(DocFormat::Markdown) => markdown_anchors(text)
            .iter()
            .any(|candidate| candidate == anchor),
        Some(DocFormat::Html) => html_anchors(text)
            .iter()
            .any(|candidate| candidate == anchor),
        _ => true,
    }
}

fn markdown_anchors(text: &str) -> Vec<String> {
    let mut anchors = html_anchors(text);
    let mut used = HashSet::new();
    let mut heading = None;
    for event in Parser::new(text) {
        match event {
            Event::Start(Tag::Heading { .. }) => heading = Some(String::new()),
            Event::Text(value) | Event::Code(value) => {
                if let Some(heading) = &mut heading {
                    heading.push_str(&value);
                }
            }
            Event::End(TagEnd::Heading(_)) => {
                if let Some(heading) = heading.take() {
                    let slug = slug(&heading);
                    let mut anchor = slug.clone();
                    let mut suffix = 1;
                    while used.contains(&anchor) {
                        anchor = format!("{slug}-{suffix}");
                        suffix += 1;
                    }
                    used.insert(anchor.clone());
                    anchors.push(anchor);
                }
            }
            _ => {}
        }
    }
    anchors
}

fn html_anchors(text: &str) -> Vec<String> {
    let parser =
        NaiveParser::new_with_emitter(PosTrackingReader::new(text), TracingEmitter::default());
    parser
        .flatten()
        .filter_map(|(token, _)| match token {
            Token::StartTag(tag) => Some(tag),
            _ => None,
        })
        .flat_map(|tag| {
            let is_anchor = tag.name.as_str().eq_ignore_ascii_case("a");
            tag.attributes.into_iter().filter_map(move |attribute| {
                (attribute.name.eq_ignore_ascii_case("id")
                    || (is_anchor && attribute.name.eq_ignore_ascii_case("name")))
                .then(|| attribute.value.to_owned())
            })
        })
        .collect()
}

fn slug(heading: &str) -> String {
    let mut slug = String::new();
    for character in heading.chars().flat_map(char::to_lowercase) {
        if character.is_whitespace() {
            slug.push('-');
        } else if character.is_alphanumeric() || character == '-' || character == '_' {
            slug.push(character);
        }
    }
    slug
}

const UNKNOWN_FILE_LIMIT: u64 = 2 * 1024 * 1024;

fn collect_links(
    paths_input: &[PathBuf],
    walk_opts: &WalkOptions,
) -> Result<(Vec<PathBuf>, Vec<FoundLink>), String> {
    let paths = walk(paths_input, walk_opts)?;
    let mut links = Vec::new();
    for path in &paths {
        let Some(bytes) = read_for_detection(path)? else {
            continue;
        };
        let Some(format) = detect_format(path, &bytes) else {
            continue;
        };
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

fn read_for_detection(path: &std::path::Path) -> Result<Option<Vec<u8>>, String> {
    let mut file = std::fs::File::open(path).map_err(|e| format!("{}: {e}", path.display()))?;
    let mut bytes = vec![0; 8 * 1024];
    let read = file
        .read(&mut bytes)
        .map_err(|e| format!("{}: {e}", path.display()))?;
    bytes.truncate(read);

    let has_binary_magic = bytes.starts_with(b"%PDF-") || bytes.starts_with(b"PK\x03\x04");
    // Unknown files have no user-declared format to preserve. Limit them before
    // allocating their complete contents; known extensions and magic keep their
    // full-read behavior so mis-extensioned supported documents still work.
    if extension_format(path).is_none()
        && !has_binary_magic
        && file
            .metadata()
            .map_err(|e| format!("{}: {e}", path.display()))?
            .len()
            > UNKNOWN_FILE_LIMIT
    {
        return Ok(None);
    }
    file.read_to_end(&mut bytes)
        .map_err(|e| format!("{}: {e}", path.display()))?;
    Ok(Some(bytes))
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

    fn input(path: &Path) -> ScanInput {
        ScanInput {
            paths: vec![path.into()],
            walk: WalkOptions::default(),
            max_concurrency: 1,
            exclude_urls: vec![],
            exclude_domains: vec![],
            check_local: true,
        }
    }

    #[tokio::test]
    async fn local_links_ignore_queries_and_decode_fragments_strictly() {
        let directory = tempfile::tempdir().unwrap();
        std::fs::write(directory.path().join("file.md"), "<a id=\"hello world\">\n").unwrap();
        std::fs::write(directory.path().join("hash#file.md"), "# Present\n").unwrap();
        std::fs::create_dir(directory.path().join("docs")).unwrap();
        std::fs::write(
            directory.path().join("source.md"),
            "[query](file.md?x=1)\n[fragment](file.md?x=1#hello%20world)\n[hash](hash%23file.md#present)\n[bad](file.md#%FF)\n[missing](missing.md#%FF)\n[directory](docs/#%FF)\n",
        )
        .unwrap();
        let calls = Arc::new(AtomicUsize::new(0));
        let report = scan(input(directory.path()), &Fake(calls.clone()), &NoProgress)
            .await
            .unwrap();
        assert_eq!(report.findings.len(), 3, "{:#?}", report.findings);
        for url in ["file.md#%FF", "missing.md#%FF", "docs/#%FF"] {
            let finding = report
                .findings
                .iter()
                .find(|finding| finding.url == url)
                .unwrap();
            assert_eq!(finding.verdict.reason, Reason::SyntaxInvalid);
            assert!(
                finding.verdict.evidence[0]
                    .detail
                    .contains("invalid percent escape in fragment: invalid UTF-8")
            );
        }
        assert_eq!(calls.load(Ordering::SeqCst), 0);
    }

    #[tokio::test]
    async fn local_anchor_parsers_follow_rendered_markdown_and_html_structure() {
        let directory = tempfile::tempdir().unwrap();
        std::fs::write(
            directory.path().join("target.md"),
            "# Same\n# Same\n# *Emphasis* and `code` [link](#same)!!!\n# Unicode cafe\u{301}\n<div data-id=\"fake\"></div><!-- <a id=\"comment\"> --><A ID=unquoted><a NaMe=\"named&amp;anchor\">\n",
        )
        .unwrap();
        std::fs::write(
            directory.path().join("source.md"),
            "[one](target.md#same) [two](target.md#same-1) [rendered](target.md#emphasis-and-code-link) [unicode](target.md#unicode-cafe) [unquoted](target.md#unquoted) [named](target.md#named%26anchor) [fake](target.md#fake) [comment](target.md#comment)",
        )
        .unwrap();
        let report = scan(
            input(directory.path()),
            &Fake(Arc::new(AtomicUsize::new(0))),
            &NoProgress,
        )
        .await
        .unwrap();
        assert_eq!(report.findings.len(), 2, "{:#?}", report.findings);
        assert!(
            report
                .findings
                .iter()
                .all(|finding| finding.verdict.reason == Reason::LocalMissing)
        );
        assert!(
            report
                .findings
                .iter()
                .any(|finding| finding.url == "target.md#fake")
        );
        assert!(
            report
                .findings
                .iter()
                .any(|finding| finding.url == "target.md#comment")
        );
    }

    #[test]
    fn markdown_heading_slugs_match_github_rules_and_reserve_collisions() {
        assert_eq!(
            markdown_anchors(
                "# Same\n# Same-1\n# Same\n# What's & This?\n# 2024 Plan\n# Repeated   whitespace\n# !!!\n# ???\n# Rust \u{1f980} Caf\u{e9}\n"
            ),
            [
                "same",
                "same-1",
                "same-2",
                "whats--this",
                "2024-plan",
                "repeated---whitespace",
                "",
                "-1",
                "rust--caf\u{e9}",
            ]
        );
    }

    #[tokio::test]
    async fn local_links_resolve_github_style_unicode_heading_slugs() {
        let directory = tempfile::tempdir().unwrap();
        std::fs::write(
            directory.path().join("target.md"),
            "# Rust \u{1f980} Crab\n# Apostrophe\u{2019}s trimmed\n# Caf\u{e9} menu\n",
        )
        .unwrap();
        std::fs::write(
            directory.path().join("source.md"),
            "[emoji](target.md#rust--crab) [apostrophe](target.md#apostrophes-trimmed) [accented](target.md#caf\u{e9}-menu)",
        )
        .unwrap();

        let report = scan(
            input(directory.path()),
            &Fake(Arc::new(AtomicUsize::new(0))),
            &NoProgress,
        )
        .await
        .unwrap();

        assert!(report.findings.is_empty(), "{:#?}", report.findings);
    }

    #[tokio::test]
    async fn uppercase_contact_schemes_are_validated_without_checker_calls() {
        let directory = tempfile::tempdir().unwrap();
        std::fs::write(
            directory.path().join("source.md"),
            "[mail](MAILTO:valid@example.test) [tel](TEL:+1-555-0100) [bad mail](MAILTO:not-an-address) [bad tel](TEL:abc)",
        )
        .unwrap();
        let calls = Arc::new(AtomicUsize::new(0));
        let report = scan(input(directory.path()), &Fake(calls.clone()), &NoProgress)
            .await
            .unwrap();
        assert_eq!(report.findings.len(), 2);
        assert!(
            report
                .findings
                .iter()
                .all(|finding| finding.verdict.reason == Reason::SyntaxInvalid)
        );
        assert_eq!(calls.load(Ordering::SeqCst), 0);
    }

    #[tokio::test]
    async fn fragmentless_directory_links_are_clean_but_fragmented_ones_are_not() {
        let directory = tempfile::tempdir().unwrap();
        std::fs::create_dir(directory.path().join("docs")).unwrap();
        std::fs::write(
            directory.path().join("source.md"),
            "[directory](docs/) [directory anchor](docs/#section)",
        )
        .unwrap();
        let report = scan(
            input(directory.path()),
            &Fake(Arc::new(AtomicUsize::new(0))),
            &NoProgress,
        )
        .await
        .unwrap();
        assert_eq!(report.findings.len(), 1);
        assert_eq!(report.findings[0].url, "docs/#section");
        assert_eq!(report.findings[0].verdict.reason, Reason::LocalMissing);
        assert!(
            report.findings[0].verdict.evidence[0]
                .detail
                .contains("anchors cannot be checked in directory")
        );
    }

    #[tokio::test]
    async fn reference_style_local_links_report_missing_destinations() {
        let directory = tempfile::tempdir().unwrap();
        std::fs::write(
            directory.path().join("source.md"),
            "[one][missing] [two][missing]\n\n[missing]: absent.md\n",
        )
        .unwrap();
        let report = scan(
            input(directory.path()),
            &Fake(Arc::new(AtomicUsize::new(0))),
            &NoProgress,
        )
        .await
        .unwrap();
        assert_eq!(report.findings.len(), 1);
        assert_eq!(report.findings[0].url, "absent.md");
        assert_eq!(report.findings[0].verdict.reason, Reason::LocalMissing);
    }

    #[tokio::test]
    async fn root_relative_reference_links_are_validated() {
        let directory = tempfile::tempdir().unwrap();
        let missing = directory.path().join("missing.md");
        let filesystem_root = missing.ancestors().last().unwrap();
        let missing = format!(
            "/{}",
            missing
                .strip_prefix(filesystem_root)
                .unwrap()
                .to_string_lossy()
                .replace('\\', "/")
        );
        std::fs::write(
            directory.path().join("source.md"),
            format!("[missing][target]\n\n[target]: {missing}\n"),
        )
        .unwrap();
        let report = scan(
            input(directory.path()),
            &Fake(Arc::new(AtomicUsize::new(0))),
            &NoProgress,
        )
        .await
        .unwrap();
        assert_eq!(report.findings.len(), 1);
        assert_eq!(report.findings[0].url, missing);
        assert_eq!(report.findings[0].verdict.reason, Reason::LocalMissing);
    }

    #[tokio::test]
    async fn skips_unknown_text_like_binary_after_the_probe_prefix() {
        let directory = tempfile::tempdir().unwrap();
        std::fs::write(directory.path().join("real.txt"), "https://real.test/x").unwrap();
        let mut binary = vec![b'a'; 8 * 1024];
        binary.push(0xff);
        std::fs::write(directory.path().join("unknown"), binary).unwrap();
        let calls = Arc::new(AtomicUsize::new(0));
        let report = scan(
            ScanInput {
                paths: vec![directory.path().into()],
                walk: WalkOptions::default(),
                max_concurrency: 1,
                exclude_urls: vec![],
                exclude_domains: vec![],
                check_local: true,
            },
            &Fake(calls.clone()),
            &NoProgress,
        )
        .await
        .unwrap();
        assert_eq!(report.findings.len(), 1);
        assert_eq!(report.findings[0].url, "https://real.test/x");
        assert_eq!(calls.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn skips_large_unknown_files_but_scans_large_known_formats() {
        let directory = tempfile::tempdir().unwrap();
        std::fs::write(
            directory.path().join("large.unknown"),
            vec![0; 3 * 1024 * 1024],
        )
        .unwrap();
        let mut text = vec![b'a'; 3 * 1024 * 1024];
        text.extend_from_slice(b" https://large-known.test/x");
        std::fs::write(directory.path().join("large.txt"), text).unwrap();
        let calls = Arc::new(AtomicUsize::new(0));
        let report = scan(
            ScanInput {
                paths: vec![directory.path().into()],
                walk: WalkOptions::default(),
                max_concurrency: 1,
                exclude_urls: vec![],
                exclude_domains: vec![],
                check_local: true,
            },
            &Fake(calls.clone()),
            &NoProgress,
        )
        .await
        .unwrap();
        assert_eq!(report.findings.len(), 1);
        assert_eq!(report.findings[0].url, "https://large-known.test/x");
        assert_eq!(calls.load(Ordering::SeqCst), 1);
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
                check_local: true,
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
                    check_local: true,
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
        zip.write_all(br#"<w:document xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main"><w:body><w:p><w:r><w:instrText>HYPERLINK &quot;https://mixed.test/docx&quot;</w:instrText></w:r></w:p></w:body></w:document>"#).unwrap();
        let docx = zip.finish().unwrap().into_inner();
        std::fs::write(directory.path().join("four.bin"), docx).unwrap();
        let mut zip = zip::ZipWriter::new(std::io::Cursor::new(Vec::new()));
        for (name, xml) in [
            (
                "xl/workbook.xml",
                r#"<workbook xmlns="http://schemas.openxmlformats.org/spreadsheetml/2006/main" xmlns:r="http://schemas.openxmlformats.org/officeDocument/2006/relationships"><sheets><sheet name="S" r:id="rId1"/></sheets></workbook>"#,
            ),
            (
                "xl/_rels/workbook.xml.rels",
                r#"<Relationships><Relationship Id="rId1" Target="worksheets/sheet1.xml"/></Relationships>"#,
            ),
            (
                "xl/worksheets/sheet1.xml",
                r#"<worksheet xmlns="http://schemas.openxmlformats.org/spreadsheetml/2006/main"><sheetData><row><c r="A1"><f>HYPERLINK(&quot;https://mixed.test/xlsx&quot;)</f></c></row></sheetData></worksheet>"#,
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
                r#"<p:sld xmlns:p="http://schemas.openxmlformats.org/presentationml/2006/main" xmlns:a="http://schemas.openxmlformats.org/drawingml/2006/main" xmlns:r="http://schemas.openxmlformats.org/officeDocument/2006/relationships"><p:cNvPr><a:hlinkClick r:id="rId1"/></p:cNvPr></p:sld>"#,
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
                check_local: true,
            },
            &Fake(calls.clone()),
            &NoProgress,
        )
        .await
        .unwrap();
        assert_eq!(report.files_scanned, 7);
        assert_eq!(report.findings.len(), 7, "{:#?}", report.findings);
        assert_eq!(calls.load(Ordering::SeqCst), 7);
        let found = |url: &str, format, location| {
            report.findings.iter().any(|finding| {
                finding.url == url
                    && finding.source.format == format
                    && finding.source.location == location
            })
        };
        assert!(found(
            "https://mixed.test/markdown",
            crate::model::DocFormat::Markdown,
            crate::model::Location::Text { line: 1, column: 5 }
        ));
        assert!(found(
            "https://mixed.test/html",
            crate::model::DocFormat::Html,
            crate::model::Location::Text {
                line: 1,
                column: 10
            }
        ));
        assert!(found(
            "https://mixed.test/text",
            crate::model::DocFormat::Text,
            crate::model::Location::Text { line: 1, column: 1 }
        ));
        assert!(found(
            "https://mixed.test/docx",
            crate::model::DocFormat::Docx,
            crate::model::Location::Docx { paragraph: 1 }
        ));
        assert!(found(
            "https://mixed.test/xlsx",
            crate::model::DocFormat::Xlsx,
            crate::model::Location::Xlsx {
                sheet: "S".into(),
                cell: "A1".into()
            }
        ));
        assert!(found(
            "https://mixed.test/pptx",
            crate::model::DocFormat::Pptx,
            crate::model::Location::Pptx { slide: 1 }
        ));
        assert!(found(
            "https://mixed.test/pdf",
            crate::model::DocFormat::Pdf,
            crate::model::Location::Pdf {
                page: 1,
                annotation: None
            }
        ));
    }

    #[tokio::test]
    async fn skips_unknown_binary_files_but_scans_extensionless_text() {
        let directory = tempfile::tempdir().unwrap();
        std::fs::write(
            directory.path().join("plain"),
            "https://extensionless.test/x",
        )
        .unwrap();
        std::fs::write(directory.path().join("garbage"), [0, 0xff, 1]).unwrap();
        let mut zip = zip::ZipWriter::new(std::io::Cursor::new(Vec::new()));
        zip.start_file("unrelated.txt", zip::write::SimpleFileOptions::default())
            .unwrap();
        zip.write_all(b"https://inside-zip.test/x").unwrap();
        std::fs::write(
            directory.path().join("archive"),
            zip.finish().unwrap().into_inner(),
        )
        .unwrap();
        let calls = Arc::new(AtomicUsize::new(0));
        let report = scan(
            ScanInput {
                paths: vec![directory.path().into()],
                walk: WalkOptions::default(),
                max_concurrency: 1,
                exclude_urls: vec![],
                exclude_domains: vec![],
                check_local: true,
            },
            &Fake(calls.clone()),
            &NoProgress,
        )
        .await
        .unwrap();
        assert_eq!(report.findings.len(), 1);
        assert_eq!(calls.load(Ordering::SeqCst), 1);
    }
}
