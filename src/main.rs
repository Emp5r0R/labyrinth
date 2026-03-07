use clap::{Parser, Subcommand};
use tracing::info;
use tracing::Level;
use tracing_subscriber::fmt::format::FmtSpan;

mod agent;
mod config;
mod error;
mod protocol;
mod security;
mod server;
mod streaming;
mod styling;

fn print_logo() {
    println!("{}", styling::format_logo());
}

#[derive(Parser)]
#[clap(author, version, about, long_about = None)]
struct Cli {
    #[clap(subcommand)]
    command: Commands,
}

#[derive(Subcommand)]
enum Commands {
    /// Start Labyrinth server
    Server {
        /// Server listening address
        #[clap(short, long, default_value = "0.0.0.0:44344")]
        listen_addr: String,
        /// Disable authentication (for testing only)
        #[clap(long)]
        no_auth: bool,
        /// Skip interactive mode and run headless
        #[clap(long)]
        headless: bool,
        /// TUN interface name for tunneling
        #[clap(long)]
        interface: Option<String>,
        /// Target subnet to route (e.g., 192.168.1.0/24)
        #[clap(long)]
        route: Option<String>,
        /// Domain for TLS certificate
        #[clap(long)]
        domain: Option<String>,
    },
    /// Connect agent to server
    Agent {
        /// Server address to connect to
        #[clap(short, long)]
        server: String,
        /// Base64 encoded server certificate
        #[clap(long)]
        cert: Option<String>,
        /// Accept certificate with specific SHA256 fingerprint
        #[clap(short, long)]
        fingerprint: Option<String>,
        /// SOCKS5 proxy URL (e.g., socks5://user:pass@127.0.0.1:1080)
        #[clap(short, long)]
        proxy: Option<String>,
        /// Auto-retry on connection failure
        #[clap(short, long)]
        retry: bool,
    },
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    print_logo();
    let cli = Cli::parse();

    let log_level = match &cli.command {
        Commands::Server { headless, .. } if !headless => Level::ERROR,
        _ => Level::INFO,
    };

    tracing_subscriber::fmt()
        .with_span_events(FmtSpan::CLOSE)
        .with_target(false)
        .with_level(true)
        .with_max_level(log_level)
        .init();

    match &cli.command {
        Commands::Server {
            listen_addr,
            no_auth,
            headless,
            interface,
            route,
            domain,
        } => {
            if *headless {
                println!(
                    "{}",
                    styling::format_success_msg(
                        styling::SUCCESS_INDICATOR,
                        "Starting Labyrinth server in headless mode"
                    )
                );
                if let Err(e) = server::run_headless_server(
                    listen_addr,
                    *no_auth,
                    interface.clone(),
                    route.clone(),
                    domain.clone(),
                )
                .await
                {
                    eprintln!("Server error: {}", e);
                }
            } else {
                println!(
                    "{}",
                    styling::format_success_msg(
                        styling::SUCCESS_INDICATOR,
                        "Starting Labyrinth server in interactive mode"
                    )
                );
                if let Err(e) =
                    server::run_interactive_server(listen_addr, *no_auth, domain.clone()).await
                {
                    eprintln!("Server error: {}", e);
                }
            }
        }
        Commands::Agent {
            server,
            cert,
            fingerprint,
            proxy,
            retry,
        } => {
            info!("Connecting agent to {}", server);
            if let Err(e) = agent::run_agent(
                server,
                cert.clone(),
                fingerprint.clone(),
                proxy.clone(),
                *retry,
            )
            .await
            {
                eprintln!("Agent error: {}", e);
            }
        }
    }
    Ok(())
}
