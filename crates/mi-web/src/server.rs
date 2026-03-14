use crate::routes;
use crate::sse;
use axum::Router;
use mi_core::{LiveConfig, MiningStats};
use std::sync::Arc;

pub async fn start_server(
    bind: &str,
    stats: Arc<MiningStats>,
    live_config: Arc<LiveConfig>,
) -> Result<(), mi_core::MiMinerError> {
    let state = routes::AppState {
        stats,
        live_config,
    };

    let app = Router::new()
        .route("/", axum::routing::get(routes::index))
        .route("/api/stats", axum::routing::get(routes::stats_json))
        .route("/api/wallet", axum::routing::get(routes::wallet_status))
        .route("/api/wallet/generate", axum::routing::post(routes::wallet_generate))
        .route("/api/wallet/address", axum::routing::post(routes::wallet_set_address))
        .route("/api/mining/pause", axum::routing::post(routes::mining_pause))
        .route("/api/mining/resume", axum::routing::post(routes::mining_resume))
        .route("/api/mining/stop", axum::routing::post(routes::mining_stop))
        .route("/api/config", axum::routing::get(routes::config_get))
        .route("/api/config", axum::routing::post(routes::config_save))
        .route("/api/hardware", axum::routing::get(routes::hardware_info))
        .route("/api/optimization", axum::routing::get(routes::optimization_check))
        .route("/api/config/auto", axum::routing::post(routes::auto_configure))
        .route("/api/test/connection", axum::routing::post(routes::test_connection))
        .route("/api/test/benchmark", axum::routing::post(routes::test_benchmark))
        .route("/events", axum::routing::get(sse::stats_stream))
        .with_state(state);

    let listener = tokio::net::TcpListener::bind(bind)
        .await
        .map_err(|e| mi_core::MiMinerError::Web(format!("Bind failed on {bind}: {e}")))?;

    tracing::info!("Dashboard: http://{bind}");

    axum::serve(listener, app)
        .await
        .map_err(|e| mi_core::MiMinerError::Web(format!("Server error: {e}")))?;

    Ok(())
}
