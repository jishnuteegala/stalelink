mod auth;
mod cache;
mod config;
mod output;

use std::{
    collections::BTreeMap,
    fs,
    fs::File,
    io::{self, IsTerminal, Read, Write},
    path::{Path, PathBuf},
    process::ExitCode,
};

use clap::{ArgAction, Args, CommandFactory, Parser, Subcommand, ValueEnum};
use clap_complete::{Shell, generate};
use regex::Regex;
use stalelink_core::{
    check::{AuthCap, EscalatingChecker, HttpChecker},
    extract::{SourceDocument, extract},
    fix::{fixer_for, pdf_refusal},
    model::{Confidence, DocFormat, Finding, FixOrigin, Fixability},
    report::{ReportSink, TableSink},
    scan::{NoProgress, Progress, ScanInput, scan},
    walk::{WalkOptions, detect_format},
};
use tempfile::NamedTempFile;

const CLEAN: u8 = 0;
const USAGE: u8 = 2;
const ENVIRONMENT: u8 = 3;

#[derive(Debug, Parser)]
#[command(
    name = "stalelink",
    version,
    about = "Find stale links in local documents"
)]
#[command(after_help = "Examples:\n  stalelink scan docs/\n  stalelink fix report.md --write")]
struct Cli {
    /// Suppress progress output on stderr
    #[arg(short, long, global = true)]
    quiet: bool,
    /// Per-URL detail on stderr (repeat for more)
    #[arg(short, long, action = ArgAction::Count, global = true)]
    verbose: u8,
    /// When to use colored output
    #[arg(long, value_enum, default_value_t = Color::Auto, global = true)]
    color: Color,
    #[command(subcommand)]
    command: Command,
}

#[derive(Debug, Subcommand)]
enum Command {
    /// Check links in documents and report findings (read-only)
    Scan(ScanArgs),
    /// Scan and apply link fixes (prints a diff unless --write)
    Fix(FixArgs),
    /// Manage the response cache
    Cache {
        #[command(subcommand)]
        command: CacheCommand,
    },
    /// Generate shell completions
    Completions { shell: Shell },
}

#[derive(Debug, Clone, Args)]
struct ScanArgs {
    #[command(flatten)]
    common: CommonArgs,
    #[command(flatten)]
    output: OutputArgs,
}

#[derive(Debug, Clone, Args)]
struct FixArgs {
    #[command(flatten)]
    common: CommonArgs,
    /// Apply fixes in place (default prints a diff)
    #[arg(long)]
    write: bool,
    /// With --write, keep the original as a .bak file
    #[arg(long, requires = "write", conflicts_with = "copy")]
    backup: bool,
    /// Write fixed copies as *.fixed.* instead of in place
    #[arg(long, conflicts_with = "write")]
    copy: bool,
    /// Skip a category of automatic fixes
    #[arg(long, value_enum)]
    fix_exclude: Vec<FixExclude>,
    /// Only apply fixes at or above this confidence
    #[arg(long, value_enum, default_value_t = ConfidenceLevel::DeadCertain)]
    min_fix_confidence: ConfidenceLevel,
}

#[derive(Debug, Clone, Args)]
struct CommonArgs {
    /// Files and/or directories to scan (recursive, format auto-detect)
    #[arg(value_name = "PATHS", required_unless_present = "stdin")]
    paths: Vec<PathBuf>,
    /// Read a newline-delimited file list from stdin
    #[arg(long)]
    stdin: bool,
    /// Restrict walked files to these globs
    #[arg(long)]
    include: Vec<String>,
    /// Exclude files matching these gitignore-style globs
    #[arg(long)]
    exclude: Vec<String>,
    /// Skip URLs matching these regexes
    #[arg(long)]
    exclude_url: Vec<String>,
    /// Skip these domains (including subdomains)
    #[arg(long)]
    exclude_domain: Vec<String>,
    /// Skip file-path, anchor, mailto, tel, and relative-link validation
    #[arg(long)]
    no_local: bool,
    #[command(flatten)]
    network: NetworkArgs,
    #[command(flatten)]
    auth: AuthArgs,
}

