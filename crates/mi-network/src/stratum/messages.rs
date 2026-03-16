use serde::{Deserialize, Serialize};

/// JSON-RPC request
#[derive(Debug, Serialize)]
pub struct JsonRpcRequest {
    pub id: u64,
    pub method: String,
    pub params: Vec<serde_json::Value>,
}

/// JSON-RPC response
#[derive(Debug, Deserialize)]
pub struct JsonRpcResponse {
    pub id: Option<u64>,
    pub result: Option<serde_json::Value>,
    pub error: Option<serde_json::Value>,
    pub method: Option<String>,
    pub params: Option<serde_json::Value>,
}

#[derive(Debug)]
pub struct JsonRpcError {
    pub code: i64,
    pub message: String,
}

impl JsonRpcError {
    /// Parse an error from a serde_json::Value.
    /// Handles both object {"code":N,"message":"..."} and array [N,"msg",""] formats.
    pub fn from_value(v: &serde_json::Value) -> Option<Self> {
        if let Some(obj) = v.as_object() {
            Some(Self {
                code: obj.get("code").and_then(|c| c.as_i64()).unwrap_or(0),
                message: obj.get("message").and_then(|m| m.as_str()).unwrap_or("").to_string(),
            })
        } else if let Some(arr) = v.as_array() {
            Some(Self {
                code: arr.first().and_then(|c| c.as_i64()).unwrap_or(0),
                message: arr.get(1).and_then(|m| m.as_str()).unwrap_or("").to_string(),
            })
        } else {
            None
        }
    }
}

/// Parsed mining.notify parameters
#[derive(Debug, Clone)]
pub struct MiningNotify {
    pub job_id: String,
    pub prev_hash: String,
    pub coinbase_1: String,
    pub coinbase_2: String,
    pub merkle_branches: Vec<String>,
    pub version: String,
    pub nbits: String,
    pub ntime: String,
    pub clean_jobs: bool,
}

impl MiningNotify {
    pub fn from_params(params: &[serde_json::Value]) -> Result<Self, String> {
        if params.len() < 9 {
            return Err(format!(
                "mining.notify requires 9 params, got {}",
                params.len()
            ));
        }

        let merkle_branches = params[4]
            .as_array()
            .ok_or("merkle_branch must be array")?
            .iter()
            .map(|v| v.as_str().unwrap_or("").to_string())
            .collect();

        Ok(Self {
            job_id: params[0].as_str().unwrap_or("").to_string(),
            prev_hash: params[1].as_str().unwrap_or("").to_string(),
            coinbase_1: params[2].as_str().unwrap_or("").to_string(),
            coinbase_2: params[3].as_str().unwrap_or("").to_string(),
            merkle_branches,
            version: params[5].as_str().unwrap_or("").to_string(),
            nbits: params[6].as_str().unwrap_or("").to_string(),
            ntime: params[7].as_str().unwrap_or("").to_string(),
            clean_jobs: params[8].as_bool().unwrap_or(false),
        })
    }
}

/// mining.subscribe result
#[derive(Debug)]
pub struct SubscribeResult {
    pub extranonce1: String,
    pub extranonce2_size: usize,
}

impl SubscribeResult {
    pub fn from_result(result: &serde_json::Value) -> Result<Self, String> {
        let arr = result.as_array().ok_or("subscribe result must be array")?;
        if arr.len() < 3 {
            return Err(format!(
                "subscribe result needs 3 elements, got {}",
                arr.len()
            ));
        }

        Ok(Self {
            extranonce1: arr[1].as_str().unwrap_or("").to_string(),
            extranonce2_size: arr[2].as_u64().unwrap_or(4) as usize,
        })
    }
}

