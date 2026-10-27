//! Minimal GGUF metadata reader.
//!
//! Only the header is parsed: magic, version, counts and the metadata
//! key/value section. Tensor data is never touched, so this reads a few MiB off
//! the front of a multi-GB file. It answers one question: how many bytes of KV
//! cache one token of context costs, plus the trained context length.
//!
//! Every failure returns `None`, which callers read as "fall back to the
//! baseline context", never as "assume this model is cheap".

use std::collections::HashMap;
use std::fs::File;
use std::io;
use std::os::unix::fs::FileExt;
use std::path::Path;

pub const GGUF_MAGIC: u32 = 0x4655_4747; // "GGUF" little-endian
const MIN_SUPPORTED_VERSION: u32 = 2;
const MAX_SUPPORTED_VERSION: u32 = 3;

const READ_CHUNK_BYTES: usize = 1024 * 1024;
// Guards against a corrupt file steering the reader into unbounded work.
const MAX_METADATA_BYTES: u64 = 64 * 1024 * 1024;
const MAX_KV_COUNT: u64 = 4096;
const MAX_STRING_BYTES: u64 = 64 * 1024 * 1024;
const MAX_CAPTURED_ARRAY_LENGTH: u64 = 1024;
/// `Number.MAX_SAFE_INTEGER`, the JS reader's bound on 64-bit lengths.
const MAX_SAFE_INTEGER: u64 = (1 << 53) - 1;

/// GGUF value type tags.
pub mod value_type {
    pub const UINT8: u32 = 0;
    pub const INT8: u32 = 1;
    pub const UINT16: u32 = 2;
    pub const INT16: u32 = 3;
    pub const UINT32: u32 = 4;
    pub const INT32: u32 = 5;
    pub const FLOAT32: u32 = 6;
    pub const BOOL: u32 = 7;
    pub const STRING: u32 = 8;
    pub const ARRAY: u32 = 9;
    pub const UINT64: u32 = 10;
    pub const INT64: u32 = 11;
    pub const FLOAT64: u32 = 12;
}
use value_type as t;

fn scalar_bytes(kind: u32) -> Option<u64> {
    Some(match kind {
        t::UINT8 | t::INT8 | t::BOOL => 1,
        t::UINT16 | t::INT16 => 2,
        t::UINT32 | t::INT32 | t::FLOAT32 => 4,
        t::UINT64 | t::INT64 | t::FLOAT64 => 8,
        _ => return None,
    })
}

/// `attention.head_count_kv` may be one number or one per layer.
#[derive(Debug, Clone, PartialEq)]
pub enum HeadCountKv {
    Uniform(f64),
    PerLayer(Vec<f64>),
}

/// The architecture fields needed to price the KV cache.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct GgufMetadata {
    pub architecture: String,
    pub block_count: Option<f64>,
    pub context_length: Option<f64>,
    pub embedding_length: Option<f64>,
    pub head_count: Option<f64>,
    pub head_count_kv: Option<HeadCountKv>,
    pub key_length: Option<f64>,
    pub value_length: Option<f64>,
    pub has_recurrent_state: bool,
}

/// Random-access reads from the front of a file. A short read is allowed at
/// the end of the file.
pub trait GgufSource {
    fn read_at(&mut self, offset: u64, length: usize) -> io::Result<Vec<u8>>;
}

impl GgufSource for &[u8] {
    fn read_at(&mut self, offset: u64, length: usize) -> io::Result<Vec<u8>> {
        let start = (offset as usize).min(self.len());
        let end = start.saturating_add(length).min(self.len());
        Ok(self[start..end].to_vec())
    }
}

struct FileSource(File);

impl GgufSource for FileSource {
    fn read_at(&mut self, offset: u64, length: usize) -> io::Result<Vec<u8>> {
        let mut buffer = vec![0u8; length];
        let mut filled = 0;
        while filled < length {
            let n = self
                .0
                .read_at(&mut buffer[filled..], offset + filled as u64)?;
            if n == 0 {
                break;
            }
            filled += n;
        }
        buffer.truncate(filled);
        Ok(buffer)
    }
}