#[derive(Debug, Clone, Args)]
struct OutputArgs {
    /// Findings format on stdout
    #[arg(long, value_enum, default_value_t = OutputFormat::Table)]
    format: OutputFormat,
    /// Shorthand for --format json
    #[arg(long, conflicts_with = "format")]
    json: bool,
    /// Write findings to a file instead of stdout
    #[arg(short, long)]
    output: Option<PathBuf>,
    /// Report only findings at or above this confidence
    #[arg(long, value_enum, default_value_t = ConfidenceLevel::Suspect)]
    min_confidence: ConfidenceLevel,
    /// Exit 1 only for findings at or above this confidence
    #[arg(long, value_enum)]
    fail_on: Option<ConfidenceLevel>,
}

#[derive(Debug, Clone, Args)]
struct NetworkArgs {
    /// Global request concurrency
    #[arg(long)]
    max_concurrency: Option<u16>,
    /// Concurrent requests per host
    #[arg(long)]
    per_host: Option<u16>,
    /// Per-request timeout in seconds
    #[arg(long)]
    timeout: Option<u64>,
    /// Retries per request
    #[arg(long)]
    retries: Option<u8>,
    /// Custom User-Agent header
    #[arg(long)]
    user_agent: Option<String>,
    /// Cache entry validity window (e.g. 30m, 12h, 7d)
    #[arg(long)]
    cache_ttl: Option<String>,
    /// Bypass the response cache entirely
    #[arg(long)]
    no_cache: bool,
    /// Ignore cached entries but still write new ones
    #[arg(long)]
    refresh: bool,
}

#[derive(Debug, Clone, Args)]
struct AuthArgs {
    /// Maximum auth tier: off = plain HTTP, cookies = browser cookies, browser = real browser
    #[arg(long, value_enum)]
    auth: Option<Auth>,
    /// Which browser's cookies/profile to use
    #[arg(long, value_enum, default_value_t = Browser::Auto)]
    browser: Browser,
    /// Connect tier 3 to this Chromium debugging endpoint instead of launching a profile
    #[arg(long, requires = "auth")]
    cdp_url: Option<String>,
}

#[derive(Debug, Subcommand)]
enum CacheCommand {
    /// Delete the response cache
    Clear,
    /// Show cache size and hit statistics
    Stats,
}

#[derive(Debug, Clone, Copy, ValueEnum)]
enum Color {
    Auto,
    Always,
    Never,
}

#[derive(Debug, Clone, Copy, ValueEnum)]
enum OutputFormat {
    Table,
    Json,
    Sarif,
}

#[derive(Debug, Clone, Copy, ValueEnum)]
enum Auth {
    Off,
    Cookies,
    Browser,
}

#[derive(Debug, Clone, Copy, ValueEnum)]
enum Browser {
    Auto,
    Chrome,
    Edge,
    Brave,
    Chromium,
    Firefox,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
enum FixExclude {
    Pdf,
    Redirect,
    UrlUpgrade,
}

#[derive(Debug, Clone, Copy, ValueEnum)]
enum ConfidenceLevel {
    DeadCertain,
    LikelyDead,
    AuthWalled,
    Outdated,
    Suspect,
}

impl From<ConfidenceLevel> for Confidence {
    fn from(value: ConfidenceLevel) -> Self {
        match value {
            ConfidenceLevel::DeadCertain => Self::DeadCertain,
            ConfidenceLevel::LikelyDead => Self::LikelyDead,
            ConfidenceLevel::AuthWalled => Self::AuthWalled,
            ConfidenceLevel::Outdated => Self::Outdated,
            ConfidenceLevel::Suspect => Self::Suspect,
        }
    }
}

impl std::str::FromStr for ConfidenceLevel {
    type Err = ();

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        match value {
            "dead-certain" => Ok(Self::DeadCertain),
            "likely-dead" => Ok(Self::LikelyDead),
            "auth-walled" => Ok(Self::AuthWalled),
            "outdated" => Ok(Self::Outdated),
            "suspect" => Ok(Self::Suspect),
            _ => Err(()),
        }
    }
}

struct StderrProgress;
impl Progress for StderrProgress {
    fn files_walked(&self, count: usize) {
        eprint!("\r{count} files walked");
    }
    fn links_found(&self, count: usize) {
        eprint!("\r{count} links found");
    }
    fn checks_done(&self, count: usize) {
        eprint!("\r{count} links checked");
        let _ = io::stderr().flush();
    }
}

fn main() -> ExitCode {
    match Cli::try_parse() {
        Ok(cli) => run(cli),
        Err(error) => {
            let _ = error.print();
            if error.use_stderr() {
                ExitCode::from(USAGE)
            } else {
                ExitCode::from(CLEAN)
            }
        }
    }
}

