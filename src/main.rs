#[tokio::main]
async fn main() -> anyhow::Result<()> {
    labyrinth::cli::run_labyrinth_cli().await
}
