use super::messages::{
    build_submit, JsonRpcRequest, JsonRpcResponse, MiningNotify, SubscribeResult,
};
use super::session::StratumSession;
use mi_core::MiningStats;
use mi_mining::block::BlockTemplate;
use std::sync::atomic::Ordering;
use std::sync::Arc;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::TcpStream;
use tokio::sync::mpsc;

/// Callback when new work is available from stratum.
pub type WorkCallback = Box<dyn Fn(BlockTemplate, [u8; 32]) + Send + Sync>;

/// Stratum v1 client.
pub struct StratumClient {
    url: String,
    worker: String,
    password: String,
    stats: Arc<MiningStats>,
}

impl StratumClient {
    pub fn new(url: &str, worker: &str, password: &str, stats: Arc<MiningStats>) -> Self {
        Self {
            url: url.to_string(),
            worker: worker.to_string(),
            password: password.to_string(),
            stats,
        }
    }

    /// Run the stratum client. Connects, subscribes, authorizes, and processes notifications.
    /// Calls `on_work` whenever new mining work is available.
    /// Calls `submit_rx` to receive share submissions from the mining engine.
    pub async fn run(
        &self,
        on_work: Arc<WorkCallback>,
        mut submit_rx: mpsc::Receiver<ShareSubmission>,
    ) -> Result<(), mi_core::MiMinerError> {
        loop {
            match self.connect_and_run(&on_work, &mut submit_rx).await {
                Ok(()) => {
                    tracing::info!("Stratum connection closed cleanly");
                    break;
                }
                Err(e) => {
                    tracing::error!("Stratum connection error: {e}");
                    if self.stats.should_stop.load(Ordering::Relaxed) {
                        break;
                    }
                    tracing::info!("Reconnecting in 5 seconds...");
                    tokio::time::sleep(std::time::Duration::from_secs(5)).await;
                }
            }
        }
        Ok(())
    }

    async fn connect_and_run(
        &self,
        on_work: &Arc<WorkCallback>,
        submit_rx: &mut mpsc::Receiver<ShareSubmission>,
    ) -> Result<(), mi_core::MiMinerError> {
        // Parse URL: stratum+tcp://host:port
        let addr = self
            .url
            .trim_start_matches("stratum+tcp://")
            .trim_start_matches("stratum://");

        tracing::info!("Connecting to {addr}...");
        let stream = TcpStream::connect(addr)
            .await
            .map_err(|e| mi_core::MiMinerError::Network(format!("Connect failed: {e}")))?;

        tracing::info!("Connected to {addr}");

        let (reader, mut writer) = stream.into_split();
        let mut reader = BufReader::new(reader);
        let mut session = StratumSession::new(self.worker.clone());
        let mut request_id: u64 = 1;

        // Subscribe
        let subscribe = JsonRpcRequest {
            id: request_id,
            method: "mining.subscribe".to_string(),
            params: vec![serde_json::Value::String("mi-miner/0.1.0".to_string())],
        };
        request_id += 1;
        send_request(&mut writer, &subscribe).await?;

        // Authorize
        let authorize = JsonRpcRequest {
            id: request_id,
            method: "mining.authorize".to_string(),
            params: vec![
                serde_json::Value::String(self.worker.clone()),
                serde_json::Value::String(self.password.clone()),
            ],
        };
        request_id += 1;
        send_request(&mut writer, &authorize).await?;

        // Read responses and notifications
        let mut line = String::new();
        loop {
            line.clear();

            tokio::select! {
                result = reader.read_line(&mut line) => {
                    let n = result.map_err(|e| mi_core::MiMinerError::Network(format!("Read error: {e}")))?;
                    if n == 0 {
                        return Err(mi_core::MiMinerError::Network("Connection closed".to_string()));
                    }

                    let response: JsonRpcResponse = match serde_json::from_str(line.trim()) {
                        Ok(r) => r,
                        Err(e) => {
                            tracing::warn!("Failed to parse response: {e} — {}", line.trim());
                            continue;
                        }
                    };

                    self.handle_response(&mut session, &response, on_work)?;
                }

                Some(submission) = submit_rx.recv() => {
                    let extranonce2_hex = format!("{:0>width$x}", submission.extranonce2, width = session.extranonce2_size * 2);
                    let nonce_hex = format!("{:08x}", submission.nonce);

                    let req = build_submit(
                        request_id,
                        session.worker(),
                        &submission.job_id,
                        &extranonce2_hex,
                        &submission.ntime,
                        &nonce_hex,
                    );
                    request_id += 1;

                    self.stats.shares_submitted.fetch_add(1, Ordering::Relaxed);
                    tracing::info!(
                        job = submission.job_id,
                        nonce = nonce_hex,
                        "Submitting share"
                    );

                    send_request(&mut writer, &req).await?;
                }

                _ = tokio::time::sleep(std::time::Duration::from_secs(300)) => {
                    // Timeout — connection might be dead
                    return Err(mi_core::MiMinerError::Network("Read timeout".to_string()));
                }
            }

            if self.stats.should_stop.load(Ordering::Relaxed) {
                break;
            }
        }

        Ok(())
    }