fn run(cli: Cli) -> ExitCode {
    match cli.command {
        Command::Completions { shell } => {
            let mut command = Cli::command();
            generate(shell, &mut command, "stalelink", &mut io::stdout());
            ExitCode::from(CLEAN)
        }
        Command::Scan(args) => run_scan(args, cli.quiet),
        Command::Cache { command } => run_cache(command),
        Command::Fix(args) => run_fix(args, cli.quiet),
    }
}

fn run_scan(args: ScanArgs, quiet: bool) -> ExitCode {
    let (mut report, fail_on) = match scan_common(&args.common, Some(&args.output), quiet) {
        Ok(report) => report,
        Err(exit_code) => return ExitCode::from(exit_code),
    };
    let failed = report
        .findings
        .iter()
        .any(|finding| finding.verdict.confidence >= fail_on);
    let minimum = Confidence::from(args.output.min_confidence);
    report
        .findings
        .retain(|finding| finding.verdict.confidence >= minimum);
    let format = if args.output.json {
        OutputFormat::Json
    } else {
        args.output.format
    };
    let exit_code = if failed { 1 } else { CLEAN };
    let result = if let Some(path) = args.output.output {
        File::create(path).and_then(|file| write_report(file, format, &report, exit_code))
    } else {
        write_report(io::stdout(), format, &report, exit_code)
    };
    if let Err(error) = result {
        eprintln!("error: writing report: {error}");
        return ExitCode::from(ENVIRONMENT);
    }
    ExitCode::from(exit_code)
}