#[derive(Debug)]
struct Malformed;

type Parse<T> = Result<T, Malformed>;

/// Forward-only cursor over the front of the file, buffered in chunks.
struct HeaderCursor<'a, S: GgufSource + ?Sized> {
    source: &'a mut S,
    chunk: Vec<u8>,
    chunk_start: u64,
    position: u64,
}

impl<'a, S: GgufSource + ?Sized> HeaderCursor<'a, S> {
    fn new(source: &'a mut S) -> Self {
        Self {
            source,
            chunk: Vec::new(),
            chunk_start: 0,
            position: 0,
        }
    }

    fn ensure(&mut self, length: u64) -> Parse<()> {
        if self.position.saturating_add(length) > MAX_METADATA_BYTES {
            return Err(Malformed);
        }
        let within = self.position >= self.chunk_start
            && self.position + length <= self.chunk_start + self.chunk.len() as u64;
        if within {
            return Ok(());
        }
        let want = (length as usize).max(READ_CHUNK_BYTES);
        let buffer = self
            .source
            .read_at(self.position, want)
            .map_err(|_| Malformed)?;
        if (buffer.len() as u64) < length {
            return Err(Malformed);
        }
        self.chunk = buffer;
        self.chunk_start = self.position;
        Ok(())
    }

    fn take(&mut self, length: u64) -> Parse<&[u8]> {
        self.ensure(length)?;
        let start = (self.position - self.chunk_start) as usize;
        self.position += length;
        Ok(&self.chunk[start..start + length as usize])
    }

    /// Advances without reading, so huge arrays cost nothing.
    fn skip(&mut self, length: u64) -> Parse<()> {
        self.position = self.position.checked_add(length).ok_or(Malformed)?;
        Ok(())
    }

    fn read_u32(&mut self) -> Parse<u32> {
        let bytes = self.take(4)?;
        Ok(u32::from_le_bytes(bytes.try_into().unwrap()))
    }

    fn read_length(&mut self) -> Parse<u64> {
        let bytes = self.take(8)?;
        let value = u64::from_le_bytes(bytes.try_into().unwrap());
        if value > MAX_SAFE_INTEGER {
            return Err(Malformed);
        }
        Ok(value)
    }

    fn read_string(&mut self) -> Parse<String> {
        let length = self.read_length()?;
        if length > MAX_STRING_BYTES {
            return Err(Malformed);
        }
        Ok(String::from_utf8_lossy(self.take(length)?).into_owned())
    }

    fn skip_string(&mut self) -> Parse<()> {
        let length = self.read_length()?;
        if length > MAX_STRING_BYTES {
            return Err(Malformed);
        }
        self.skip(length)
    }

    fn read_scalar(&mut self, kind: u32) -> Parse<f64> {
        let size = scalar_bytes(kind).ok_or(Malformed)?;
        let b = self.take(size)?;
        Ok(match kind {
            t::UINT8 => b[0] as f64,
            t::INT8 => b[0] as i8 as f64,
            t::UINT16 => u16::from_le_bytes([b[0], b[1]]) as f64,
            t::INT16 => i16::from_le_bytes([b[0], b[1]]) as f64,
            t::UINT32 => u32::from_le_bytes(b.try_into().unwrap()) as f64,
            t::INT32 => i32::from_le_bytes(b.try_into().unwrap()) as f64,
            t::FLOAT32 => f32::from_le_bytes(b.try_into().unwrap()) as f64,
            t::BOOL => {
                if b[0] != 0 {
                    1.0
                } else {
                    0.0
                }
            }
            t::UINT64 => u64::from_le_bytes(b.try_into().unwrap()) as f64,
            t::INT64 => i64::from_le_bytes(b.try_into().unwrap()) as f64,
            t::FLOAT64 => f64::from_le_bytes(b.try_into().unwrap()),
            _ => return Err(Malformed),
        })
    }
}

