use std::path::PathBuf;
use std::io::Error as IoError;
use std::num::ParseIntError;
use std::net::AddrParseError;
use config::ConfigError;
use tera::Error as TeraError;
use hyper::error::Error as HyperError;

/// The error type used by this crate.
#[derive(Debug, Fail)]
pub enum Error {
    #[fail(display = "IO error")]
    Io(#[cause] IoError),

    #[fail(display = "Int Parse error")]
    IntParse(#[cause] ParseIntError),

    #[fail(display = "Address Parse error")]
    AddrParse(#[cause] AddrParseError),

    #[fail(display = "Config error")]
    Config(#[cause] ConfigError),

    #[fail(display = "Template error: {}", _0)]
    Template(String),
    // Template(#[cause] ::tera::Error),

    #[fail(display = "Server error")]
    Hyper(#[cause] HyperError),

    #[fail(display = "Argument error: {}", _0)]
    Argument(String),

    #[fail(display = "blog root directory `{:?}` already exists", _0)]
    RootDirExisted(PathBuf),

    #[fail(display = "blog theme `{}` not found", _0)]
    ThemeNotFound(String),

    #[fail(display = "post `{:?}` head part format error", _0)]
    PostHead(PathBuf),

    #[fail(display = "post `{:?}` has not body part", _0)]
    PostNoBody(PathBuf),
}

impl From<IoError> for Error {
     fn from(err: IoError) -> Error {
         Error::Io(err)
     }
}

impl From<ParseIntError> for Error {
     fn from(err: ParseIntError) -> Error {
         Error::IntParse(err)
     }
}

impl From<AddrParseError> for Error {
     fn from(err: AddrParseError) -> Error {
         Error::AddrParse(err)
     }
}

impl From<ConfigError> for Error {
     fn from(err: ConfigError) -> Error {
         Error::Config(err)
     }
}

impl From<TeraError> for Error {
     fn from(err: TeraError) -> Error {
         Error::Template(err.description().to_string())
     }
}

impl From<HyperError> for Error {
     fn from(err: HyperError) -> Error {
         Error::Hyper(err)
     }
}

/// A specialized `Result` type where the error is hard-wired to [`Error`].
///
/// [`Error`]: enum.Error.html
pub type Result<T> = ::std::result::Result<T, Error>;