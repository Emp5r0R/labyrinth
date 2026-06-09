#[tokio::main]
async fn main() -> anyhow::Result<()> {
    labyrinth::cli::run_dweller_cli().await
}