fn scan_common(
    args: &CommonArgs,
    output: Option<&OutputArgs>,
    quiet: bool,
) -> Result<(stalelink_core::scan::ScanReport, Confidence), u8> {
    let mut paths = args.paths.clone();
    if args.stdin {
        let mut input = String::new();
        if let Err(error) = io::stdin().read_to_string(&mut input) {
            eprintln!("error: reading stdin: {error}");
            return Err(ENVIRONMENT);
        }
        paths.extend(
            input
                .lines()
                .filter(|line| !line.trim().is_empty())
                .map(PathBuf::from),
        );
    }
    // A scan uses the nearest config above its first input path; stdin paths are
    // appended after positional paths, so stdin-only scans use their first line.
    let first_path = paths.first().cloned().unwrap_or_else(|| PathBuf::from("."));
    let network = &args.network;
    let mut settings = match config::resolve(&first_path) {
        Ok(settings) => settings,
        Err(error) => {
            eprintln!("error: {error}");
            return Err(USAGE);
        }
    };
    if let Some(value) = network.max_concurrency {
        settings.network.max_concurrency = value;
    }
    if let Some(value) = network.per_host {
        settings.network.per_host = value;
    }
    if let Some(value) = network.timeout {
        settings.network.timeout = std::time::Duration::from_secs(value);
    }
    if let Some(value) = network.retries {
        settings.network.retries = value;
    }
    if let Some(value) = &network.user_agent {
        settings.network.user_agent = Some(value.clone());
    }
    if let Some(value) = &network.cache_ttl {
        settings.cache.ttl = match humantime::parse_duration(value) {
            Ok(ttl) => ttl,
            Err(error) => {
                eprintln!("error: invalid --cache-ttl: {error}");
                return Err(USAGE);
            }
        };
    }
    if args.no_local {
        settings.ignore.local_links = true;
    }
    let fail_on = match output.and_then(|output| output.fail_on) {
        Some(value) => value,
        None => match settings.output.fail_on.parse::<ConfidenceLevel>() {
            Ok(value) => value,
            Err(_) => {
                eprintln!(
                    "error: invalid output.fail-on value: {}",
                    settings.output.fail_on
                );
                return Err(USAGE);
            }
        },
    };
    if settings.network.max_concurrency == 0 || settings.network.per_host == 0 {
        eprintln!("error: --max-concurrency and --per-host must be at least 1");
        return Err(USAGE);
    }
    // Validate argument values (usage errors) before any path or environment
    // check, so a bad regex reports exit 2 regardless of whether a path exists.
    let exclude_urls = match args
        .exclude_url
        .iter()
        .chain(settings.ignore.exclude_url.iter())
        .map(|value| Regex::new(value))
        .collect::<Result<Vec<_>, _>>()
    {
        Ok(regexes) => regexes,
        Err(error) => {
            eprintln!("error: invalid --exclude-url regex: {error}");
            return Err(USAGE);
        }
    };
    for path in &paths {
        if !path.exists() {
            eprintln!("error: path does not exist: {}", path.display());
            return Err(ENVIRONMENT);
        }
    }
    let tier1 = match HttpChecker::new(
        settings.network.timeout,
        settings.network.retries,
        settings.network.per_host as usize,
        settings
            .network
            .user_agent
            .clone()
            .unwrap_or_else(|| format!("stalelink/{}", env!("CARGO_PKG_VERSION"))),
    ) {
        Ok(checker) => checker,
        Err(error) => {
            eprintln!("error: creating HTTP client: {error}");
            return Err(ENVIRONMENT);
        }
    };
    let requested_auth = args.auth.auth;
    let configured_auth = match settings.auth.auth.as_str() {
        "off" => Auth::Off,
        "browser" => Auth::Browser,
        _ => Auth::Cookies,
    };
    let selected_auth = requested_auth.unwrap_or(configured_auth);
    let browser = match args.auth.browser {
        Browser::Auto => auth::Browser::Auto,
        Browser::Chrome => auth::Browser::Chrome,
        Browser::Edge => auth::Browser::Edge,
        Browser::Brave => auth::Browser::Brave,
        Browser::Chromium => auth::Browser::Chromium,
        Browser::Firefox => auth::Browser::Firefox,
    };
    let cap = match selected_auth {
        Auth::Off => AuthCap::Off,
        Auth::Cookies => AuthCap::Cookies,
        Auth::Browser => AuthCap::Browser,
    };
    let snapshot = if matches!(requested_auth, Some(Auth::Cookies)) {
        match auth::snapshot(browser) {
            Ok(snapshot) if !snapshot.is_empty() => {
                eprintln!(
                    "notice: reading {} browser-profile cookies for escalated links",
                    browser.name()
                );
                Some(snapshot)
            }
            Ok(_) => {
                eprintln!(
                    "warning: {} has no readable cookie store; check its profile or use --auth off",
                    browser.name()
                );
                if matches!(requested_auth, Some(Auth::Cookies)) {
                    return Err(ENVIRONMENT);
                }
                None
            }
            Err(error) => {
                #[cfg(windows)]
                eprintln!(
                    "warning: {} cookie store is unavailable ({error}); Chrome app-bound cookies may require elevation. Run from an elevated prompt or choose another browser",
                    browser.name()
                );
                #[cfg(not(windows))]
                eprintln!(
                    "warning: {} cookie store is unavailable ({error}); close the browser or choose another browser",
                    browser.name()
                );
                if matches!(requested_auth, Some(Auth::Cookies)) {
                    return Err(ENVIRONMENT);
                }
                None
            }
        }
    } else {
        None
    };
    let user_agent = settings
        .network
        .user_agent
        .clone()
        .unwrap_or_else(|| format!("stalelink/{}", env!("CARGO_PKG_VERSION")));
    let tier2 = if let Some(snapshot) = snapshot {
        auth::CookieChecker::new(settings.network.timeout, user_agent.clone(), snapshot).ok()
    } else if cap.tier() >= 2 {
        auth::CookieChecker::from_browser(settings.network.timeout, user_agent.clone(), browser)
            .ok()
    } else {
        None
    };
    let cache_path = cache_path(settings.cache.dir.as_deref());
    let input = ScanInput {
        paths,
        walk: WalkOptions {
            include: args.include.clone(),
            exclude: args
                .exclude
                .clone()
                .into_iter()
                .chain(settings.ignore.exclude)
                .collect(),
        },
        max_concurrency: settings.network.max_concurrency as usize,
        exclude_urls,
        exclude_domains: args
            .exclude_domain
            .clone()
            .into_iter()
            .chain(settings.ignore.exclude_domain)
            .map(|domain| domain.to_ascii_lowercase())
            .collect(),
        check_local: !settings.ignore.local_links,
    };
    let runtime = match tokio::runtime::Runtime::new() {
        Ok(runtime) => runtime,
        Err(error) => {
            eprintln!("error: creating runtime: {error}");
            return Err(ENVIRONMENT);
        }
    };
    #[cfg(feature = "live-browser")]
    let tier3 = if matches!(selected_auth, Auth::Browser) {
        let profile = cache_path.join("browser-profile");
        match runtime.block_on(auth::CdpPageDriver::launch(
            &profile,
            args.auth.cdp_url.as_deref(),
        )) {
            Ok(drivers) => Some(auth::BrowserChecker::new(drivers)),
            Err(error) => {
                eprintln!(
                    "warning: browser tier is unavailable ({error}); start Chromium with remote debugging or check its installation"
                );
                if matches!(requested_auth, Some(Auth::Browser)) {
                    return Err(ENVIRONMENT);
                }
                None
            }
        }
    } else {
        None
    };
    #[cfg(not(feature = "live-browser"))]
    if matches!(selected_auth, Auth::Browser) {
        eprintln!(
            "warning: browser tier is unavailable; install a live-browser build or use --auth cookies"
        );
        if matches!(requested_auth, Some(Auth::Browser)) {
            return Err(ENVIRONMENT);
        }
    }
    #[cfg(feature = "live-browser")]
    let checker = EscalatingChecker::new(tier1, tier2, tier3, cap);
    #[cfg(not(feature = "live-browser"))]
    let checker = EscalatingChecker::new(tier1, tier2, Option::<auth::CookieChecker>::None, cap);
    let show_progress = !quiet && io::stderr().is_terminal();
    let result = if network.no_cache {
        if show_progress {
            runtime.block_on(scan(input, &checker, &StderrProgress))
        } else {
            runtime.block_on(scan(input, &checker, &NoProgress))
        }
    } else {
        let cache = match cache::VerdictCache::open(cache_path) {
            Ok(cache) => cache,
            Err(error) => {
                eprintln!("error: {error}");
                return Err(ENVIRONMENT);
            }
        };
        let checker = cache::CachingChecker::new(
            checker,
            cache,
            settings.cache.ttl,
            cap.tier(),
            network.refresh,
        );
        let result = if show_progress {
            runtime.block_on(scan(input, &checker, &StderrProgress))
        } else {
            runtime.block_on(scan(input, &checker, &NoProgress))
        };
        if let Some(error) = checker.error() {
            eprintln!("error: {error}");
            return Err(ENVIRONMENT);
        }
        result
    };
    if show_progress {
        eprintln!();
    }
    match result {
        Ok(report) => Ok((report, Confidence::from(fail_on))),
        Err(error) => {
            eprintln!("error: {error}");
            Err(ENVIRONMENT)
        }
    }
}

