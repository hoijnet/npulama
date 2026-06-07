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

    /// Model alias to load on startup (overrides preferred_model in config)
    #[arg(long)]
    pub model: Option<String>,

    /// Context window size in tokens, 2048–131072 (overrides config)
    #[arg(long)]
    pub context: Option<u32>,
}

impl Args {
    pub fn apply_to(&self, config: &mut Config) {
        if let Some(port) = self.port {
            config.port = port;
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
        if let Some(ref m) = self.model {
            config.preferred_model = Some(m.clone());
        }
        if let Some(c) = self.context {
            config.context_size = c.clamp(2048, 131072);
        }
    }
}