#[derive(Debug, Clone)]
enum Captured {
    Number(f64),
    Text(String),
    Numbers(Vec<f64>),
}

fn read_array<S: GgufSource + ?Sized>(
    cursor: &mut HeaderCursor<'_, S>,
    capture: bool,
) -> Parse<Option<Captured>> {
    let element = cursor.read_u32()?;
    let length = cursor.read_length()?;

    if element == t::STRING {
        // Each element has its own length prefix, so it must be walked
        // (tokenizer.ggml.tokens, ~150k entries).
        for _ in 0..length {
            cursor.skip_string()?;
        }
        return Ok(None);
    }

    let size = scalar_bytes(element).ok_or(Malformed)?;
    if !capture || length > MAX_CAPTURED_ARRAY_LENGTH {
        cursor.skip(length.checked_mul(size).ok_or(Malformed)?)?;
        return Ok(None);
    }
    let mut values = Vec::with_capacity(length as usize);
    for _ in 0..length {
        values.push(cursor.read_scalar(element)?);
    }
    Ok(Some(Captured::Numbers(values)))
}

fn read_value<S: GgufSource + ?Sized>(
    cursor: &mut HeaderCursor<'_, S>,
    kind: u32,
    capture: bool,
) -> Parse<Option<Captured>> {
    match kind {
        t::STRING => {
            if capture {
                Ok(Some(Captured::Text(cursor.read_string()?)))
            } else {
                cursor.skip_string()?;
                Ok(None)
            }
        }
        t::ARRAY => read_array(cursor, capture),
        _ => {
            let size = scalar_bytes(kind).ok_or(Malformed)?;
            if capture {
                Ok(Some(Captured::Number(cursor.read_scalar(kind)?)))
            } else {
                cursor.skip(size)?;
                Ok(None)
            }
        }
    }
}

const WANTED_SUFFIXES: [&str; 7] = [
    ".block_count",
    ".context_length",
    ".embedding_length",
    ".attention.head_count",
    ".attention.head_count_kv",
    ".attention.key_length",
    ".attention.value_length",
];

fn is_wanted(key: &str) -> bool {
    key == "general.architecture" || WANTED_SUFFIXES.iter().any(|s| key.ends_with(s))
}

fn parse<S: GgufSource + ?Sized>(source: &mut S) -> Parse<Option<GgufMetadata>> {
    let mut cursor = HeaderCursor::new(source);
    if cursor.read_u32()? != GGUF_MAGIC {
        return Ok(None);
    }
    let version = cursor.read_u32()?;
    if !(MIN_SUPPORTED_VERSION..=MAX_SUPPORTED_VERSION).contains(&version) {
        return Ok(None);
    }
    cursor.read_length()?; // tensor_count, unused
    let kv_count = cursor.read_length()?;
    if kv_count > MAX_KV_COUNT {
        return Ok(None);
    }

    let mut entries: HashMap<String, Captured> = HashMap::new();
    let mut has_recurrent_state = false;
    for _ in 0..kv_count {
        let key = cursor.read_string()?;
        let kind = cursor.read_u32()?;
        if key.contains(".ssm.") {
            has_recurrent_state = true;
        }
        let wanted = is_wanted(&key);
        if let Some(value) = read_value(&mut cursor, kind, wanted)? {
            entries.insert(key, value);
        }
    }

    let architecture = match entries.get("general.architecture") {
        Some(Captured::Text(arch)) if !arch.is_empty() => arch.clone(),
        _ => return Ok(None),
    };
    let number = |suffix: &str| match entries.get(&format!("{architecture}{suffix}")) {
        Some(Captured::Number(n)) => Some(*n),
        _ => None,
    };
    let head_count_kv = match entries.get(&format!("{architecture}.attention.head_count_kv")) {
        Some(Captured::Number(n)) => Some(HeadCountKv::Uniform(*n)),
        Some(Captured::Numbers(values)) => Some(HeadCountKv::PerLayer(values.clone())),
        _ => None,
    };
    Ok(Some(GgufMetadata {
        block_count: number(".block_count"),
        context_length: number(".context_length"),
        embedding_length: number(".embedding_length"),
        head_count: number(".attention.head_count"),
        head_count_kv,
        key_length: number(".attention.key_length"),
        value_length: number(".attention.value_length"),
        has_recurrent_state,
        architecture,
    }))
}

