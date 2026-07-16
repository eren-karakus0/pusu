//! PUSU çalışan düğümü. Yapılandırmayı ortamdan okur, watcher döngüsünü
//! çalıştırır. Bütün mantık [`pusu_node`] kütüphanesinde; buradaki tek iş
//! günlüğü kurup [`pusu_node::run`]'u çağırmak.

use std::process::ExitCode;

#[tokio::main]
async fn main() -> ExitCode {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "info,pusu_node=debug".into()),
        )
        .init();

    let cfg = match pusu_node::Config::from_env() {
        Ok(c) => c,
        Err(e) => {
            eprintln!("yapılandırma hatası: {e}");
            return ExitCode::FAILURE;
        }
    };

    match pusu_node::run(cfg).await {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            tracing::error!("düğüm durdu: {e}");
            ExitCode::FAILURE
        }
    }
}
