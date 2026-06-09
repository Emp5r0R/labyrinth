#[tokio::main]
async fn main() -> anyhow::Result<()> {
    labyrinth::cli::run_agent_cli().await
}
