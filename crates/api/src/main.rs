//! PUSU ingress sunucusu. Store'a bağlanır, router'ı bind edip serve eder.
//! Bütün mantık [`pusu_api`] kütüphanesinde.

use std::process::ExitCode;

#[tokio::main]
async fn main() -> ExitCode {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "info,pusu_api=debug".into()),
        )
        .init();

    match run().await {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            tracing::error!("sunucu durdu: {e}");
            ExitCode::FAILURE
        }
    }
}

async fn run() -> Result<(), Box<dyn std::error::Error>> {
    let database_url =
        std::env::var("PUSU_DATABASE_URL").map_err(|_| "PUSU_DATABASE_URL tanımlı değil")?;
    let addr = std::env::var("PUSU_API_ADDR").unwrap_or_else(|_| "0.0.0.0:3000".into());

    // Blob'lar at-rest şifreli; anahtar yoksa/bozuksa hemen dur (node ile
    // AYNI PUSU_BLOB_KEY set edilmeli, yoksa yazılan blob'lar okunamaz).
    pusu_store::check_key()?;

    let store = pusu_store::Store::connect(&database_url).await?;
    let app = pusu_api::router(store);

    let listener = tokio::net::TcpListener::bind(&addr).await?;
    tracing::info!("PUSU api dinliyor: {addr}");
    axum::serve(listener, app).await?;
    Ok(())
}