/// Reads the metadata through `source`; `None` on anything malformed.
pub fn read_gguf_metadata<S: GgufSource + ?Sized>(source: &mut S) -> Option<GgufMetadata> {
    parse(source).ok().flatten()
}

/// Reads the metadata of a file on disk; `None` on any failure.
pub fn read_gguf_metadata_from_file(path: &Path) -> Option<GgufMetadata> {
    let file = File::open(path).ok()?;
    read_gguf_metadata(&mut FileSource(file))
}

/// Builds GGUF headers in memory for tests.
#[cfg(test)]
pub(crate) mod fixtures {
    use super::{GGUF_MAGIC, value_type as t};

    #[derive(Clone)]
    pub enum Value {
        Scalar(u32, f64),
        Text(String),
        Array(u32, Vec<f64>),
        Strings(Vec<String>),
    }

    pub struct Entry {
        pub key: String,
        pub value: Value,
    }

    pub fn u32e(key: &str, value: u32) -> Entry {
        Entry {
            key: key.into(),
            value: Value::Scalar(t::UINT32, value as f64),
        }
    }

    pub fn text(key: &str, value: &str) -> Entry {
        Entry {
            key: key.into(),
            value: Value::Text(value.into()),
        }
    }

    pub fn scalar(key: &str, kind: u32, value: f64) -> Entry {
        Entry {
            key: key.into(),
            value: Value::Scalar(kind, value),
        }
    }

    pub fn array(key: &str, kind: u32, values: Vec<f64>) -> Entry {
        Entry {
            key: key.into(),
            value: Value::Array(kind, values),
        }
    }

    pub fn strings(key: &str, values: Vec<String>) -> Entry {
        Entry {
            key: key.into(),
            value: Value::Strings(values),
        }
    }

    fn string_bytes(out: &mut Vec<u8>, value: &str) {
        out.extend((value.len() as u64).to_le_bytes());
        out.extend(value.as_bytes());
    }

    fn scalar_bytes(out: &mut Vec<u8>, kind: u32, value: f64) {
        match kind {
            t::UINT8 | t::INT8 => out.push(value as i64 as u8),
            t::UINT16 | t::INT16 => out.extend((value as i64 as u16).to_le_bytes()),
            t::UINT32 | t::INT32 => out.extend((value as i64 as u32).to_le_bytes()),
            t::FLOAT32 => out.extend((value as f32).to_le_bytes()),
            t::BOOL => out.push(u8::from(value != 0.0)),
            t::UINT64 | t::INT64 => out.extend((value as i64 as u64).to_le_bytes()),
            t::FLOAT64 => out.extend(value.to_le_bytes()),
            _ => panic!("unsupported scalar type {kind}"),
        }
    }

    pub struct Options {
        pub version: u32,
        pub magic: u32,
        pub kv_count: Option<u64>,
    }

    impl Default for Options {
        fn default() -> Self {
            Self {
                version: 3,
                magic: GGUF_MAGIC,
                kv_count: None,
            }
        }
    }

    pub fn build(entries: &[Entry], options: Options) -> Vec<u8> {
        let mut out = Vec::new();
        out.extend(options.magic.to_le_bytes());
        out.extend(options.version.to_le_bytes());
        out.extend(0u64.to_le_bytes());
        out.extend(
            options
                .kv_count
                .unwrap_or(entries.len() as u64)
                .to_le_bytes(),
        );
        for entry in entries {
            string_bytes(&mut out, &entry.key);
            match &entry.value {
                Value::Scalar(kind, value) => {
                    out.extend(kind.to_le_bytes());
                    scalar_bytes(&mut out, *kind, *value);
                }
                Value::Text(value) => {
                    out.extend(t::STRING.to_le_bytes());
                    string_bytes(&mut out, value);
                }
                Value::Array(kind, values) => {
                    out.extend(t::ARRAY.to_le_bytes());
                    out.extend(kind.to_le_bytes());
                    out.extend((values.len() as u64).to_le_bytes());
                    for value in values {
                        scalar_bytes(&mut out, *kind, *value);
                    }
                }
                Value::Strings(values) => {
                    out.extend(t::ARRAY.to_le_bytes());
                    out.extend(t::STRING.to_le_bytes());
                    out.extend((values.len() as u64).to_le_bytes());
                    for value in values {
                        string_bytes(&mut out, value);
                    }
                }
            }
        }
        out
    }

