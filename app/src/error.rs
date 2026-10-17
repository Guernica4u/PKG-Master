use std::fmt;

/// Every expected failure. The variants mirror `pkgmaster/errors.py` so the
/// app can tell "you need a passcode" apart from "this file is corrupt".
#[derive(Debug)]
pub enum Error {
    /// The input ended before a structure's declared size.
    Truncated(String),
    /// The input has no PS4 CNT or PS5 FIH signature.
    NotAPackage(String),
    /// A structure parsed but its contents are not self-consistent.
    Corrupt(String),
    /// The image uses a format feature this version does not implement.
    Unsupported(String),
    /// A passcode or EKPFS key is required to continue.
    KeyRequired(String),
    /// A key was supplied, but the resulting image is not readable.
    WrongKey(String),
    /// A content file could not be converted to the requested format.
    Conversion(String),
    Io(std::io::Error),
}

pub type Result<T> = std::result::Result<T, Error>;

impl Error {
    /// Process exit code, matching the Python CLI.
    pub fn exit_code(&self) -> i32 {
        match self {
            Error::KeyRequired(_) | Error::WrongKey(_) => 3,
            Error::Unsupported(_) => 4,
            _ => 2,
        }
    }

    pub fn needs_key(&self) -> bool {
        matches!(self, Error::KeyRequired(_) | Error::WrongKey(_))
    }
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Error::Truncated(m)
            | Error::NotAPackage(m)
            | Error::Corrupt(m)
            | Error::Unsupported(m)
            | Error::KeyRequired(m)
            | Error::WrongKey(m)
            | Error::Conversion(m) => f.write_str(m),
            Error::Io(e) => write!(f, "{e}"),
        }
    }
}

impl std::error::Error for Error {}

impl From<std::io::Error> for Error {
    fn from(e: std::io::Error) -> Self {
        Error::Io(e)
    }
}