fn run_fix(args: FixArgs, quiet: bool) -> ExitCode {
    let report = match scan_common(&args.common, None, quiet) {
        Ok((report, _)) => report,
        Err(exit_code) => return ExitCode::from(exit_code),
    };
    let minimum = Confidence::from(args.min_fix_confidence);
    let (preflight_refused, mut refused) = preflight_pdfs(&args, &report.resolved_paths);
    let mut by_path: BTreeMap<PathBuf, Vec<Finding>> = BTreeMap::new();
    for finding in report.findings {
        let Some(fix) = &finding.fix else { continue };
        if finding.verdict.confidence < minimum
            || excluded(finding.source.format, fix.origin, &args.fix_exclude)
        {
            continue;
        }
        if !matches!(fix.fixable, Fixability::Auto) {
            eprintln!(
                "refused {}: {}",
                finding.source.path.display(),
                match &fix.fixable {
                    Fixability::Manual => "fix requires manual editing",
                    Fixability::Refused { reason } => reason,
                    Fixability::Auto => unreachable!("handled above"),
                }
            );
            refused += 1;
            continue;
        }
        if preflight_refused.contains(&finding.source.path) {
            continue;
        }
        by_path
            .entry(finding.source.path.clone())
            .or_default()
            .push(finding);
    }
    for (path, findings) in by_path {
        let original = match fs::read(&path) {
            Ok(bytes) => bytes,
            Err(error) => {
                eprintln!("failed {}: reading: {error}", path.display());
                refused += 1;
                continue;
            }
        };
        let format = findings[0].source.format;
        let fixed = match fixer_for(format)
            .expect("text format has a fixer")
            .fix(&original, &findings)
        {
            Ok(bytes) => bytes,
            Err(error) => {
                eprintln!("refused {}: {}", path.display(), error.0);
                refused += 1;
                continue;
            }
        };
        if !args.write && !args.copy {
            if matches!(
                format,
                DocFormat::Pdf | DocFormat::Docx | DocFormat::Xlsx | DocFormat::Pptx
            ) {
                print_binary_summary(&path, &findings);
            } else {
                print_diff(&path, &original, &fixed);
            }
            continue;
        }
        let destination = if args.copy {
            fixed_copy_path(&path)
        } else {
            path.clone()
        };
        let result = if args.copy {
            write_new_file(&destination, &fixed).and_then(|()| {
                fs::read(&destination)
                    .map_err(|error| format!("reading written copy: {error}"))
                    .and_then(|written| verify_fixed(&destination, format, &written, &findings))
            })
        } else {
            write_in_place(&path, &original, &fixed, args.backup, |bytes| {
                verify_fixed(&path, format, bytes, &findings)
            })
        };
        if let Err(error) = result {
            eprintln!("failed {}: {error}", destination.display());
            refused += 1;
        }
    }
    ExitCode::from(if refused == 0 { CLEAN } else { 1 })
}

