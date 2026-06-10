use crate::transport::TransportMode;
use crate::{agent, server, styling};
use clap::{Args, Parser, Subcommand};
use tracing::{info, Level};
use tracing_subscriber::fmt::format::FmtSpan;

#[derive(Parser)]
#[command(author, version, about, long_about = None)]
pub struct LabyrinthCli {
    #[command(subcommand)]
    command: LabyrinthCommand,
}

#[derive(Subcommand)]
pub enum LabyrinthCommand {
    /// Start Labyrinth server
    Server(ServerArgs),
    /// Connect agent to server
    Agent(AgentArgs),
    /// Start a persistent inbound dweller listener
    Dweller(DwellerArgs),
}

#[derive(Parser)]
#[command(author, version, about = "Start Labyrinth server", long_about = None)]
pub struct ServerCli {
    #[command(flatten)]
    args: ServerArgs,
}

#[derive(Parser)]
#[command(author, version, about = "Connect Labyrinth agent to server", long_about = None)]
pub struct AgentCli {
    #[command(flatten)]
    args: AgentArgs,
}

#[derive(Parser)]
#[command(author, version, about = "Start Labyrinth dweller listener", long_about = None)]
pub struct DwellerCli {
    #[command(flatten)]
    args: DwellerArgs,
}

#[derive(Args, Clone)]
pub struct ServerArgs {
    /// Server listening address
    #[arg(short, long, default_value = "0.0.0.0:44344")]
    pub listen_addr: String,
    /// Disable authentication (for testing only)
    #[arg(long)]
    pub no_auth: bool,
    /// Skip interactive mode and run headless
    #[arg(long)]
    pub headless: bool,
    /// TUN interface name for tunneling
    #[arg(long)]
    pub interface: Option<String>,
    /// Target subnet to route (e.g., 192.168.1.0/24)
    #[arg(long)]
    pub route: Option<String>,
    /// Domain for TLS certificate
    #[arg(long)]
    pub domain: Option<String>,
    /// Agent transport to listen for
    #[arg(long, value_enum, default_value_t = TransportMode::Tcp)]
    pub transport: TransportMode,
    /// Disable the read-only browser visualization dashboard
    #[arg(long)]
    pub no_web_ui: bool,
    /// Browser visualization dashboard listening address
    #[arg(long, default_value = "127.0.0.1:44777")]
    pub web_ui_addr: String,
}

#[derive(Args, Clone)]
pub struct AgentArgs {
    /// Server address to connect to
    #[arg(short, long)]
    pub server: String,
    /// Base64 encoded server certificate
    #[arg(long)]
    pub cert: Option<String>,
    /// Accept certificate with specific SHA256 fingerprint
    #[arg(short, long)]
    pub fingerprint: Option<String>,
    /// SOCKS5 proxy URL (e.g., socks5://user:pass@127.0.0.1:1080)
    #[arg(short, long)]
    pub proxy: Option<String>,
    /// Transport used to connect to the server
    #[arg(long, value_enum, default_value_t = TransportMode::Tcp)]
    pub transport: TransportMode,
    /// Auto-retry on connection failure
    #[arg(short, long)]
    pub retry: bool,
}

#[derive(Args, Clone)]
pub struct DwellerArgs {
    /// Listen address for inbound server connections
    #[arg(short, long, default_value = "0.0.0.0:45454")]
    pub listen: String,
    /// TLS certificate PEM path
    #[arg(long)]
    pub cert_file: String,
    /// TLS private key PEM path
    #[arg(long)]
    pub key_file: String,
    /// Stable dweller identifier
    #[arg(long)]
    pub id: String,
    /// Optional display name override
    #[arg(long)]
    pub name: Option<String>,
    /// Shared dweller auth key used by the server to authenticate
    #[arg(long)]
    pub auth_key: String,
}

pub async fn run_labyrinth_cli() -> anyhow::Result<()> {
    print_logo();
    let cli = LabyrinthCli::parse();
    run_command(cli.command).await
}

pub async fn run_server_cli() -> anyhow::Result<()> {
    print_logo();
    let cli = ServerCli::parse();
    run_server(cli.args).await
}

pub async fn run_agent_cli() -> anyhow::Result<()> {
    print_logo();
    let cli = AgentCli::parse();
    run_agent(cli.args).await
}

pub async fn run_dweller_cli() -> anyhow::Result<()> {
    print_logo();
    let cli = DwellerCli::parse();
    run_dweller(cli.args).await
}

async fn run_command(command: LabyrinthCommand) -> anyhow::Result<()> {
    match command {
        LabyrinthCommand::Server(args) => run_server(args).await,
        LabyrinthCommand::Agent(args) => run_agent(args).await,
        LabyrinthCommand::Dweller(args) => run_dweller(args).await,
    }
}

async fn run_server(args: ServerArgs) -> anyhow::Result<()> {
    let log_level = if args.headless {
        Level::INFO
    } else {
        Level::ERROR
    };
    init_logging(log_level);

    if args.headless {
        println!(
            "{}",
            styling::format_success_msg(
                styling::SUCCESS_INDICATOR,
                "Starting Labyrinth server in headless mode"
            )
        );
        let _headless_compat = (args.interface, args.route);
        server::run_headless_server(
            &args.listen_addr,
            args.no_auth,
            args.domain,
            args.transport,
            !args.no_web_ui,
            &args.web_ui_addr,
        )
        .await?;
    } else {
        println!(
            "{}",
            styling::format_success_msg(
                styling::SUCCESS_INDICATOR,
                "Starting Labyrinth server in interactive mode"
            )
        );
        server::run_interactive_server(
            &args.listen_addr,
            args.no_auth,
            args.domain,
            args.transport,
            !args.no_web_ui,
            &args.web_ui_addr,
        )
        .await?;
    }

    Ok(())
}

async fn run_agent(args: AgentArgs) -> anyhow::Result<()> {
    init_logging(Level::INFO);
    info!("Connecting agent to {}", args.server);
    agent::run_agent(
        &args.server,
        args.cert,
        args.fingerprint,
        args.proxy,
        args.transport,
        args.retry,
    )
    .await?;
    Ok(())
}

async fn run_dweller(args: DwellerArgs) -> anyhow::Result<()> {
    init_logging(Level::INFO);
    info!("Starting dweller listener on {}", args.listen);
    agent::run_dweller(
        &args.listen,
        &args.cert_file,
        &args.key_file,
        &args.id,
        args.name,
        args.auth_key,
    )
    .await?;
    Ok(())
}

fn print_logo() {
    println!("{}", styling::format_logo());
}

fn init_logging(log_level: Level) {
    let _ = tracing_subscriber::fmt()
        .with_span_events(FmtSpan::CLOSE)
        .with_target(false)
        .with_level(true)
        .with_max_level(log_level)
        .try_init();
}