    fn handle_response(
        &self,
        session: &mut StratumSession,
        response: &JsonRpcResponse,
        on_work: &Arc<WorkCallback>,
    ) -> Result<(), mi_core::MiMinerError> {
        // Server notification (no id, has method)
        if let Some(ref method) = response.method {
            match method.as_str() {
                "mining.notify" => {
                    if let Some(ref params) = response.params {
                        let params_arr = params
                            .as_array()
                            .ok_or_else(|| {
                                mi_core::MiMinerError::Stratum("notify params not array".into())
                            })?;

                        let notify = MiningNotify::from_params(params_arr)
                            .map_err(|e| mi_core::MiMinerError::Stratum(e))?;

                        tracing::info!(
                            job = notify.job_id,
                            clean = notify.clean_jobs,
                            "New mining job"
                        );

                        let template = session
                            .process_notify(notify)
                            .map_err(|e| mi_core::MiMinerError::Stratum(e))?;

                        let target = session.share_target();
                        on_work(template, target);
                    }
                }
                "mining.set_difficulty" => {
                    if let Some(ref params) = response.params {
                        if let Some(arr) = params.as_array() {
                            if let Some(diff) = arr.first().and_then(|v| v.as_f64()) {
                                tracing::info!(difficulty = diff, "Difficulty set");
                                session.current_difficulty = diff;
                            }
                        }
                    }
                }
                _ => {
                    tracing::debug!(method = method, "Unknown server method");
                }
            }
            return Ok(());
        }

        // Response to our request
        if let Some(id) = response.id {
            if let Some(ref error) = response.error {
                if id >= 3 {
                    // Share rejection
                    tracing::warn!(
                        id = id,
                        code = error.code,
                        msg = error.message,
                        "Share rejected"
                    );
                    self.stats.shares_rejected.fetch_add(1, Ordering::Relaxed);
                } else {
                    tracing::error!(id = id, msg = error.message, "Request failed");
                }
                return Ok(());
            }

            match id {
                1 => {
                    // Subscribe response
                    if let Some(ref result) = response.result {
                        let sub = SubscribeResult::from_result(result)
                            .map_err(|e| mi_core::MiMinerError::Stratum(e))?;
                        tracing::info!(
                            extranonce1 = sub.extranonce1,
                            extranonce2_size = sub.extranonce2_size,
                            "Subscribed"
                        );
                        session.set_extranonce(&sub.extranonce1, sub.extranonce2_size);
                    }
                }
                2 => {
                    // Authorize response
                    let authorized = response
                        .result
                        .as_ref()
                        .and_then(|v| v.as_bool())
                        .unwrap_or(false);
                    if authorized {
                        tracing::info!("Worker authorized");
                    } else {
                        tracing::error!("Worker authorization FAILED");
                    }
                }
                _ => {
                    // Share accept
                    if response.result.as_ref().and_then(|v| v.as_bool()) == Some(true) {
                        tracing::info!(id = id, "Share accepted");
                        self.stats.shares_accepted.fetch_add(1, Ordering::Relaxed);
                    }
                }
            }
        }

        Ok(())
    }
}

/// Share submission from the mining engine to the stratum client.
#[derive(Debug, Clone)]
pub struct ShareSubmission {
    pub job_id: String,
    pub extranonce2: u64,
    pub ntime: String,
    pub nonce: u32,
}

async fn send_request(
    writer: &mut tokio::net::tcp::OwnedWriteHalf,
    request: &JsonRpcRequest,
) -> Result<(), mi_core::MiMinerError> {
    let mut json = serde_json::to_string(request)
        .map_err(|e| mi_core::MiMinerError::Stratum(format!("Serialize error: {e}")))?;
    json.push('\n');

    writer
        .write_all(json.as_bytes())
        .await
        .map_err(|e| mi_core::MiMinerError::Network(format!("Write error: {e}")))?;

    tracing::debug!(method = request.method, id = request.id, "Sent request");
    Ok(())
}