    /// Llama 3.2 3B, as its GGUF declares itself.
    pub fn llama_3_2_3b() -> Vec<Entry> {
        vec![
            text("general.architecture", "llama"),
            u32e("llama.block_count", 28),
            u32e("llama.context_length", 131072),
            u32e("llama.embedding_length", 3072),
            u32e("llama.attention.head_count", 24),
            u32e("llama.attention.head_count_kv", 8),
            u32e("llama.attention.key_length", 128),
            u32e("llama.attention.value_length", 128),
        ]
    }
}

#[cfg(test)]
mod tests {
    use super::fixtures::*;
    use super::*;

    fn read(entries: &[Entry]) -> Option<GgufMetadata> {
        let bytes = build(entries, Options::default());
        read_gguf_metadata(&mut bytes.as_slice())
    }

    #[test]
    fn reads_the_architecture_fields_that_determine_kv_cost() {
        let m = read(&llama_3_2_3b()).unwrap();
        assert_eq!(m.architecture, "llama");
        assert_eq!(m.block_count, Some(28.0));
        assert_eq!(m.head_count_kv, Some(HeadCountKv::Uniform(8.0)));
        assert_eq!(m.key_length, Some(128.0));
        assert_eq!(m.value_length, Some(128.0));
        assert_eq!(m.embedding_length, Some(3072.0));
        assert_eq!(m.head_count, Some(24.0));
        assert_eq!(m.context_length, Some(131072.0));
        assert!(!m.has_recurrent_state);
    }

    #[test]
    fn reads_the_models_own_context_length() {
        let m = read(&[
            text("general.architecture", "qwen2"),
            u32e("qwen2.block_count", 28),
            u32e("qwen2.context_length", 131072),
            u32e("qwen2.attention.head_count_kv", 4),
            u32e("qwen2.attention.key_length", 128),
            u32e("qwen2.attention.value_length", 128),
        ])
        .unwrap();
        assert_eq!(m.context_length, Some(131072.0));
    }

    #[test]
    fn reads_a_per_layer_head_count_kv_array() {
        let m = read(&[
            text("general.architecture", "gemma3"),
            u32e("gemma3.block_count", 4),
            array(
                "gemma3.attention.head_count_kv",
                t::UINT32,
                vec![2.0, 8.0, 2.0, 2.0],
            ),
            u32e("gemma3.attention.key_length", 256),
            u32e("gemma3.attention.value_length", 256),
        ])
        .unwrap();
        assert_eq!(
            m.head_count_kv,
            Some(HeadCountKv::PerLayer(vec![2.0, 8.0, 2.0, 2.0]))
        );
    }

    #[test]
    fn flags_recurrent_state() {
        let m = read(&[
            text("general.architecture", "qwen35"),
            u32e("qwen35.block_count", 33),
            u32e("qwen35.attention.head_count_kv", 4),
            u32e("qwen35.attention.key_length", 256),
            u32e("qwen35.attention.value_length", 256),
            u32e("qwen35.ssm.state_size", 128),
        ])
        .unwrap();
        assert!(m.has_recurrent_state);
    }

