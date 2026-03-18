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
    on_block_found: Option<Arc<dyn Fn(u64) + Send + Sync>>,
}

impl StratumClient {
    pub fn new(url: &str, worker: &str, password: &str, stats: Arc<MiningStats>) -> Self {
        Self {
            url: url.to_string(),
            worker: worker.to_string(),
            password: password.to_string(),
            stats,
            on_block_found: None,
        }
    }

    /// Set a callback invoked when a share is accepted (= block found on solo pools).
    pub fn set_on_block_found(&mut self, cb: Arc<dyn Fn(u64) + Send + Sync>) {
        self.on_block_found = Some(cb);
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
                    let extranonce2_hex = hex::encode(&submission.extranonce2.to_le_bytes()[..session.extranonce2_size]);
                    // Nonce is submitted as big-endian hex of the integer value.
                    // Pools do parseInt(hex, 16) to recover the integer, then serialize
                    // it as LE in the block header. This is standard stratum convention.
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

                        // Store network difficulty derived from nbits
                        let net_diff = mi_core::bitcoin_util::nbits_to_difficulty(template.bits);
                        self.stats.network_difficulty.store(
                            net_diff.to_bits(),
                            Ordering::Relaxed,
                        );

                        let target = session.share_target();
                        tracing::debug!(
                            difficulty = session.current_difficulty,
                            target_hex = hex::encode(&target[0..8]),
                            "Share target"
                        );
                        on_work(template, target);
                    }
                }
                "mining.set_difficulty" => {
                    if let Some(ref params) = response.params {
                        if let Some(arr) = params.as_array() {
                            if let Some(diff) = arr.first().and_then(|v| v.as_f64()) {
                                tracing::info!(difficulty = diff, "Difficulty set");
                                session.current_difficulty = diff;
                                self.stats.pool_difficulty.store(
                                    diff.to_bits(),
                                    Ordering::Relaxed,
                                );
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
            if let Some(ref error_val) = response.error {
                if !error_val.is_null() {
                let error = super::messages::JsonRpcError::from_value(error_val)
                    .unwrap_or(super::messages::JsonRpcError { code: 0, message: error_val.to_string() });
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
                    // Share accept — on solo pools, this IS a block find
                    if response.result.as_ref().and_then(|v| v.as_bool()) == Some(true) {
                        self.stats.shares_accepted.fetch_add(1, Ordering::Relaxed);
                        let block_num =
                            self.stats.blocks_found.fetch_add(1, Ordering::Relaxed) + 1;
                        tracing::warn!(
                            id = id,
                            block = block_num,
                            "BLOCK FOUND! Share accepted."
                        );
                        if let Some(ref cb) = self.on_block_found {
                            cb(block_num);
                        }
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

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::Ordering;

    #[test]
    fn test_stratum_client_new() {
        let stats = MiningStats::new();
        let client = StratumClient::new(
            "stratum+tcp://pool.example.com:3333",
            "worker1",
            "x",
            stats.clone(),
        );
        assert_eq!(client.url, "stratum+tcp://pool.example.com:3333");
        assert_eq!(client.worker, "worker1");
        assert_eq!(client.password, "x");
        assert!(client.on_block_found.is_none());
    }

    #[test]
    fn test_stratum_client_new_preserves_url_variants() {
        let stats = MiningStats::new();
        let client = StratumClient::new("stratum://host:1234", "w", "p", stats);
        assert_eq!(client.url, "stratum://host:1234");
    }

    #[test]
    fn test_stratum_client_new_empty_fields() {
        let stats = MiningStats::new();
        let client = StratumClient::new("", "", "", stats);
        assert_eq!(client.url, "");
        assert_eq!(client.worker, "");
        assert_eq!(client.password, "");
    }

    #[test]
    fn test_set_on_block_found_sets_callback() {
        let stats = MiningStats::new();
        let mut client = StratumClient::new("url", "w", "p", stats);
        assert!(client.on_block_found.is_none());

        let called = Arc::new(std::sync::atomic::AtomicU64::new(0));
        let called_clone = called.clone();
        client.set_on_block_found(Arc::new(move |block_num| {
            called_clone.store(block_num, Ordering::Relaxed);
        }));

        assert!(client.on_block_found.is_some());

        // Invoke the callback and verify it works
        if let Some(ref cb) = client.on_block_found {
            cb(42);
        }
        assert_eq!(called.load(Ordering::Relaxed), 42);
    }

    #[test]
    fn test_set_on_block_found_can_replace_callback() {
        let stats = MiningStats::new();
        let mut client = StratumClient::new("url", "w", "p", stats);

        let val = Arc::new(std::sync::atomic::AtomicU64::new(0));

        let v1 = val.clone();
        client.set_on_block_found(Arc::new(move |n| {
            v1.store(n * 10, Ordering::Relaxed);
        }));
        client.on_block_found.as_ref().unwrap()(5);
        assert_eq!(val.load(Ordering::Relaxed), 50);

        let v2 = val.clone();
        client.set_on_block_found(Arc::new(move |n| {
            v2.store(n * 100, Ordering::Relaxed);
        }));
        client.on_block_found.as_ref().unwrap()(3);
        assert_eq!(val.load(Ordering::Relaxed), 300);
    }

    #[test]
    fn test_share_submission_create_and_clone() {
        let submission = ShareSubmission {
            job_id: "job_abc".to_string(),
            extranonce2: 0x12345678,
            ntime: "65a5e300".to_string(),
            nonce: 0xdeadbeef,
        };

        assert_eq!(submission.job_id, "job_abc");
        assert_eq!(submission.extranonce2, 0x12345678);
        assert_eq!(submission.ntime, "65a5e300");
        assert_eq!(submission.nonce, 0xdeadbeef);

        let cloned = submission.clone();
        assert_eq!(cloned.job_id, submission.job_id);
        assert_eq!(cloned.extranonce2, submission.extranonce2);
        assert_eq!(cloned.ntime, submission.ntime);
        assert_eq!(cloned.nonce, submission.nonce);
    }

    #[test]
    fn test_share_submission_debug_format() {
        let submission = ShareSubmission {
            job_id: "j1".to_string(),
            extranonce2: 1,
            ntime: "aabb".to_string(),
            nonce: 99,
        };
        let debug = format!("{:?}", submission);
        assert!(debug.contains("ShareSubmission"));
        assert!(debug.contains("j1"));
    }

    #[test]
    fn test_share_submission_zero_values() {
        let submission = ShareSubmission {
            job_id: "".to_string(),
            extranonce2: 0,
            ntime: "".to_string(),
            nonce: 0,
        };
        assert_eq!(submission.extranonce2, 0);
        assert_eq!(submission.nonce, 0);
    }

    #[test]
    fn test_share_submission_max_values() {
        let submission = ShareSubmission {
            job_id: "max".to_string(),
            extranonce2: u64::MAX,
            ntime: "ffffffff".to_string(),
            nonce: u32::MAX,
        };
        assert_eq!(submission.extranonce2, u64::MAX);
        assert_eq!(submission.nonce, u32::MAX);
    }

    #[test]
    fn test_handle_response_share_rejected() {
        let stats = MiningStats::new();
        let client = StratumClient::new("url", "w", "p", stats.clone());
        let mut session = StratumSession::new("w".to_string());
        let on_work: Arc<WorkCallback> = Arc::new(Box::new(|_, _| {}));

        // Share rejection: id >= 3 with an error
        let response: JsonRpcResponse = serde_json::from_str(
            r#"{"id":3,"result":null,"error":{"code":23,"message":"Low difficulty share"}}"#,
        )
        .unwrap();

        client
            .handle_response(&mut session, &response, &on_work)
            .unwrap();
        assert_eq!(stats.shares_rejected.load(Ordering::Relaxed), 1);
    }

    #[test]
    fn test_handle_response_share_accepted_increments_blocks() {
        let stats = MiningStats::new();
        let client = StratumClient::new("url", "w", "p", stats.clone());
        let mut session = StratumSession::new("w".to_string());
        let on_work: Arc<WorkCallback> = Arc::new(Box::new(|_, _| {}));

        // Share accepted: id >= 3, result = true
        let response: JsonRpcResponse =
            serde_json::from_str(r#"{"id":5,"result":true,"error":null}"#).unwrap();

        client
            .handle_response(&mut session, &response, &on_work)
            .unwrap();
        assert_eq!(stats.shares_accepted.load(Ordering::Relaxed), 1);
        assert_eq!(stats.blocks_found.load(Ordering::Relaxed), 1);
    }

    #[test]
    fn test_handle_response_share_accepted_fires_callback() {
        let stats = MiningStats::new();
        let mut client = StratumClient::new("url", "w", "p", stats.clone());
        let mut session = StratumSession::new("w".to_string());
        let on_work: Arc<WorkCallback> = Arc::new(Box::new(|_, _| {}));

        let found_block = Arc::new(std::sync::atomic::AtomicU64::new(0));
        let found_clone = found_block.clone();
        client.set_on_block_found(Arc::new(move |n| {
            found_clone.store(n, Ordering::Relaxed);
        }));

        let response: JsonRpcResponse =
            serde_json::from_str(r#"{"id":10,"result":true,"error":null}"#).unwrap();

        client
            .handle_response(&mut session, &response, &on_work)
            .unwrap();
        assert_eq!(found_block.load(Ordering::Relaxed), 1);
    }

    #[test]
    fn test_handle_response_multiple_rejections() {
        let stats = MiningStats::new();
        let client = StratumClient::new("url", "w", "p", stats.clone());
        let mut session = StratumSession::new("w".to_string());
        let on_work: Arc<WorkCallback> = Arc::new(Box::new(|_, _| {}));

        for id in 3..8 {
            let json = format!(
                r#"{{"id":{},"result":null,"error":{{"code":23,"message":"bad"}}}}"#,
                id
            );
            let response: JsonRpcResponse = serde_json::from_str(&json).unwrap();
            client
                .handle_response(&mut session, &response, &on_work)
                .unwrap();
        }
        assert_eq!(stats.shares_rejected.load(Ordering::Relaxed), 5);
    }

    #[test]
    fn test_handle_response_subscribe_sets_extranonce() {
        let stats = MiningStats::new();
        let client = StratumClient::new("url", "w", "p", stats.clone());
        let mut session = StratumSession::new("w".to_string());
        let on_work: Arc<WorkCallback> = Arc::new(Box::new(|_, _| {}));

        // Subscribe response has id=1
        let response: JsonRpcResponse = serde_json::from_str(
            r#"{"id":1,"result":[[],"aabbccdd",8],"error":null}"#,
        )
        .unwrap();

        client
            .handle_response(&mut session, &response, &on_work)
            .unwrap();
        assert_eq!(session.extranonce1, vec![0xaa, 0xbb, 0xcc, 0xdd]);
        assert_eq!(session.extranonce2_size, 8);
    }

    #[test]
    fn test_handle_response_set_difficulty() {
        let stats = MiningStats::new();
        let client = StratumClient::new("url", "w", "p", stats.clone());
        let mut session = StratumSession::new("w".to_string());
        let on_work: Arc<WorkCallback> = Arc::new(Box::new(|_, _| {}));

        let response: JsonRpcResponse = serde_json::from_str(
            r#"{"id":null,"method":"mining.set_difficulty","params":[256.0]}"#,
        )
        .unwrap();

        client
            .handle_response(&mut session, &response, &on_work)
            .unwrap();
        assert_eq!(session.current_difficulty, 256.0);
    }

    #[test]
    fn test_handle_response_unknown_method_no_error() {
        let stats = MiningStats::new();
        let client = StratumClient::new("url", "w", "p", stats.clone());
        let mut session = StratumSession::new("w".to_string());
        let on_work: Arc<WorkCallback> = Arc::new(Box::new(|_, _| {}));

        let response: JsonRpcResponse = serde_json::from_str(
            r#"{"id":null,"method":"mining.unknown_method","params":[]}"#,
        )
        .unwrap();

        // Should succeed without error
        let result = client.handle_response(&mut session, &response, &on_work);
        assert!(result.is_ok());
    }

    #[test]
    fn test_handle_response_error_on_id_less_than_3() {
        let stats = MiningStats::new();
        let client = StratumClient::new("url", "w", "p", stats.clone());
        let mut session = StratumSession::new("w".to_string());
        let on_work: Arc<WorkCallback> = Arc::new(Box::new(|_, _| {}));

        // Error on id=2 (authorize) should NOT increment shares_rejected
        let response: JsonRpcResponse = serde_json::from_str(
            r#"{"id":2,"result":null,"error":{"code":-1,"message":"not authorized"}}"#,
        )
        .unwrap();

        client
            .handle_response(&mut session, &response, &on_work)
            .unwrap();
        assert_eq!(stats.shares_rejected.load(Ordering::Relaxed), 0);
    }

    #[test]
    fn test_handle_response_authorize_success() {
        let stats = MiningStats::new();
        let client = StratumClient::new("url", "w", "p", stats.clone());
        let mut session = StratumSession::new("w".to_string());
        let on_work: Arc<WorkCallback> = Arc::new(Box::new(|_, _| {}));

        let response: JsonRpcResponse =
            serde_json::from_str(r#"{"id":2,"result":true,"error":null}"#).unwrap();

        // Should not error; no stats to check beyond that
        let result = client.handle_response(&mut session, &response, &on_work);
        assert!(result.is_ok());
    }

    #[test]
    fn test_handle_response_authorize_failure() {
        let stats = MiningStats::new();
        let client = StratumClient::new("url", "w", "p", stats.clone());
        let mut session = StratumSession::new("w".to_string());
        let on_work: Arc<WorkCallback> = Arc::new(Box::new(|_, _| {}));

        let response: JsonRpcResponse =
            serde_json::from_str(r#"{"id":2,"result":false,"error":null}"#).unwrap();

        let result = client.handle_response(&mut session, &response, &on_work);
        assert!(result.is_ok());
    }

    #[test]
    fn test_handle_response_notify_calls_on_work() {
        let stats = MiningStats::new();
        let client = StratumClient::new("url", "w", "p", stats.clone());
        let mut session = StratumSession::new("w".to_string());
        session.set_extranonce("aabb", 4);

        let work_received = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let wr = work_received.clone();
        let on_work: Arc<WorkCallback> = Arc::new(Box::new(move |_template, _target| {
            wr.store(true, Ordering::Relaxed);
        }));

        let prev_hash = "0".repeat(64);
        let json = format!(
            r#"{{"id":null,"method":"mining.notify","params":["job1","{}","01020304","05060708",[],"20000000","1d00ffff","65a5e300",true]}}"#,
            prev_hash
        );
        let response: JsonRpcResponse = serde_json::from_str(&json).unwrap();

        client
            .handle_response(&mut session, &response, &on_work)
            .unwrap();
        assert!(work_received.load(Ordering::Relaxed));
        assert!(session.current_job.is_some());
    }

    #[test]
    fn test_nonce_hex_encoding_byte_order() {
        // Nonce is submitted as big-endian hex of the integer value.
        // Pools do parseInt(hex, 16) to recover the integer, then serialize
        // it as LE in the block header. This is standard stratum convention.
        assert_eq!(format!("{:08x}", 0xDEADBEEFu32), "deadbeef");
        assert_eq!(format!("{:08x}", 42u32), "0000002a");
        assert_eq!(format!("{:08x}", 0u32), "00000000");
        assert_eq!(format!("{:08x}", u32::MAX), "ffffffff");
    }

    #[test]
    fn test_nonce_hex_roundtrips_through_parseint() {
        // The critical invariant: the pool does parseInt(nonce_hex, 16) to get the
        // integer, then writes it as LE bytes in the header. This must produce the
        // same bytes as what the miner used when hashing.
        for nonce in [0u32, 1, 42, 0xDEADBEEF, u32::MAX, 0x12345678] {
            let nonce_hex = format!("{:08x}", nonce);

            // Pool side: parseInt → LE bytes
            let pool_nonce = u32::from_str_radix(&nonce_hex, 16).unwrap();
            let pool_bytes = pool_nonce.to_le_bytes();

            // Miner side: nonce stored as LE bytes in header
            let miner_bytes = nonce.to_le_bytes();

            assert_eq!(
                pool_bytes, miner_bytes,
                "Nonce {nonce} (hex {nonce_hex}): pool bytes {:?} != miner bytes {:?}",
                pool_bytes, miner_bytes
            );
        }
    }

    #[test]
    fn test_nonce_hex_is_not_le_bytes() {
        // Guard against the previous bug where nonce was encoded as LE hex bytes.
        // For asymmetric nonces, LE hex != BE hex, so the pool would reconstruct
        // a different nonce and reject the share.
        let nonce: u32 = 0xDEADBEEF;
        let correct_hex = format!("{:08x}", nonce);     // "deadbeef" (BE)
        let wrong_hex = hex::encode(nonce.to_le_bytes()); // "efbeadde" (LE)

        assert_eq!(correct_hex, "deadbeef");
        assert_eq!(wrong_hex, "efbeadde");
        assert_ne!(correct_hex, wrong_hex, "BE and LE hex must differ for asymmetric nonces");

        // Verify correct encoding roundtrips through parseInt
        let pool_nonce = u32::from_str_radix(&correct_hex, 16).unwrap();
        assert_eq!(pool_nonce, nonce);

        // Verify wrong encoding does NOT roundtrip
        let wrong_nonce = u32::from_str_radix(&wrong_hex, 16).unwrap();
        assert_ne!(wrong_nonce, nonce, "LE hex encoding would give pool the wrong nonce");
    }

    #[test]
    fn test_extranonce2_hex_encoding_byte_order() {
        // extranonce2 is also placed as raw bytes into the coinbase, LE encoded.
        let en2: u64 = 0x42;
        let size = 4;
        let hex_str = hex::encode(&en2.to_le_bytes()[..size]);
        assert_eq!(hex_str, "42000000");

        // Zero extranonce2
        let hex_str = hex::encode(&0u64.to_le_bytes()[..4]);
        assert_eq!(hex_str, "00000000");

        // Larger extranonce2 with size=8
        let en2: u64 = 0x0102030405060708;
        let hex_str = hex::encode(&en2.to_le_bytes()[..8]);
        assert_eq!(hex_str, "0807060504030201");
    }

    #[test]
    fn test_submission_roundtrip_hash_matches() {
        // End-to-end: construct session → process notify → build header → hash →
        // format submission → reconstruct header on "pool side" → SHA-256d → verify match.
        use mi_core::bitcoin_util::sha256d;

        let mut session = StratumSession::new("w".to_string());
        session.set_extranonce("aabbccdd", 4);

        let prev_hash_hex = "0".repeat(64);
        let coinbase1_hex = "01000000010000000000000000000000000000000000000000000000000000000000000000ffffffff0704";
        let coinbase2_hex = "0101000000000000001976a914000000000000000000000000000000000000000088ac00000000";
        let ntime_hex = "65a5e300";
        let nbits_hex = "1d00ffff";
        let version_hex = "20000000";

        let notify = super::super::messages::MiningNotify {
            job_id: "roundtrip_test".to_string(),
            prev_hash: prev_hash_hex.clone(),
            coinbase_1: coinbase1_hex.to_string(),
            coinbase_2: coinbase2_hex.to_string(),
            merkle_branches: vec![],
            version: version_hex.to_string(),
            nbits: nbits_hex.to_string(),
            ntime: ntime_hex.to_string(),
            clean_jobs: true,
        };

        let template = session.process_notify(notify).unwrap();

        // Miner side: build header and hash with a specific nonce
        let extranonce2: u64 = 0;
        let test_nonce: u32 = 0xDEADBEEF;

        let (_header, mut header_bytes) = template.build_header(extranonce2);
        header_bytes[76..80].copy_from_slice(&test_nonce.to_le_bytes());

        let miner_hash = sha256d(&header_bytes);

        // Format submission the same way the real code does
        let extranonce2_hex = hex::encode(&extranonce2.to_le_bytes()[..session.extranonce2_size]);
        let nonce_hex = format!("{:08x}", test_nonce);

        // Pool side: reconstruct the header from the submitted hex values
        // 1. Rebuild coinbase with extranonce1 + extranonce2
        let cb1_bytes = hex::decode(coinbase1_hex).unwrap();
        let cb2_bytes = hex::decode(coinbase2_hex).unwrap();
        let en2_bytes = hex::decode(&extranonce2_hex).unwrap();

        let mut coinbase_tx = Vec::new();
        coinbase_tx.extend_from_slice(&cb1_bytes);
        coinbase_tx.extend_from_slice(&session.extranonce1);
        coinbase_tx.extend_from_slice(&en2_bytes);
        coinbase_tx.extend_from_slice(&cb2_bytes);

        let coinbase_hash = sha256d(&coinbase_tx);

        // 2. Compute merkle root (no branches)
        let merkle_root = coinbase_hash;

        // 3. Build header
        let version = i32::from_str_radix(version_hex, 16).unwrap();
        let bits = u32::from_str_radix(nbits_hex, 16).unwrap();
        let timestamp = u32::from_str_radix(ntime_hex, 16).unwrap();

        // Decode prev_hash with stratum's 4-byte group reversal
        let prev_hash_bytes = hex::decode(&prev_hash_hex).unwrap();
        let mut prev_hash = [0u8; 32];
        for i in 0..8 {
            for j in 0..4 {
                prev_hash[i * 4 + j] = prev_hash_bytes[i * 4 + (3 - j)];
            }
        }

        let mut pool_header = [0u8; 80];
        pool_header[0..4].copy_from_slice(&version.to_le_bytes());
        pool_header[4..36].copy_from_slice(&prev_hash);
        pool_header[36..68].copy_from_slice(&merkle_root);
        pool_header[68..72].copy_from_slice(&timestamp.to_le_bytes());
        pool_header[72..76].copy_from_slice(&bits.to_le_bytes());
        // Pool does parseInt(nonce_hex, 16) to get the integer, then writes LE
        let pool_nonce = u32::from_str_radix(&nonce_hex, 16).unwrap();
        pool_header[76..80].copy_from_slice(&pool_nonce.to_le_bytes());

        // 4. Hash and compare
        let pool_hash = sha256d(&pool_header);

        assert_eq!(
            miner_hash, pool_hash,
            "Miner hash and pool-reconstructed hash must match!\n\
             miner header: {}\n\
             pool  header: {}\n\
             nonce_hex: {nonce_hex}\n\
             extranonce2_hex: {extranonce2_hex}",
            hex::encode(header_bytes),
            hex::encode(pool_header),
        );
    }

    #[test]
    fn test_submission_roundtrip_with_extranonce2_nonzero() {
        // Verify that GPU work (extranonce2=1) also roundtrips correctly.
        // This ensures the CPU/GPU separation via different extranonce2 values
        // produces shares that the pool can validate.
        use mi_core::bitcoin_util::sha256d;

        let mut session = StratumSession::new("w".to_string());
        session.set_extranonce("aabbccdd", 4);

        let notify = super::super::messages::MiningNotify {
            job_id: "gpu_roundtrip".to_string(),
            prev_hash: "0".repeat(64),
            coinbase_1: "01000000010000000000000000000000000000000000000000000000000000000000000000ffffffff0704".to_string(),
            coinbase_2: "0101000000000000001976a914000000000000000000000000000000000000000088ac00000000".to_string(),
            merkle_branches: vec![],
            version: "20000000".to_string(),
            nbits: "1d00ffff".to_string(),
            ntime: "65a5e300".to_string(),
            clean_jobs: true,
        };

        let template = session.process_notify(notify).unwrap();

        // GPU side: build header with extranonce2=1
        let extranonce2: u64 = 1;
        let test_nonce: u32 = 0x42424242;
        let (_header, mut header_bytes) = template.build_header(extranonce2);
        header_bytes[76..80].copy_from_slice(&test_nonce.to_le_bytes());
        let miner_hash = sha256d(&header_bytes);

        // Format submission with extranonce2=1
        let extranonce2_hex = hex::encode(&extranonce2.to_le_bytes()[..session.extranonce2_size]);
        let nonce_hex = format!("{:08x}", test_nonce);

        // Verify extranonce2_hex is different from zero
        assert_eq!(extranonce2_hex, "01000000");
        assert_ne!(extranonce2_hex, "00000000");

        // Pool side: reconstruct header using submitted extranonce2=1
        let cb1 = hex::decode("01000000010000000000000000000000000000000000000000000000000000000000000000ffffffff0704").unwrap();
        let cb2 = hex::decode("0101000000000000001976a914000000000000000000000000000000000000000088ac00000000").unwrap();
        let en2_bytes = hex::decode(&extranonce2_hex).unwrap();

        let mut coinbase = Vec::new();
        coinbase.extend_from_slice(&cb1);
        coinbase.extend_from_slice(&session.extranonce1);
        coinbase.extend_from_slice(&en2_bytes);
        coinbase.extend_from_slice(&cb2);

        let coinbase_hash = sha256d(&coinbase);
        let merkle_root = coinbase_hash;

        let mut pool_header = [0u8; 80];
        pool_header[0..4].copy_from_slice(&0x20000000i32.to_le_bytes());
        // prev_hash is all zeros (with 4-byte group reversal = still zeros)
        pool_header[36..68].copy_from_slice(&merkle_root);
        pool_header[68..72].copy_from_slice(&0x65a5e300u32.to_le_bytes());
        pool_header[72..76].copy_from_slice(&0x1d00ffffu32.to_le_bytes());
        // Pool does parseInt(nonce_hex, 16) → integer → LE bytes in header
        let pool_nonce_gpu = u32::from_str_radix(&nonce_hex, 16).unwrap();
        pool_header[76..80].copy_from_slice(&pool_nonce_gpu.to_le_bytes());

        let pool_hash = sha256d(&pool_header);
        assert_eq!(miner_hash, pool_hash, "GPU extranonce2=1 roundtrip must match");

        // Also verify that extranonce2=0 and extranonce2=1 produce DIFFERENT headers
        let (_, header0) = template.build_header(0);
        assert_ne!(&header_bytes[36..68], &header0[36..68],
            "extranonce2=0 and extranonce2=1 must have different merkle roots");
    }

    #[test]
    fn test_handle_response_share_result_false_no_accept() {
        let stats = MiningStats::new();
        let client = StratumClient::new("url", "w", "p", stats.clone());
        let mut session = StratumSession::new("w".to_string());
        let on_work: Arc<WorkCallback> = Arc::new(Box::new(|_, _| {}));

        // result is false (not true) - should not increment shares_accepted
        let response: JsonRpcResponse =
            serde_json::from_str(r#"{"id":4,"result":false,"error":null}"#).unwrap();

        client
            .handle_response(&mut session, &response, &on_work)
            .unwrap();
        assert_eq!(stats.shares_accepted.load(Ordering::Relaxed), 0);
        assert_eq!(stats.blocks_found.load(Ordering::Relaxed), 0);
    }
}