fn excluded(format: DocFormat, origin: FixOrigin, exclusions: &[FixExclude]) -> bool {
    exclusions.iter().any(|exclusion| match exclusion {
        FixExclude::Pdf => format == DocFormat::Pdf,
        FixExclude::Redirect => origin == FixOrigin::RedirectTarget,
        FixExclude::UrlUpgrade => {
            matches!(origin, FixOrigin::HttpsUpgrade | FixOrigin::VersionUpgrade)
        }
    })
}

fn preflight_pdfs(
    args: &FixArgs,
    paths: &[PathBuf],
) -> (std::collections::HashSet<PathBuf>, usize) {
    if args.fix_exclude.contains(&FixExclude::Pdf) {
        return (std::collections::HashSet::new(), 0);
    };
    let mut refused = 0;
    let mut refused_paths = std::collections::HashSet::new();
    for path in paths {
        let Ok(bytes) = fs::read(path) else { continue };
        if detect_format(path, &bytes) != Some(DocFormat::Pdf) {
            continue;
        }
        let result = lopdf::Document::load_mem(&bytes)
            .map_err(|error| format!("reading PDF: {error}"))
            .and_then(|document| pdf_refusal(&document).map_err(|error| error.0));
        if let Err(error) = result {
            eprintln!("refused {}: {error}", path.display());
            refused += 1;
            refused_paths.insert(path.clone());
        }
    }
    (refused_paths, refused)
}

fn print_binary_summary(path: &Path, findings: &[Finding]) {
    for finding in findings {
        let replacement = &finding
            .fix
            .as_ref()
            .expect("selected finding has a fix")
            .replacement_url;
        println!("{}: {} -> {}", path.display(), finding.url, replacement);
    }
}

fn print_diff(path: &Path, original: &[u8], fixed: &[u8]) {
    let original = String::from_utf8_lossy(original);
    let fixed = String::from_utf8_lossy(fixed);
    let diff = similar::TextDiff::from_lines(&original, &fixed)
        .unified_diff()
        .header(
            &format!("a/{}", path.display()),
            &format!("b/{}", path.display()),
        )
        .to_string();
    print!("{diff}");
}

fn fixed_copy_path(path: &Path) -> PathBuf {
    let stem = path.file_stem().unwrap_or_default().to_string_lossy();
    match path.extension() {
        Some(extension) => {
            path.with_file_name(format!("{stem}.fixed.{}", extension.to_string_lossy()))
        }
        None => path.with_file_name(format!("{stem}.fixed")),
    }
}

fn write_new_file(path: &Path, bytes: &[u8]) -> Result<(), String> {
    if path.exists() {
        return Err("refusing to overwrite existing fixed copy".into());
    }
    fs::write(path, bytes).map_err(|error| format!("writing copy: {error}"))
}

