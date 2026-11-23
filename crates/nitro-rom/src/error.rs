use thiserror::Error;

pub type Result<T> = std::result::Result<T, RomError>;

#[derive(Debug, Error)]
pub enum RomError {
    #[error("I/O error: {0}")]
    Io(#[from] std::io::Error),

    #[error("ROM image too small ({actual} bytes; minimum {minimum} bytes required)")]
    TooSmall { actual: usize, minimum: usize },

    #[error("out-of-bounds ROM slice `{context}` (offset 0x{offset:X}, size 0x{size:X}, rom_len 0x{rom_len:X})")]
    SliceOutOfBounds {
        context: &'static str,
        offset: u64,
        size: u64,
        rom_len: u64,
    },

    #[error("invalid Backward LZ77 (BLZ) stream: {0}")]
    InvalidBlz(&'static str),

    #[error("invalid NitroFS directory table: {0}")]
    InvalidNitroFs(String),

    #[error("unsafe relative path rejected in NitroFS: `{0}`")]
    UnsafePath(String),
}
