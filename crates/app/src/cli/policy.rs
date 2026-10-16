//! `codoseo policy`: the site policy file (`codoseo.toml`). `init` writes a starting file,
//! `validate` checks one, and `export`/`import` move a monitored site's policy to and from the
//! hosted app over the REST API. `crawl` and `check` read the file too ([`resolve`]).

use std::io::Write;
use std::path::{Path, PathBuf};

use clap::{Args, Subcommand};
use codoseo_core::crawl::AddressPolicy;
use codoseo_crawler::fetch::{Fetcher, FetcherConfig};
use codoseo_policy::template::init_file;
use codoseo_policy::{DEFAULT_FILE, HostPref, LoadedFile, Slash, load_file, parse_file};
use url::Url;

use super::{CliError, EXIT_FAIL_ON, EXIT_OK, Outcome};

/// The hosted app, for `export` and `import`.
const DEFAULT_BASE_URL: &str = "https://codoseo.com";

#[derive(Debug, Args)]
pub struct PolicyArgs {
    #[command(subcommand)]
    command: PolicyCommand,
}

#[derive(Debug, Subcommand)]
enum PolicyCommand {
    /// Write a codoseo.toml with every key, commented out at its default
    Init(InitArgs),
    /// Check a policy file (default ./codoseo.toml): exit 1 when it is not valid
    Validate(ValidateArgs),
    /// Download a monitored site's policy as TOML (REST API, needs an API key)
    Export(ExportArgs),
    /// Replace a monitored site's policy with a file (REST API, needs an API key)
    Import(ImportArgs),
}

#[derive(Debug, Args)]
struct InitArgs {
    /// Read the preferred host (www or not) and trailing slash from where this URL redirects
    #[arg(long, value_name = "URL")]
    from: Option<Url>,
    /// Where to write it
    #[arg(short, long, default_value = DEFAULT_FILE)]
    output: PathBuf,
    /// Overwrite an existing file
    #[arg(long)]
    force: bool,
}

#[derive(Debug, Args)]
struct ValidateArgs {
    /// The file to check (default ./codoseo.toml)
    file: Option<PathBuf>,
}

#[derive(Debug, Args)]
struct Remote {
    /// The site's id, from `list_sites` or the site's address in the app (/s/{id}/...)
    #[arg(long)]
    site: String,
    /// An API key (Settings → API keys); also read from CODOSEO_API_KEY
    #[arg(long)]
    api_key: Option<String>,
    /// The app's address, for a self-hosted install
    #[arg(long, default_value = DEFAULT_BASE_URL)]
    base_url: Url,
}

#[derive(Debug, Args)]
struct ExportArgs {
    #[command(flatten)]
    remote: Remote,
    /// Write it to FILE instead of stdout
    #[arg(short, long)]
    output: Option<PathBuf>,
}

#[derive(Debug, Args)]
struct ImportArgs {
    #[command(flatten)]
    remote: Remote,
    /// The policy file to upload (default ./codoseo.toml)
    file: Option<PathBuf>,
}

pub async fn run(args: PolicyArgs) -> Outcome {
    match args.command {
        PolicyCommand::Init(a) => init(a).await,
        PolicyCommand::Validate(a) => validate(a),
        PolicyCommand::Export(a) => export(a).await,
        PolicyCommand::Import(a) => import(a).await,
    }
}

/// The address form `url` settles on: www or not from the host it redirects to, and the trailing
/// slash from its path when it has one (a bare home page says nothing about slashes).
async fn address_of(url: &Url) -> Result<(HostPref, Slash), CliError> {
    let fetcher = Fetcher::new(FetcherConfig::new(AddressPolicy::AllowPrivate))?;
    let res = fetcher.fetch(url).await?;
    let host = if res
        .final_url
        .host_str()
        .is_some_and(|h| h.starts_with("www."))
    {
        HostPref::Www
    } else {
        HostPref::Apex
    };
    let path = res.final_url.path();
    let slash = if path.len() > 1 && path.ends_with('/') {
        Slash::With
    } else {
        Slash::Without
    };
    Ok((host, slash))
}

async fn init(args: InitArgs) -> Outcome {
    if args.output.exists() && !args.force {
        return Err(CliError::msg(format!(
            "{} already exists (--force overwrites it)",
            args.output.display()
        )));
    }
    let address = match &args.from {
        Some(url) => Some(address_of(url).await?),
        None => None,
    };
    std::fs::write(&args.output, init_file(address))
        .map_err(|e| CliError::msg(format!("cannot write {}: {e}", args.output.display())))?;
    eprintln!("wrote {}", args.output.display());
    Ok(EXIT_OK)
}

fn validate(args: ValidateArgs) -> Outcome {
    let path = args.file.unwrap_or_else(|| PathBuf::from(DEFAULT_FILE));
    match load_file(&path) {
        Ok(f) => {
            warn_unknown(&f);
            println!("{}: valid policy ({})", path.display(), f.hash);
            Ok(EXIT_OK)
        }
        Err(e) => {
            for line in e.lines() {
                eprintln!("{}", super::clean(line));
            }
            Ok(EXIT_FAIL_ON)
        }
    }
}

