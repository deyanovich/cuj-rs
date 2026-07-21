//! cuj-level error conditions (SPEC §13) plus surfaced substrate
//! errors, each mapped to a stable exit code (SPEC §11.3).

use std::fmt;

#[derive(Debug)]
pub enum Error {
    /// A jot reference, profile, attachment filename, bookmark, or
    /// todo match did not resolve.
    NotFound(String),
    /// The reference resolves to a tombstoned jot.
    Deleted(String),
    /// An exclusive/required constraint check failed.
    ConstraintViolation(String),
    /// A content-rewrite operation hit a frozen profile.
    Frozen(String),
    /// An index-backed query could not be served at the requested
    /// state.
    Stale(String),
    /// Head CAS retries exhausted.
    Conflict,
    /// The vault's cuj/vault protocol is not supported.
    Unsupported(String),
    /// Command-line usage error.
    Usage(String),
    /// Substrate error (metatheca / extensions / jots), surfaced
    /// distinguishably.
    Substrate(String),
    Io(std::io::Error),
}

impl Error {
    pub fn code(&self) -> &'static str {
        match self {
            Error::NotFound(_) => "NotFound",
            Error::Deleted(_) => "Deleted",
            Error::ConstraintViolation(_) => "ConstraintViolation",
            Error::Frozen(_) => "Frozen",
            Error::Stale(_) => "Stale",
            Error::Conflict => "Conflict",
            Error::Unsupported(_) => "Unsupported",
            Error::Usage(_) => "Usage",
            Error::Substrate(_) => "Substrate",
            Error::Io(_) => "Io",
        }
    }

    /// Documented exit-code assignment (SPEC §11.3).
    pub fn exit_code(&self) -> i32 {
        match self {
            Error::Usage(_) => 2,
            Error::NotFound(_) => 3,
            Error::Deleted(_) => 4,
            Error::ConstraintViolation(_) => 5,
            Error::Frozen(_) => 6,
            Error::Stale(_) => 7,
            Error::Conflict => 8,
            Error::Unsupported(_) => 9,
            Error::Substrate(_) | Error::Io(_) => 1,
        }
    }
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Error::NotFound(m) => write!(f, "not found: {m}"),
            Error::Deleted(m) => write!(f, "deleted: {m}"),
            Error::ConstraintViolation(m) => write!(f, "constraint violation: {m}"),
            Error::Frozen(m) => write!(f, "frozen: {m}"),
            Error::Stale(m) => write!(f, "stale index: {m}"),
            Error::Conflict => write!(f, "conflict: concurrent writer, retries exhausted"),
            Error::Unsupported(m) => write!(f, "unsupported: {m}"),
            Error::Usage(m) => write!(f, "usage: {m}"),
            Error::Substrate(m) => write!(f, "substrate: {m}"),
            Error::Io(e) => write!(f, "io: {e}"),
        }
    }
}

impl std::error::Error for Error {}

impl From<std::io::Error> for Error {
    fn from(e: std::io::Error) -> Self {
        Error::Io(e)
    }
}

impl From<metatheca::Error> for Error {
    fn from(e: metatheca::Error) -> Self {
        match e {
            metatheca::Error::Conflict => Error::Conflict,
            metatheca::Error::NotFound(m) => Error::NotFound(m),
            other => Error::Substrate(other.to_string()),
        }
    }
}

impl From<taxopsis::Error> for Error {
    fn from(e: taxopsis::Error) -> Self {
        Error::Substrate(format!("taxopsis: {e}"))
    }
}

impl From<logopsis::Error> for Error {
    fn from(e: logopsis::Error) -> Self {
        Error::Substrate(format!("logopsis: {e}"))
    }
}

impl From<semopsis::Error> for Error {
    fn from(e: semopsis::Error) -> Self {
        Error::Substrate(format!("semopsis: {e}"))
    }
}

impl From<zetetes::Error> for Error {
    fn from(e: zetetes::Error) -> Self {
        Error::Substrate(format!("zetetes: {e}"))
    }
}

pub type Result<T> = std::result::Result<T, Error>;
