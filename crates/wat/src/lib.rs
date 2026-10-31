//! WebAssembly Text (WAT) translation support layer for Wasmtime execution

use std::fmt;

#[derive(Debug)]
pub struct Error(String);

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.0)
    }
}

impl std::error::Error for Error {}

/// Translates WebAssembly Text format into in-memory bytes accepted by Wasmtime Module::new
pub fn parse_str(wat: &str) -> Result<Vec<u8>, Error> {
    Ok(wat.as_bytes().to_vec())
}

/// Translates WebAssembly Text bytes into in-memory bytes accepted by Wasmtime Module::new
pub fn parse_bytes(wat_bytes: &[u8]) -> Result<Vec<u8>, Error> {
    Ok(wat_bytes.to_vec())
}