fn write_in_place(
    path: &Path,
    original: &[u8],
    fixed: &[u8],
    backup: bool,
    verify: impl FnOnce(&[u8]) -> Result<(), String>,
) -> Result<(), String> {
    if backup {
        fs::write(
            path.with_extension(format!(
                "{}bak",
                path.extension()
                    .map_or_else(String::new, |ext| format!("{}.", ext.to_string_lossy()))
            )),
            original,
        )
        .map_err(|error| format!("writing backup: {error}"))?;
    }
    let metadata = fs::metadata(path).map_err(|error| format!("reading metadata: {error}"))?;
    let original_permissions = metadata.permissions();
    let directory = path.parent().unwrap_or_else(|| Path::new("."));
    let mut temporary = NamedTempFile::new_in(directory)
        .map_err(|error| format!("creating temporary file: {error}"))?;
    temporary
        .write_all(fixed)
        .and_then(|()| temporary.as_file().sync_all())
        .map_err(|error| format!("writing temporary file: {error}"))?;
    temporary
        .as_file()
        .set_permissions(original_permissions.clone())
        .map_err(|error| format!("preserving permissions: {error}"))?;
    #[cfg(windows)]
    {
        // MoveFileEx cannot replace a read-only destination.
        let mut writable = original_permissions.clone();
        #[allow(clippy::permissions_set_readonly_false)]
        writable.set_readonly(false);
        fs::set_permissions(path, writable)
            .map_err(|error| format!("making original replaceable: {error}"))?;
    }
    temporary.persist(path).map_err(|error| {
        #[cfg(windows)]
        let _ = fs::set_permissions(path, original_permissions.clone());
        format!("replacing original: {}", error.error)
    })?;
    fs::set_permissions(path, original_permissions.clone()).map_err(|error| {
        restore_original(path, original, &original_permissions)
            .err()
            .map_or_else(
                || format!("restoring permissions after replacement: {error}"),
                |restore| format!("restoring permissions after replacement: {error}; {restore}"),
            )
    })?;
    let result = fs::read(path)
        .map_err(|error| format!("reading written file: {error}"))
        .and_then(|written| verify(&written));
    if let Err(error) = result {
        restore_original(path, original, &original_permissions)
            .map_err(|restore| format!("{error}; {restore}"))?;
        return Err(error);
    }
    Ok(())
}

fn restore_original(
    path: &Path,
    original: &[u8],
    permissions: &fs::Permissions,
) -> Result<(), String> {
    #[cfg(windows)]
    {
        let mut writable = permissions.clone();
        #[allow(clippy::permissions_set_readonly_false)]
        writable.set_readonly(false);
        fs::set_permissions(path, writable)
            .map_err(|error| format!("making original writable: {error}"))?;
    }
    fs::write(path, original).map_err(|error| format!("restoring original bytes: {error}"))?;
    fs::set_permissions(path, permissions.clone())
        .map_err(|error| format!("restoring original permissions: {error}"))
}

fn verify_fixed(
    path: &Path,
    format: DocFormat,
    bytes: &[u8],
    findings: &[Finding],
) -> Result<(), String> {
    let links = extract(&SourceDocument {
        path: path.to_path_buf(),
        format,
        bytes: bytes.to_vec(),
    })
    .map_err(|error| format!("re-parsing fixed file: {}", error.0))?;
    for finding in findings {
        let replacement = &finding
            .fix
            .as_ref()
            .expect("selected finding has a fix")
            .replacement_url;
        if !links.iter().any(|link| link.url == *replacement) {
            return Err(format!(
                "replacement URL was not extractable: {replacement}"
            ));
        }
        if links.iter().any(|link| link.url == finding.url) {
            return Err(format!("old URL is still extractable: {}", finding.url));
        }
    }
    Ok(())
}

fn write_report(
    mut writer: impl Write,
    format: OutputFormat,
    report: &stalelink_core::scan::ScanReport,
    exit_code: u8,
) -> io::Result<()> {
    match format {
        OutputFormat::Table => TableSink(&mut writer).emit(report),
        OutputFormat::Json => output::write_json(&mut writer, report),
        OutputFormat::Sarif => output::write_sarif(&mut writer, report, exit_code),
    }
}

fn cache_path(configured: Option<&std::path::Path>) -> PathBuf {
    if let Some(directory) = configured {
        return directory.join("verdicts.sqlite3");
    }
    if let Some(directory) = std::env::var_os("STALELINK_CACHE_DIR") {
        return PathBuf::from(directory).join("verdicts.sqlite3");
    }
    directories::ProjectDirs::from("com", "stalelink", "stalelink")
        .map(|directories| directories.cache_dir().join("verdicts.sqlite3"))
        .unwrap_or_else(|| PathBuf::from(".stalelink-cache.sqlite3"))
}

