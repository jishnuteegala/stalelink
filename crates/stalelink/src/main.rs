use std::{io, path::PathBuf, process::ExitCode};

use clap::{ArgAction, Args, CommandFactory, Parser, Subcommand, ValueEnum};
use clap_complete::{generate, shells};
use stalelink_core::model::Confidence;

pub const CLEAN: u8 = 0;
pub const FINDINGS: u8 = 1;
pub const USAGE: u8 = 2;
pub const ENVIRONMENT: u8 = 3;

#[derive(Debug, Parser)]
#[command(
    name = "stalelink",
    version,
    about = "Find stale links in local documents"
)]
#[command(after_help = "Examples:\n  stalelink scan docs/\n  stalelink fix report.md --write")]
struct Cli {
    #[arg(long, global = true)]
    config: Option<PathBuf>,
    #[arg(long, global = true)]
    no_config: bool,
    #[arg(short, long, global = true)]
    quiet: bool,
    #[arg(short, long, action = ArgAction::Count, global = true)]
    verbose: u8,
    #[arg(long, value_enum, default_value_t = Color::Auto, global = true)]
    color: Color,
    #[command(subcommand)]
    command: Command,
}

#[derive(Debug, Subcommand)]
enum Command {
    Scan(ScanArgs),
    Fix(FixArgs),
    Cache {
        #[command(subcommand)]
        command: CacheCommand,
    },
    Completions {
        shell: CompletionShell,
    },
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
    #[arg(long)]
    write: bool,
    #[arg(long)]
    backup: bool,
    #[arg(long, conflicts_with = "write")]
    copy: bool,
    #[arg(long, value_enum)]
    fix_exclude: Vec<FixExclude>,
    #[arg(long, value_enum, default_value_t = ConfidenceLevel::DeadCertain)]
    min_fix_confidence: ConfidenceLevel,
}

#[derive(Debug, Args)]
struct CommonArgs {
    #[command(flatten)]
    output: OutputArgs,
    #[arg(value_name = "PATHS", required_unless_present = "stdin")]
    paths: Vec<PathBuf>,
    #[arg(long)]
    stdin: bool,
    #[arg(long)]
    include: Vec<String>,
    #[arg(long)]
    exclude: Vec<String>,
    #[arg(long)]
    exclude_url: Vec<String>,
    #[arg(long)]
    exclude_domain: Vec<String>,
    #[arg(long)]
    no_local: bool,
    #[command(flatten)]
    network: NetworkArgs,
    #[command(flatten)]
    auth: AuthArgs,
}

#[derive(Debug, Args)]
struct OutputArgs {
    #[arg(long, value_enum, default_value_t = OutputFormat::Table)]
    format: OutputFormat,
    #[arg(long, overrides_with = "format")]
    json: bool,
    #[arg(short, long)]
    output: Option<PathBuf>,
    #[arg(long, value_enum, default_value_t = ConfidenceLevel::Suspect)]
    min_confidence: ConfidenceLevel,
    #[arg(long, value_enum, default_value_t = ConfidenceLevel::Suspect)]
    fail_on: ConfidenceLevel,
}

#[derive(Debug, Args)]
struct NetworkArgs {
    #[arg(long, default_value_t = 128)]
    max_concurrency: u16,
    #[arg(long, default_value_t = 4)]
    per_host: u16,
    #[arg(long, default_value_t = 20)]
    timeout: u64,
    #[arg(long, default_value_t = 2)]
    retries: u8,
    #[arg(long)]
    user_agent: Option<String>,
    #[arg(long, default_value = "24h")]
    cache_ttl: String,
    #[arg(long)]
    no_cache: bool,
    #[arg(long)]
    refresh: bool,
}

#[derive(Debug, Args)]
struct AuthArgs {
    #[arg(long, value_enum, default_value_t = Auth::Cookies)]
    auth: Auth,
    #[arg(long, value_enum, default_value_t = Browser::Auto)]
    browser: Browser,
}

#[derive(Debug, Subcommand)]
enum CacheCommand {
    Clear,
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
enum CompletionShell {
    Bash,
    Elvish,
    Fish,
    PowerShell,
    Zsh,
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
            match shell {
                CompletionShell::Bash => {
                    generate(shells::Bash, &mut command, "stalelink", &mut io::stdout())
                }
                CompletionShell::Elvish => {
                    generate(shells::Elvish, &mut command, "stalelink", &mut io::stdout())
                }
                CompletionShell::Fish => {
                    generate(shells::Fish, &mut command, "stalelink", &mut io::stdout())
                }
                CompletionShell::PowerShell => generate(
                    shells::PowerShell,
                    &mut command,
                    "stalelink",
                    &mut io::stdout(),
                ),
                CompletionShell::Zsh => {
                    generate(shells::Zsh, &mut command, "stalelink", &mut io::stdout())
                }
            }
            ExitCode::from(CLEAN)
        }
        Command::Scan(_) | Command::Fix(_) | Command::Cache { .. } => {
            eprintln!("error: not implemented yet");
            ExitCode::from(ENVIRONMENT)
        }
    }
}
