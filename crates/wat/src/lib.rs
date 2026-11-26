//! WebAssembly Text (WAT) translation support layer for Wasmtime execution

use std::fmt;

#[derive(Debug)]
pub struct Error(String);

impl Error {
    pub fn set_path<P: AsRef<std::path::Path>>(&mut self, path: P) {
        self.0.push_str(&format!(" (in {})", path.as_ref().display()));
    }
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.0)
    }
}

impl std::error::Error for Error {}

/// Translates WebAssembly Text format into in-memory bytes accepted by Wasmtime Module::new
pub fn parse_str(wat: &str) -> Result<Vec<u8>, Error> {
    let buf = wast::parser::ParseBuffer::new(wat).map_err(|e| Error(e.to_string()))?;
    let mut wat_parsed: wast::Wat = wast::parser::parse(&buf).map_err(|e| Error(e.to_string()))?;
    let binary = wat_parsed.encode().map_err(|e| Error(e.to_string()))?;
    Ok(binary)
}

/// Translates WebAssembly Text bytes into in-memory bytes accepted by Wasmtime Module::new
pub fn parse_bytes(wat_bytes: &[u8]) -> Result<Vec<u8>, Error> {
    if wat_bytes.starts_with(b"\0asm") {
        return Ok(wat_bytes.to_vec());
    }
    let s = std::str::from_utf8(wat_bytes).map_err(|e| Error(e.to_string()))?;
    parse_str(s)
}

/// Translates a file containing WebAssembly Text format or binary bytes into in-memory bytes
pub fn parse_file<P: AsRef<std::path::Path>>(path: P) -> Result<Vec<u8>, Error> {
    let bytes = std::fs::read(path.as_ref()).map_err(|e| Error(e.to_string()))?;
    parse_bytes(&bytes)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_str_module() {
        let wasm = parse_str("(module (func))").expect("WAT compilation should succeed");
        assert!(wasm.len() >= 8);
        assert_eq!(&wasm[0..8], &[0x00, 0x61, 0x73, 0x6d, 0x01, 0x00, 0x00, 0x00]);
    }

    #[test]
    fn test_parse_bytes_wat_and_binary() {
        let wat_src = b"(module (memory 1))";
        let wasm = parse_bytes(wat_src).expect("WAT bytes should compile");
        assert_eq!(&wasm[0..4], &[0x00, 0x61, 0x73, 0x6d]);

        // Passing binary wasm should return the same bytes
        let wasm_clone = parse_bytes(&wasm).expect("binary bytes pass-through");
        assert_eq!(wasm, wasm_clone);
    }

    #[test]
    fn test_invalid_wat() {
        let res = parse_str("(invalid wat syntax !!!)");
        assert!(res.is_err());
    }
}