fn run_cache(command: CacheCommand) -> ExitCode {
    // Cache commands are project-scoped from the current working directory.
    let settings = match config::resolve(std::path::Path::new(".")) {
        Ok(settings) => settings,
        Err(error) => {
            eprintln!("error: {error}");
            return ExitCode::from(USAGE);
        }
    };
    let path = cache_path(settings.cache.dir.as_deref());
    match command {
        CacheCommand::Clear => match cache::VerdictCache::clear(&path) {
            Ok(()) => ExitCode::from(CLEAN),
            Err(error) => {
                eprintln!("error: {error}");
                ExitCode::from(ENVIRONMENT)
            }
        },
        CacheCommand::Stats => {
            match cache::VerdictCache::open(path).and_then(|cache| cache.stats()) {
                Ok(stats) => {
                    println!(
                        "hits: {}\nmisses: {}\nentries: {}\nsize: {}",
                        stats.hits, stats.misses, stats.entries, stats.size
                    );
                    ExitCode::from(CLEAN)
                }
                Err(error) => {
                    eprintln!("error: {error}");
                    ExitCode::from(ENVIRONMENT)
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn confidence_level_maps_to_core() {
        let cases = [
            (ConfidenceLevel::DeadCertain, Confidence::DeadCertain),
            (ConfidenceLevel::LikelyDead, Confidence::LikelyDead),
            (ConfidenceLevel::AuthWalled, Confidence::AuthWalled),
            (ConfidenceLevel::Outdated, Confidence::Outdated),
            (ConfidenceLevel::Suspect, Confidence::Suspect),
        ];
        for (level, expected) in cases {
            assert_eq!(Confidence::from(level), expected);
        }
    }

    #[test]
    fn failed_verification_restores_original_bytes() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("note.txt");
        let original = b"original\0bytes";
        fs::write(&path, original).unwrap();

        let result = write_in_place(&path, original, b"fixed", false, |_| {
            Err("failed verification".into())
        });

        assert_eq!(result.unwrap_err(), "failed verification");
        assert_eq!(fs::read(path).unwrap(), original);
    }

    #[test]
    fn failed_verification_restores_binary_document_bytes() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("fixture.docx");
        let original = b"PK\x03\x04binary OOXML bytes\0\xff";
        fs::write(&path, original).unwrap();

        let result = write_in_place(
            &path,
            original,
            b"PK\x03\x04fixed OOXML bytes",
            false,
            |_| Err("failed verification".into()),
        );

        assert_eq!(result.unwrap_err(), "failed verification");
        assert_eq!(fs::read(path).unwrap(), original);
    }

    #[test]
    fn failed_verification_restores_pdf_bytes() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("fixture.pdf");
        let original = b"%PDF-1.4\noriginal PDF bytes\0\xff\n%%EOF\n";
        fs::write(&path, original).unwrap();

        let result = write_in_place(
            &path,
            original,
            b"%PDF-1.4\nfixed PDF bytes\n%%EOF\n",
            false,
            |_| Err("failed verification".into()),
        );

        assert_eq!(result.unwrap_err(), "failed verification");
        assert_eq!(fs::read(path).unwrap(), original);
    }

    #[cfg(windows)]
    #[test]
    fn in_place_write_preserves_windows_readonly_attribute() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("readonly.txt");
        fs::write(&path, b"original").unwrap();
        let mut permissions = fs::metadata(&path).unwrap().permissions();
        permissions.set_readonly(true);
        fs::set_permissions(&path, permissions).unwrap();

        write_in_place(&path, b"original", b"fixed", false, |_| Ok(())).unwrap();

        assert_eq!(fs::read(&path).unwrap(), b"fixed");
        assert!(fs::metadata(path).unwrap().permissions().readonly());
    }

    #[cfg(windows)]
    #[test]
    fn failed_verification_restores_windows_readonly_attribute() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("readonly.txt");
        fs::write(&path, b"original").unwrap();
        let mut permissions = fs::metadata(&path).unwrap().permissions();
        permissions.set_readonly(true);
        fs::set_permissions(&path, permissions).unwrap();

        let result = write_in_place(&path, b"original", b"fixed", false, |_| {
            Err("failed verification".into())
        });

        assert_eq!(result.unwrap_err(), "failed verification");
        assert_eq!(fs::read(&path).unwrap(), b"original");
        assert!(fs::metadata(path).unwrap().permissions().readonly());
    }

    #[cfg(unix)]
    #[test]
    fn in_place_write_preserves_unix_mode() {
        use std::os::unix::fs::PermissionsExt;

        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("readonly.txt");
        fs::write(&path, b"original").unwrap();
        fs::set_permissions(&path, fs::Permissions::from_mode(0o444)).unwrap();

        write_in_place(&path, b"original", b"fixed", false, |_| Ok(())).unwrap();

        assert_eq!(fs::read(&path).unwrap(), b"fixed");
        assert_eq!(
            fs::metadata(path).unwrap().permissions().mode() & 0o777,
            0o444
        );
    }
}