    #[test]
    fn walks_past_every_scalar_type_without_losing_its_place() {
        let mut entries = vec![
            text("general.architecture", "llama"),
            scalar("noise.u8", t::UINT8, 7.0),
            scalar("noise.i8", t::INT8, 7.0),
            scalar("noise.u16", t::UINT16, 7.0),
            scalar("noise.i16", t::INT16, 7.0),
            scalar("noise.u32", t::UINT32, 7.0),
            scalar("noise.i32", t::INT32, 7.0),
            scalar("noise.f32", t::FLOAT32, 7.5),
            scalar("noise.bool", t::BOOL, 1.0),
            text("noise.string", "a longer value"),
            scalar("noise.u64", t::UINT64, 7.0),
            scalar("noise.i64", t::INT64, 7.0),
            scalar("noise.f64", t::FLOAT64, 7.5),
        ];
        entries.extend(llama_3_2_3b().into_iter().skip(1));
        let m = read(&entries).unwrap();
        assert_eq!(m.block_count, Some(28.0));
        assert_eq!(m.head_count_kv, Some(HeadCountKv::Uniform(8.0)));
    }

    #[test]
    fn skips_the_tokenizer_arrays() {
        let tokens: Vec<String> = (0..5000).map(|i| format!("token_{i}")).collect();
        let scores = vec![0.0; tokens.len()];
        let mut entries = vec![
            text("general.architecture", "llama"),
            strings("tokenizer.ggml.tokens", tokens),
            array("tokenizer.ggml.scores", t::FLOAT32, scores),
        ];
        entries.extend(llama_3_2_3b().into_iter().skip(1));
        let m = read(&entries).unwrap();
        assert_eq!(m.block_count, Some(28.0));
        assert_eq!(m.head_count_kv, Some(HeadCountKv::Uniform(8.0)));
    }

    #[test]
    fn returns_none_instead_of_failing_on_anything_malformed() {
        let good = build(&llama_3_2_3b(), Options::default());
        let mut clobbered = build(&[text("general.architecture", "llama")], Options::default());
        let at = 4 + 4 + 8 + 8 + 8 + "general.architecture".len();
        clobbered[at..at + 4].fill(0x7f);

        let cases: Vec<(&str, Vec<u8>)> = vec![
            (
                "bad magic",
                build(
                    &llama_3_2_3b(),
                    Options {
                        magic: 0xdead_beef,
                        ..Options::default()
                    },
                ),
            ),
            (
                "version 1",
                build(
                    &llama_3_2_3b(),
                    Options {
                        version: 1,
                        ..Options::default()
                    },
                ),
            ),
            ("truncated mid-header", good[..60].to_vec()),
            ("empty file", Vec::new()),
            ("header only", good[..8].to_vec()),
            (
                "implausible kv count",
                build(
                    &llama_3_2_3b(),
                    Options {
                        kv_count: Some(99999),
                        ..Options::default()
                    },
                ),
            ),
            ("unknown value type", clobbered),
        ];
        for (label, bytes) in cases {
            assert_eq!(read_gguf_metadata(&mut bytes.as_slice()), None, "{label}");
        }
    }

    struct Failing;
    impl GgufSource for Failing {
        fn read_at(&mut self, _: u64, _: usize) -> io::Result<Vec<u8>> {
            Err(io::Error::other("EIO"))
        }
    }

    #[test]
    fn returns_none_when_a_read_fails() {
        assert_eq!(read_gguf_metadata(&mut Failing), None);
    }

    #[test]
    fn returns_none_when_the_architecture_keys_are_absent() {
        assert_eq!(
            read(&[
                text("general.name", "some model"),
                u32e("general.file_type", 15),
            ]),
            None
        );
    }

    #[test]
    fn reads_a_file_on_disk() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("model.gguf");
        let mut bytes = build(&llama_3_2_3b(), Options::default());
        bytes.extend(vec![1u8; 3 * 1024 * 1024]);
        std::fs::write(&path, bytes).unwrap();
        assert_eq!(
            read_gguf_metadata_from_file(&path).unwrap().block_count,
            Some(28.0)
        );
        assert_eq!(
            read_gguf_metadata_from_file(&dir.path().join("missing")),
            None
        );
    }
}
