mod cache;
mod config;

use std::{
    fs::File,
    io::{self, IsTerminal, Read, Write},
    path::PathBuf,
    process::ExitCode,
};

use clap::{ArgAction, Args, CommandFactory, Parser, Subcommand, ValueEnum};
use clap_complete::{Shell, generate};
use regex::Regex;
use stalelink_core::{
    check::HttpChecker,
    model::Confidence,
    report::{ReportSink, TableSink},
    scan::{NoProgress, Progress, ScanInput, scan},
    walk::WalkOptions,
};

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

#[derive(Debug, Args)]
struct ScanArgs {
    #[command(flatten)]
    common: CommonArgs,
}

#[derive(Debug, Args)]
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

#[derive(Debug, Args)]
struct CommonArgs {
    #[command(flatten)]
    output: OutputArgs,
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

#[derive(Debug, Args)]
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

#[derive(Debug, Args)]
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

#[derive(Debug, Args)]
struct AuthArgs {
    /// Maximum auth tier: off = plain HTTP, cookies = browser cookies, browser = real browser
    #[arg(long, value_enum, default_value_t = Auth::Cookies)]
    auth: Auth,
    /// Which browser's cookies/profile to use
    #[arg(long, value_enum, default_value_t = Browser::Auto)]
    browser: Browser,
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

#[derive(Debug, Clone, Copy, ValueEnum)]
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
        Command::Fix(_) => {
            eprintln!("error: not implemented yet");
            ExitCode::from(ENVIRONMENT)
        }
    }
}

fn run_scan(args: ScanArgs, quiet: bool) -> ExitCode {
    let mut paths = args.common.paths;
    if args.common.stdin {
        let mut input = String::new();
        if let Err(error) = io::stdin().read_to_string(&mut input) {
            eprintln!("error: reading stdin: {error}");
            return ExitCode::from(ENVIRONMENT);
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
    let network = &args.common.network;
    let mut settings = match config::resolve(&first_path) {
        Ok(settings) => settings,
        Err(error) => {
            eprintln!("error: {error}");
            return ExitCode::from(USAGE);
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
                return ExitCode::from(USAGE);
            }
        };
    }
    if args.common.no_local {
        settings.ignore.local_links = true;
    }
    let fail_on = match args.common.output.fail_on {
        Some(value) => value,
        None => match settings.output.fail_on.parse::<ConfidenceLevel>() {
            Ok(value) => value,
            Err(_) => {
                eprintln!(
                    "error: invalid output.fail-on value: {}",
                    settings.output.fail_on
                );
                return ExitCode::from(USAGE);
            }
        },
    };
    if settings.network.max_concurrency == 0 || settings.network.per_host == 0 {
        eprintln!("error: --max-concurrency and --per-host must be at least 1");
        return ExitCode::from(USAGE);
    }
    // Validate argument values (usage errors) before any path or environment
    // check, so a bad regex reports exit 2 regardless of whether a path exists.
    let exclude_urls = match args
        .common
        .exclude_url
        .iter()
        .chain(settings.ignore.exclude_url.iter())
        .map(|value| Regex::new(value))
        .collect::<Result<Vec<_>, _>>()
    {
        Ok(regexes) => regexes,
        Err(error) => {
            eprintln!("error: invalid --exclude-url regex: {error}");
            return ExitCode::from(USAGE);
        }
    };
    if args.common.output.json || !matches!(args.common.output.format, OutputFormat::Table) {
        eprintln!("error: not implemented yet");
        return ExitCode::from(ENVIRONMENT);
    }
    for path in &paths {
        if !path.exists() {
            eprintln!("error: path does not exist: {}", path.display());
            return ExitCode::from(ENVIRONMENT);
        }
    }
    let checker = match HttpChecker::new(
        settings.network.timeout,
        settings.network.retries,
        settings.network.per_host as usize,
        settings
            .network
            .user_agent
            .unwrap_or_else(|| format!("stalelink/{}", env!("CARGO_PKG_VERSION"))),
    ) {
        Ok(checker) => checker,
        Err(error) => {
            eprintln!("error: creating HTTP client: {error}");
            return ExitCode::from(ENVIRONMENT);
        }
    };
    let cache_path = cache_path(settings.cache.dir.as_deref());
    let input = ScanInput {
        paths,
        walk: WalkOptions {
            include: args.common.include,
            exclude: args
                .common
                .exclude
                .into_iter()
                .chain(settings.ignore.exclude)
                .collect(),
        },
        max_concurrency: settings.network.max_concurrency as usize,
        exclude_urls,
        exclude_domains: args
            .common
            .exclude_domain
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
            return ExitCode::from(ENVIRONMENT);
        }
    };
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
                return ExitCode::from(ENVIRONMENT);
            }
        };
        let checker =
            cache::CachingChecker::new(checker, cache, settings.cache.ttl, 1, network.refresh);
        let result = if show_progress {
            runtime.block_on(scan(input, &checker, &StderrProgress))
        } else {
            runtime.block_on(scan(input, &checker, &NoProgress))
        };
        if let Some(error) = checker.error() {
            eprintln!("error: {error}");
            return ExitCode::from(ENVIRONMENT);
        }
        result
    };
    if show_progress {
        eprintln!();
    }
    let mut report = match result {
        Ok(report) => report,
        Err(error) => {
            eprintln!("error: {error}");
            return ExitCode::from(ENVIRONMENT);
        }
    };
    let failed = report
        .findings
        .iter()
        .any(|finding| finding.verdict.confidence >= Confidence::from(fail_on));
    let minimum = Confidence::from(args.common.output.min_confidence);
    report
        .findings
        .retain(|finding| finding.verdict.confidence >= minimum);
    let result = if let Some(path) = args.common.output.output {
        File::create(path).and_then(|file| TableSink(file).emit(&report))
    } else {
        TableSink(io::stdout()).emit(&report)
    };
    if let Err(error) = result {
        eprintln!("error: writing report: {error}");
        return ExitCode::from(ENVIRONMENT);
    }
    ExitCode::from(if failed { 1 } else { CLEAN })
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
}