/// Build a mining.submit request
pub fn build_submit(
    id: u64,
    worker: &str,
    job_id: &str,
    extranonce2: &str,
    ntime: &str,
    nonce: &str,
) -> JsonRpcRequest {
    JsonRpcRequest {
        id,
        method: "mining.submit".to_string(),
        params: vec![
            serde_json::Value::String(worker.to_string()),
            serde_json::Value::String(job_id.to_string()),
            serde_json::Value::String(extranonce2.to_string()),
            serde_json::Value::String(ntime.to_string()),
            serde_json::Value::String(nonce.to_string()),
        ],
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_mining_notify_from_valid_params() {
        let params: Vec<serde_json::Value> = vec![
            serde_json::json!("job123"),
            serde_json::json!("00000000000000000000000000000000aaaaaaaaaaaaaaaabbbbbbbbbbbbbbbb"),
            serde_json::json!("01000000"),
            serde_json::json!("ffffffff"),
            serde_json::json!(["abcd1234abcd1234abcd1234abcd1234abcd1234abcd1234abcd1234abcd1234"]),
            serde_json::json!("20000000"),
            serde_json::json!("1d00ffff"),
            serde_json::json!("65a5e300"),
            serde_json::json!(true),
        ];

        let notify = MiningNotify::from_params(&params).unwrap();
        assert_eq!(notify.job_id, "job123");
        assert_eq!(notify.version, "20000000");
        assert_eq!(notify.nbits, "1d00ffff");
        assert!(notify.clean_jobs);
        assert_eq!(notify.merkle_branches.len(), 1);
    }

    #[test]
    fn test_mining_notify_too_few_params() {
        let params: Vec<serde_json::Value> = vec![serde_json::json!("job1")];
        let result = MiningNotify::from_params(&params);
        assert!(result.is_err());
        assert!(result.unwrap_err().contains("requires 9 params"));
    }

    #[test]
    fn test_mining_notify_empty_merkle_branches() {
        let params: Vec<serde_json::Value> = vec![
            serde_json::json!("j"), serde_json::json!("ph"),
            serde_json::json!("c1"), serde_json::json!("c2"),
            serde_json::json!([]), // empty branches
            serde_json::json!("v"), serde_json::json!("b"),
            serde_json::json!("t"), serde_json::json!(false),
        ];
        let notify = MiningNotify::from_params(&params).unwrap();
        assert!(notify.merkle_branches.is_empty());
        assert!(!notify.clean_jobs);
    }

    #[test]
    fn test_subscribe_result_valid() {
        let json = serde_json::json!([
            [["mining.set_difficulty", "1"], ["mining.notify", "1"]],
            "aabbccdd",
            4
        ]);
        let result = SubscribeResult::from_result(&json).unwrap();
        assert_eq!(result.extranonce1, "aabbccdd");
        assert_eq!(result.extranonce2_size, 4);
    }

    #[test]
    fn test_subscribe_result_too_few_elements() {
        let json = serde_json::json!(["only_one"]);
        let result = SubscribeResult::from_result(&json);
        assert!(result.is_err());
    }

    #[test]
    fn test_subscribe_result_not_array() {
        let json = serde_json::json!("not_array");
        let result = SubscribeResult::from_result(&json);
        assert!(result.is_err());
    }

    #[test]
    fn test_build_submit() {
        let req = build_submit(42, "worker1", "job1", "00000001", "65a5e300", "deadbeef");
        assert_eq!(req.id, 42);
        assert_eq!(req.method, "mining.submit");
        assert_eq!(req.params.len(), 5);
        assert_eq!(req.params[0].as_str().unwrap(), "worker1");
        assert_eq!(req.params[4].as_str().unwrap(), "deadbeef");
    }

    #[test]
    fn test_json_rpc_request_serializes() {
        let req = JsonRpcRequest {
            id: 1,
            method: "mining.subscribe".to_string(),
            params: vec![serde_json::json!("mi-miner/0.1")],
        };
        let json = serde_json::to_string(&req).unwrap();
        assert!(json.contains("\"method\":\"mining.subscribe\""));
        assert!(json.contains("\"id\":1"));
    }

    #[test]
    fn test_json_rpc_response_deserializes() {
        let json = r#"{"id":1,"result":true,"error":null}"#;
        let resp: JsonRpcResponse = serde_json::from_str(json).unwrap();
        assert_eq!(resp.id, Some(1));
        assert_eq!(resp.result.unwrap().as_bool(), Some(true));
        assert!(resp.error.is_none());
    }

    #[test]
    fn test_json_rpc_response_with_error_object() {
        let json = r#"{"id":2,"result":null,"error":{"code":-1,"message":"bad"}}"#;
        let resp: JsonRpcResponse = serde_json::from_str(json).unwrap();
        let err = JsonRpcError::from_value(&resp.error.unwrap()).unwrap();
        assert_eq!(err.code, -1);
        assert_eq!(err.message, "bad");
    }

    #[test]
    fn test_json_rpc_response_with_error_array() {
        // CKPool sends errors as arrays: [code, "message", "traceback"]
        let json = r#"{"id":3,"result":null,"error":[22,"Duplicate share",""]}"#;
        let resp: JsonRpcResponse = serde_json::from_str(json).unwrap();
        let err = JsonRpcError::from_value(&resp.error.unwrap()).unwrap();
        assert_eq!(err.code, 22);
        assert_eq!(err.message, "Duplicate share");
    }

    #[test]
    fn test_json_rpc_error_null_is_none() {
        let json = r#"{"id":1,"result":true,"error":null}"#;
        let resp: JsonRpcResponse = serde_json::from_str(json).unwrap();
        // error is null — should be Some(Value::Null) but we check is_null()
        assert!(resp.error.is_none() || resp.error.as_ref().unwrap().is_null());
    }

    #[test]
    fn test_json_rpc_notification_no_id() {
        let json = r#"{"id":null,"method":"mining.notify","params":["a","b","c","d",[],"e","f","g",true]}"#;
        let resp: JsonRpcResponse = serde_json::from_str(json).unwrap();
        assert!(resp.id.is_none());
        assert_eq!(resp.method.as_deref(), Some("mining.notify"));
    }
}
