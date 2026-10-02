use thiserror::Error;

/// Failure of an API call. `status() == 0` means the server could not be reached.
#[derive(Debug, Clone, Error)]
pub enum ApiError {
    /// 401 on an authenticated call. The client has already dropped its token and caches.
    #[error("not signed in")]
    Unauthorized,
    #[error("{message}")]
    Http { status: u16, message: String },
    #[error("Can't reach the server: {0}")]
    Network(String),
    #[error("unexpected response: {0}")]
    Decode(String),
}

impl ApiError {
    pub fn status(&self) -> u16 {
        match self {
            ApiError::Unauthorized => 401,
            ApiError::Http { status, .. } => *status,
            ApiError::Network(_) | ApiError::Decode(_) => 0,
        }
    }
    pub fn message(&self) -> String {
        self.to_string()
    }
}
