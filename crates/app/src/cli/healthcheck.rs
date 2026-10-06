//! `codoseo healthcheck`: a one-shot HTTP probe for container HEALTHCHECKs. The runtime image
//! is distroless, so there is no curl or wget to call `/readyz` with.

use std::time::Duration;

use clap::Args;

use super::{CliError, EXIT_OK, Outcome};

/// Exit code when the probe fails (not reachable, timed out or a non-2xx answer).
const EXIT_UNHEALTHY: u8 = 1;

#[derive(Debug, Args)]
pub struct HealthcheckArgs {
    /// URL to GET (default: /readyz on the port from CODOSEO_BIND, on 127.0.0.1)
    #[arg(long)]
    pub url: Option<String>,
    /// Seconds to wait for an answer
    #[arg(long, default_value_t = 3)]
    pub timeout: u64,
}

/// `/readyz` on loopback at the port of `CODOSEO_BIND` (default 8080). A bind that does not
/// parse as host:port falls back to 8080 rather than failing the probe on a config typo the
/// web role will report itself.
pub fn default_url(bind: Option<&str>) -> String {
    let port = bind
        .and_then(|b| b.rsplit_once(':'))
        .and_then(|(_, port)| port.parse::<u16>().ok())
        .unwrap_or(8080);
    format!("http://127.0.0.1:{port}/readyz")
}

pub async fn run(args: HealthcheckArgs) -> Outcome {
    let url = args
        .url
        .unwrap_or_else(|| default_url(std::env::var("CODOSEO_BIND").ok().as_deref()));
    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(args.timeout))
        .redirect(reqwest::redirect::Policy::none())
        // The probe targets the container's own port; a proxy in the environment must not see it.
        .no_proxy()
        .build()
        .map_err(|e| CliError::msg(format!("could not build the HTTP client: {e}")))?;
    match client.get(&url).send().await {
        Ok(resp) if resp.status().is_success() => Ok(EXIT_OK),
        Ok(resp) => {
            eprintln!("unhealthy: GET {url} answered {}", resp.status());
            Ok(EXIT_UNHEALTHY)
        }
        Err(e) => {
            eprintln!(
                "unhealthy: GET {url} failed: {}",
                super::clean(&e.to_string())
            );
            Ok(EXIT_UNHEALTHY)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::default_url;

    #[test]
    fn default_url_uses_the_bind_port() {
        assert_eq!(default_url(None), "http://127.0.0.1:8080/readyz");
        assert_eq!(
            default_url(Some("0.0.0.0:9000")),
            "http://127.0.0.1:9000/readyz"
        );
        assert_eq!(
            default_url(Some("[::]:7000")),
            "http://127.0.0.1:7000/readyz"
        );
        assert_eq!(default_url(Some("garbage")), "http://127.0.0.1:8080/readyz");
    }
}
