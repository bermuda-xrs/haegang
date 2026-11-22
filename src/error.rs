use std::fmt;

#[derive(Debug)]
pub enum Error {
    Io(String),
    Json(String),
    /// Bad input data (unknown sender, mismatched files, empty result, bad name...).
    Invalid(String),
    NotFound(String),
    Exists(String),
    /// Non-2xx reply from a model server: (status, body).
    Http(u16, String),
    /// The model server answered but reported a failure / unexpected payload.
    Backend(String),
}

pub type Result<T> = std::result::Result<T, Error>;

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Error::Io(s) => write!(f, "io error: {s}"),
            Error::Json(s) => write!(f, "json error: {s}"),
            Error::Invalid(s) => write!(f, "invalid input: {s}"),
            Error::NotFound(s) => write!(f, "not found: {s}"),
            Error::Exists(s) => write!(f, "already exists: {s}"),
            Error::Http(c, b) => write!(f, "http {c}: {b}"),
            Error::Backend(s) => write!(f, "backend error: {s}"),
        }
    }
}

impl std::error::Error for Error {}
