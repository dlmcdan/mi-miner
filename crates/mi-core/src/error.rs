use thiserror::Error;

#[derive(Error, Debug)]
pub enum MiMinerError {
    #[error("Configuration error: {0}")]
    Config(String),

    #[error("Mining error: {0}")]
    Mining(String),

    #[error("GPU error: {0}")]
    Gpu(String),

    #[error("Network error: {0}")]
    Network(String),

    #[error("Stratum error: {0}")]
    Stratum(String),

    #[error("RPC error: {0}")]
    Rpc(String),

    #[error("Web server error: {0}")]
    Web(String),

    #[error("IO error: {0}")]
    Io(#[from] std::io::Error),
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_error_display() {
        let e = MiMinerError::Config("bad value".to_string());
        assert_eq!(e.to_string(), "Configuration error: bad value");
    }

    #[test]
    fn test_error_variants() {
        let cases = vec![
            (MiMinerError::Mining("x".into()), "Mining error: x"),
            (MiMinerError::Gpu("y".into()), "GPU error: y"),
            (MiMinerError::Network("z".into()), "Network error: z"),
            (MiMinerError::Stratum("s".into()), "Stratum error: s"),
            (MiMinerError::Rpc("r".into()), "RPC error: r"),
            (MiMinerError::Web("w".into()), "Web server error: w"),
        ];
        for (err, expected) in cases {
            assert_eq!(err.to_string(), expected);
        }
    }

    #[test]
    fn test_io_error_conversion() {
        let io_err = std::io::Error::new(std::io::ErrorKind::NotFound, "gone");
        let err: MiMinerError = io_err.into();
        assert!(err.to_string().contains("gone"));
    }
}
