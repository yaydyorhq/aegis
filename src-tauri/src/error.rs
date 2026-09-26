use thiserror::Error;

#[derive(Debug, Error)]
pub enum AppError {
    #[error("db: {0}")]
    Db(#[from] rusqlite::Error),
    #[error("crypto: {0}")]
    Crypto(String),
    #[error("vault locked")]
    VaultLocked,
    #[error("vault already initialized")]
    VaultExists,
    #[error("invalid passphrase")]
    BadPassphrase,
    #[error("not found: {0}")]
    NotFound(String),
    #[error("invalid input: {0}")]
    Invalid(String),
    #[error("rpc: {0}")]
    Rpc(String),
    #[error("io: {0}")]
    Io(#[from] std::io::Error),
    #[error("{0}")]
    Other(String),
}

impl serde::Serialize for AppError {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        serializer.serialize_str(&self.to_string())
    }
}

pub type AppResult<T> = Result<T, AppError>;
