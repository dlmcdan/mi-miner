use mi_core::MiMinerError;

/// Bitcoin Core RPC client (placeholder — uses getblocktemplate for solo mining against own node).
pub struct RpcClient {
    url: String,
    user: String,
    password: String,
}

impl RpcClient {
    pub fn new(url: &str, user: &str, password: &str) -> Self {
        Self {
            url: url.to_string(),
            user: user.to_string(),
            password: password.to_string(),
        }
    }

    /// Get a block template from Bitcoin Core.
    /// This is a placeholder — full implementation would parse the template
    /// and construct mining work from it.
    pub async fn get_block_template(&self) -> Result<serde_json::Value, MiMinerError> {
        // TODO: Implement actual RPC call using reqwest or similar
        tracing::warn!("Bitcoin Core RPC not yet implemented — use Stratum mode");
        Err(MiMinerError::Rpc(
            "RPC client not yet implemented".to_string(),
        ))
    }

    /// Submit a solved block to Bitcoin Core.
    pub async fn submit_block(&self, _block_hex: &str) -> Result<(), MiMinerError> {
        tracing::warn!("Bitcoin Core RPC submitblock not yet implemented");
        Err(MiMinerError::Rpc(
            "RPC client not yet implemented".to_string(),
        ))
    }
}
