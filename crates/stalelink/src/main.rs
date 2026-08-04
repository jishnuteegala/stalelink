use std::{io, path::PathBuf, process::ExitCode};

use clap::{ArgAction, Args, CommandFactory, Parser, Subcommand, ValueEnum};
use clap_complete::{Shell, generate};
use stalelink_core::model::Confidence;

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
    #[arg(long, requires = "write")]
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
    /// Skip file-path, anchor, and relative-link validation
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
    #[arg(long, value_enum, default_value_t = ConfidenceLevel::Suspect)]
    fail_on: ConfidenceLevel,
}

#[derive(Debug, Args)]
struct NetworkArgs {
    /// Global request concurrency
    #[arg(long, default_value_t = 128)]
    max_concurrency: u16,
    /// Concurrent requests per host
    #[arg(long, default_value_t = 4)]
    per_host: u16,
    /// Per-request timeout in seconds
    #[arg(long, default_value_t = 20)]
    timeout: u64,
    /// Retries per request
    #[arg(long, default_value_t = 2)]
    retries: u8,
    /// Custom User-Agent header
    #[arg(long)]
    user_agent: Option<String>,
    /// Cache entry validity window (e.g. 30m, 12h, 7d)
    #[arg(long, default_value = "24h")]
    cache_ttl: String,
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
        Command::Scan(_) | Command::Fix(_) | Command::Cache { .. } => {
            eprintln!("error: not implemented yet");
            ExitCode::from(ENVIRONMENT)
        }
    }
}
