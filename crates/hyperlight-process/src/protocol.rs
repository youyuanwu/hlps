//! Owned IPC values, deliberately separate from the pointer-bearing C ABI.

use std::fmt;
use std::io::{Read, Write};

use anyhow::{Context, Result, ensure};
use vercode::{VerCodable, Vercode};

pub const PROTOCOL_VERSION: u32 = 1;
pub const MAX_FRAME_SIZE: usize = 16 * 1024 * 1024;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Vercode)]
pub enum ValueType {
    Int,
    UInt,
    Long,
    ULong,
    Float,
    Double,
    String,
    Bool,
    Void,
    Bytes,
    ByteChunks,
}

#[derive(Clone, Debug, PartialEq, Vercode)]
pub enum Value {
    Int(i32),
    UInt(u32),
    Long(i64),
    ULong(u64),
    Float(f32),
    Double(f64),
    String(String),
    Bool(bool),
    Void,
    Bytes(Vec<u8>),
    ByteChunks(Vec<Vec<u8>>),
}

impl Value {
    pub fn value_type(&self) -> ValueType {
        match self {
            Self::Int(_) => ValueType::Int,
            Self::UInt(_) => ValueType::UInt,
            Self::Long(_) => ValueType::Long,
            Self::ULong(_) => ValueType::ULong,
            Self::Float(_) => ValueType::Float,
            Self::Double(_) => ValueType::Double,
            Self::String(_) => ValueType::String,
            Self::Bool(_) => ValueType::Bool,
            Self::Void => ValueType::Void,
            Self::Bytes(_) => ValueType::Bytes,
            Self::ByteChunks(_) => ValueType::ByteChunks,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Vercode)]
pub struct FunctionSignature {
    pub name: String,
    pub parameters: Vec<ValueType>,
    pub return_type: ValueType,
}

#[derive(Clone, Debug, PartialEq, Vercode)]
pub struct FunctionCall {
    pub function: String,
    pub arguments: Vec<Value>,
    pub return_type: ValueType,
}

#[derive(Clone, Debug, PartialEq, Eq, Vercode)]
pub struct CallError {
    pub code: i32,
    pub message: String,
}

impl fmt::Display for CallError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "guest error {}: {}", self.code, self.message)
    }
}

impl std::error::Error for CallError {}

#[derive(Clone, Debug, PartialEq, Vercode)]
pub enum CallResult {
    Value(Value),
    Error(CallError),
}

#[derive(Clone, Debug, PartialEq, Vercode)]
pub enum Message {
    Hello {
        protocol_version: u32,
        functions: Vec<FunctionSignature>,
    },
    Call {
        id: u64,
        call: FunctionCall,
    },
    Result {
        id: u64,
        result: CallResult,
    },
    HostCall {
        id: u64,
        call: FunctionCall,
    },
    HostResult {
        id: u64,
        result: CallResult,
    },
    Shutdown,
}

pub fn write_message(writer: &mut impl Write, message: &Message) -> Result<()> {
    let size = vercode::size(message).context("sizing IPC message")?;
    ensure!(
        size <= MAX_FRAME_SIZE,
        "IPC frame exceeds {MAX_FRAME_SIZE} bytes"
    );
    let mut buffer = vec![0; size];
    let encoded = vercode::serialize(message, &mut buffer).context("encoding IPC message")?;
    writer.write_all(encoded).context("writing IPC frame")?;
    writer.flush().context("flushing IPC frame")
}

pub fn read_message(reader: &mut impl Read) -> Result<Option<Message>> {
    let mut header = [0; 4];
    loop {
        match reader.read(&mut header[..1]) {
            Ok(0) => return Ok(None),
            Ok(_) => break,
            Err(error) if error.kind() == std::io::ErrorKind::Interrupted => continue,
            Err(error) => return Err(error).context("reading IPC frame header"),
        }
    }
    reader
        .read_exact(&mut header[1..])
        .context("truncated IPC frame header")?;
    let size = usize::try_from(u32::from_le_bytes(header)).context("IPC frame length overflow")?;
    ensure!(
        (6..=MAX_FRAME_SIZE).contains(&size),
        "invalid IPC frame length: {size}"
    );
    let mut buffer = vec![0; size];
    buffer[..4].copy_from_slice(&header);
    reader
        .read_exact(&mut buffer[4..])
        .context("truncated IPC frame payload")?;
    let (message, consumed) =
        Message::read_version(Message::MAX_VERSION, &buffer).context("decoding IPC message")?;
    ensure!(
        consumed == size,
        "IPC decoder did not consume the complete frame"
    );
    Ok(Some(message))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;

    #[test]
    fn values_and_consecutive_frames_round_trip() -> Result<()> {
        let values = vec![
            Value::Int(i32::MIN),
            Value::UInt(u32::MAX),
            Value::Long(i64::MIN),
            Value::ULong(u64::MAX),
            Value::Float(1.25),
            Value::Double(-2.5),
            Value::String("hello \u{1f30d}".into()),
            Value::Bool(true),
            Value::Void,
            Value::Bytes(vec![0, 255]),
            Value::ByteChunks(vec![vec![], vec![1, 2], vec![3]]),
        ];
        let message = Message::Call {
            id: 42,
            call: FunctionCall {
                function: "Echo".into(),
                arguments: values,
                return_type: ValueType::Void,
            },
        };
        let mut data = Vec::new();
        write_message(&mut data, &message)?;
        write_message(&mut data, &Message::Shutdown)?;
        let mut input = Cursor::new(data);
        assert_eq!(read_message(&mut input)?, Some(message));
        assert_eq!(read_message(&mut input)?, Some(Message::Shutdown));
        assert_eq!(read_message(&mut input)?, None);
        Ok(())
    }

    #[test]
    fn malformed_frames_are_rejected() {
        for data in [
            vec![6],
            vec![6, 0, 0, 0],
            vec![3, 0, 0, 0],
            ((MAX_FRAME_SIZE + 1) as u32).to_le_bytes().to_vec(),
            vec![6, 0, 0, 0, 255, 255],
        ] {
            assert!(read_message(&mut Cursor::new(data)).is_err());
        }
    }

    #[test]
    fn oversized_output_is_rejected() {
        let message = Message::Result {
            id: 1,
            result: CallResult::Value(Value::Bytes(vec![0; MAX_FRAME_SIZE])),
        };
        let mut output = Vec::new();
        assert!(write_message(&mut output, &message).is_err());
        assert!(output.is_empty());
    }
}