/// Unknown keys are ignored, and said so on stderr.
pub fn warn_unknown(f: &LoadedFile) {
    for key in &f.warnings {
        eprintln!(
            "{}: unknown key {} (ignored)",
            f.path.display(),
            super::clean(key)
        );
    }
}

/// The policy file `crawl` and `check` judge under: `--policy FILE`, else `./codoseo.toml` when
/// there is one, and none with `--no-policy`. A file that is there but not valid is an error.
pub fn resolve(explicit: Option<&Path>, no_policy: bool) -> Result<Option<LoadedFile>, CliError> {
    if no_policy {
        return Ok(None);
    }
    let path = match explicit {
        Some(p) => p.to_owned(),
        None => {
            let default = PathBuf::from(DEFAULT_FILE);
            if !default.exists() {
                return Ok(None);
            }
            default
        }
    };
    // One line: the error is printed through `clean`, which drops line breaks.
    let f = load_file(&path).map_err(|e| CliError::msg(one_line(&e)))?;
    warn_unknown(&f);
    Ok(Some(f))
}

impl Remote {
    fn key(&self) -> Result<String, CliError> {
        self.api_key
            .clone()
            .or_else(|| std::env::var("CODOSEO_API_KEY").ok())
            .filter(|k| !k.trim().is_empty())
            .ok_or_else(|| CliError::msg("--api-key (or CODOSEO_API_KEY) is required"))
    }
}

/// `file is not a valid policy:\na\nb` as `file is not a valid policy: a; b`.
fn one_line(e: &str) -> String {
    e.replace(":\n", ": ").replace('\n', "; ")
}

fn policy_url(r: &Remote) -> Result<Url, CliError> {
    r.base_url
        .join(&format!("/api/v1/sites/{}/policy", r.site.trim()))
        .map_err(|e| CliError::msg(format!("bad --base-url: {e}")))
}

/// The API's `{"error":{"message"}}`, else the status.
fn api_error(status: reqwest::StatusCode, body: &str) -> CliError {
    let message = serde_json::from_str::<serde_json::Value>(body)
        .ok()
        .and_then(|v| v["error"]["message"].as_str().map(str::to_owned))
        .unwrap_or_else(|| format!("the API answered {status}"));
    CliError::msg(super::clean(&message))
}

fn client() -> Result<reqwest::Client, CliError> {
    reqwest::Client::builder()
        .user_agent(concat!("codoseo-cli/", env!("CARGO_PKG_VERSION")))
        .build()
        .map_err(|e| CliError::msg(format!("cannot start the HTTP client: {e}")))
}

async fn export(args: ExportArgs) -> Outcome {
    let res = client()?
        .get(policy_url(&args.remote)?)
        .bearer_auth(args.remote.key()?)
        .header(reqwest::header::ACCEPT, "application/toml")
        .send()
        .await
        .map_err(|e| CliError::msg(format!("cannot reach the API: {e}")))?;
    let status = res.status();
    let body = res
        .text()
        .await
        .map_err(|e| CliError::msg(format!("cannot read the API's answer: {e}")))?;
    if !status.is_success() {
        return Err(api_error(status, &body));
    }
    let mut w = super::open_output(args.output.as_ref())?;
    w.write_all(body.as_bytes())?;
    w.flush()?;
    Ok(EXIT_OK)
}

async fn import(args: ImportArgs) -> Outcome {
    let path = args.file.unwrap_or_else(|| PathBuf::from(DEFAULT_FILE));
    let text = std::fs::read_to_string(&path)
        .map_err(|e| CliError::msg(format!("cannot read {}: {e}", path.display())))?;
    // Checked here first, so a broken file never reaches the site.
    if let Err(errors) = parse_file(&text) {
        return Err(CliError::msg(one_line(&format!(
            "{} is not a valid policy:\n{}",
            path.display(),
            codoseo_policy::error_lines(&errors)
        ))));
    }
    let res = client()?
        .put(policy_url(&args.remote)?)
        .bearer_auth(args.remote.key()?)
        .header(reqwest::header::CONTENT_TYPE, "application/toml")
        .body(text)
        .send()
        .await
        .map_err(|e| CliError::msg(format!("cannot reach the API: {e}")))?;
    let status = res.status();
    let body = res
        .text()
        .await
        .map_err(|e| CliError::msg(format!("cannot read the API's answer: {e}")))?;
    if !status.is_success() {
        return Err(api_error(status, &body));
    }
    let v: serde_json::Value = serde_json::from_str(&body).unwrap_or_default();
    let gaps = v["report"]["ai_gaps"].as_u64().unwrap_or(0);
    println!(
        "{} imported into site {} · {} AI {}",
        path.display(),
        args.remote.site.trim(),
        gaps,
        if gaps == 1 { "gap" } else { "gaps" }
    );
    Ok(EXIT_OK)
}
