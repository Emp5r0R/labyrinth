#[tokio::main]
async fn main() -> anyhow::Result<()> {
    labyrinth::cli::run_server_cli().await
}
