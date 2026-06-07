use clap::Parser;

use crate::config::Config;

#[derive(Parser, Debug)]
#[command(
    name = "npulama",
    about = "OpenAI-compatible proxy for Microsoft Foundry Local",
    long_about = None
)]
pub struct Args {
    /// Proxy listen port (overrides saved config)
    #[arg(long)]
    pub port: Option<u16>,

    /// Foundry Local upstream URL (overrides saved config)
    #[arg(long)]
    pub upstream: Option<String>,

    /// Bind to 0.0.0.0 for network access (overrides saved config)
    #[arg(long)]
    pub bind_all: bool,

    /// Require Bearer token authentication
    #[arg(long)]
    pub require_auth: bool,

    /// API token(s) to accept — may be repeated
    #[arg(long = "token")]
    pub tokens: Vec<String>,

    /// Run without GUI — proxy only, Ctrl-C to stop
    #[arg(long)]
    pub headless: bool,

    /// Start proxy immediately on launch (GUI mode)
    #[arg(long)]
    pub autostart: bool,
}

impl Args {
    /// Merge CLI overrides into a loaded Config.
    pub fn apply_to(&self, config: &mut Config) {
        if let Some(port) = self.port {
            config.port = port;
        }
        if let Some(ref url) = self.upstream {
            config.upstream_url = url.clone();
        }
        if self.bind_all {
            config.bind_all = true;
        }
        if self.require_auth {
            config.require_auth = true;
        }
        if !self.tokens.is_empty() {
            config.tokens.extend(self.tokens.iter().cloned());
            config.require_auth = true;
        }
        if self.autostart {
            config.autostart = true;
        }
    }
}
