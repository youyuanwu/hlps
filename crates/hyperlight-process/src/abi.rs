//! Layouts for the Hyperlight 0.19.0 C API on Linux x86_64.
//! Tags use C integers so invalid foreign enum values can be checked.

use std::ffi::{CStr, CString, c_char, c_int};
use std::mem::size_of;
use std::ptr;

use anyhow::{Context, Result, bail, ensure};

use crate::protocol::{Value, ValueType};

#[repr(C)]
#[derive(Clone, Copy)]
pub struct ByteVector {
    pub data: *mut u8,
    pub len: usize,
}

#[repr(C)]
#[derive(Clone, Copy)]
pub struct ByteChunk {
    pub data: *const u8,
    pub len: usize,
}

#[repr(C)]
#[derive(Clone, Copy)]
pub struct ByteChunks {
    pub chunks: *const ByteChunk,
    pub count: usize,
}

#[repr(C)]
#[derive(Clone, Copy)]
pub union Payload {
    pub int: i32,
    pub uint: u32,
    pub long: i64,
    pub ulong: u64,
    pub float: f32,
    pub double: f64,
    pub string: *mut c_char,
    pub boolean: bool,
    pub bytes: ByteVector,
    pub byte_chunks: ByteChunks,
}

#[repr(C)]
#[derive(Clone, Copy)]
pub struct Parameter {
    pub tag: c_int,
    pub value: Payload,
}

pub type ReturnValue = Parameter;

#[repr(C)]
pub struct FunctionCall {
    pub function_name: *const c_char,
    pub parameters: *const Parameter,
    pub parameters_len: usize,
    pub return_type: c_int,
}

pub type GuestFunction = unsafe extern "C" fn(*const FunctionCall) -> *mut ReturnValue;

pub(crate) fn return_tag(value_type: ValueType) -> c_int {
    match value_type {
        ValueType::Int => 0,
        ValueType::UInt => 1,
        ValueType::Long => 2,
        ValueType::ULong => 3,
        ValueType::Float => 4,
        ValueType::Double => 5,
        ValueType::String => 6,
        ValueType::Bool => 7,
        ValueType::Void => 8,
        ValueType::Bytes => 9,
        ValueType::ByteChunks => 10,
    }
}

pub(crate) fn return_type(tag: c_int) -> Result<ValueType> {
    match tag {
        0 => Ok(ValueType::Int),
        1 => Ok(ValueType::UInt),
        2 => Ok(ValueType::Long),
        3 => Ok(ValueType::ULong),
        4 => Ok(ValueType::Float),
        5 => Ok(ValueType::Double),
        6 => Ok(ValueType::String),
        7 => Ok(ValueType::Bool),
        8 => Ok(ValueType::Void),
        9 => Ok(ValueType::Bytes),
        10 => Ok(ValueType::ByteChunks),
        _ => bail!("invalid C return type: {tag}"),
    }
}

pub(crate) fn parameter_type(tag: c_int) -> Result<ValueType> {
    match tag {
        8 => Ok(ValueType::Bytes),
        9 => Ok(ValueType::ByteChunks),
        0..=7 => return_type(tag),
        _ => bail!("invalid C parameter type: {tag}"),
    }
}

pub(crate) struct OwnedValue {
    pub raw: Parameter,
    _value: Value,
    _string: Option<CString>,
    _chunks: Vec<ByteChunk>,
}

impl OwnedValue {
    pub fn new(value: Value, parameter: bool) -> Result<Self> {
        let value_type = value.value_type();
        ensure!(
            !parameter || value_type != ValueType::Void,
            "Void is not a parameter type"
        );
        let string = match &value {
            Value::String(value) => Some(
                CString::new(value.as_bytes())
                    .context("C API strings cannot contain interior NUL bytes")?,
            ),
            _ => None,
        };
        let chunks = match &value {
            Value::ByteChunks(chunks) => chunks
                .iter()
                .map(|chunk| ByteChunk {
                    data: chunk.as_ptr(),
                    len: chunk.len(),
                })
                .collect(),
            _ => Vec::new(),
        };
        let payload = match &value {
            Value::Int(value) => Payload { int: *value },
            Value::UInt(value) => Payload { uint: *value },
            Value::Long(value) => Payload { long: *value },
            Value::ULong(value) => Payload { ulong: *value },
            Value::Float(value) => Payload { float: *value },
            Value::Double(value) => Payload { double: *value },
            Value::Bool(value) => Payload { boolean: *value },
            Value::Void => Payload { ulong: 0 },
            Value::String(_) => Payload {
                string: string
                    .as_ref()
                    .context("missing C string storage")?
                    .as_ptr()
                    .cast_mut(),
            },
            Value::Bytes(bytes) => Payload {
                bytes: ByteVector {
                    data: bytes.as_ptr().cast_mut(),
                    len: bytes.len(),
                },
            },
            Value::ByteChunks(_) => Payload {
                byte_chunks: ByteChunks {
                    chunks: chunks.as_ptr(),
                    count: chunks.len(),
                },
            },
        };
        let tag = if parameter {
            match value_type {
                ValueType::Bytes => 8,
                ValueType::ByteChunks => 9,
                _ => return_tag(value_type),
            }
        } else {
            return_tag(value_type)
        };
        Ok(Self {
            raw: Parameter {
                tag,
                value: payload,
            },
            _value: value,
            _string: string,
            _chunks: chunks,
        })
    }
}

pub(crate) unsafe fn string(pointer: *const c_char) -> Result<String> {
    ensure!(!pointer.is_null(), "null C string");
    let value = unsafe { CStr::from_ptr(pointer) };
    Ok(value.to_str().context("C string is not UTF-8")?.to_owned())
}

pub(crate) unsafe fn slice<'a, T>(pointer: *const T, len: usize) -> Result<&'a [T]> {
    if len == 0 {
        return Ok(&[]);
    }
    ensure!(!pointer.is_null(), "null C array with nonzero length");
    ensure!(
        len <= isize::MAX as usize / size_of::<T>(),
        "C array length overflow"
    );
    Ok(unsafe { std::slice::from_raw_parts(pointer, len) })
}

pub(crate) unsafe fn decode(raw: Parameter, parameter: bool) -> Result<Value> {
    let value_type = if parameter {
        parameter_type(raw.tag)?
    } else {
        return_type(raw.tag)?
    };
    Ok(unsafe {
        match value_type {
            ValueType::Int => Value::Int(raw.value.int),
            ValueType::UInt => Value::UInt(raw.value.uint),
            ValueType::Long => Value::Long(raw.value.long),
            ValueType::ULong => Value::ULong(raw.value.ulong),
            ValueType::Float => Value::Float(raw.value.float),
            ValueType::Double => Value::Double(raw.value.double),
            ValueType::String => Value::String(string(raw.value.string)?),
            ValueType::Bool => Value::Bool(raw.value.boolean),
            ValueType::Void => Value::Void,
            ValueType::Bytes => {
                let bytes = raw.value.bytes;
                Value::Bytes(slice(bytes.data, bytes.len)?.to_vec())
            }
            ValueType::ByteChunks => {
                let chunks = raw.value.byte_chunks;
                Value::ByteChunks(
                    slice(chunks.chunks, chunks.count)?
                        .iter()
                        .map(|chunk| Ok(slice(chunk.data, chunk.len)?.to_vec()))
                        .collect::<Result<_>>()?,
                )
            }
        }
    })
}

pub(crate) unsafe fn decode_call(
    pointer: *const FunctionCall,
) -> Result<crate::protocol::FunctionCall> {
    ensure!(!pointer.is_null(), "null C function call");
    let call = unsafe { &*pointer };
    Ok(crate::protocol::FunctionCall {
        function: unsafe { string(call.function_name)? },
        arguments: unsafe { slice(call.parameters, call.parameters_len)? }
            .iter()
            .map(|parameter| unsafe { decode(*parameter, true) })
            .collect::<Result<_>>()?,
        return_type: return_type(call.return_type)?,
    })
}

pub(crate) fn malloc_copy<T: Copy>(values: &[T]) -> Result<*mut T> {
    let size = std::mem::size_of_val(values).max(1);
    let pointer = unsafe { libc::malloc(size) }.cast::<T>();
    ensure!(
        !pointer.is_null(),
        "native C allocation failed for {size} bytes"
    );
    unsafe { ptr::copy_nonoverlapping(values.as_ptr(), pointer, values.len()) };
    Ok(pointer)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn all_types_preserve_values_and_use_distinct_byte_tags() -> Result<()> {
        let values = [
            Value::Int(i32::MIN),
            Value::UInt(u32::MAX),
            Value::Long(i64::MIN),
            Value::ULong(u64::MAX),
            Value::Float(1.25),
            Value::Double(-2.5),
            Value::String("hello".into()),
            Value::Bool(true),
            Value::Void,
            Value::Bytes(vec![]),
            Value::Bytes(vec![0, 255]),
            Value::ByteChunks(vec![vec![], vec![1, 2]]),
        ];
        for value in values {
            let owned = OwnedValue::new(value.clone(), false)?;
            assert_eq!(unsafe { decode(owned.raw, false)? }, value);
            if value != Value::Void {
                let owned = OwnedValue::new(value.clone(), true)?;
                assert_eq!(unsafe { decode(owned.raw, true)? }, value);
            }
        }
        assert_eq!(size_of::<Parameter>(), 24);
        assert_eq!(size_of::<FunctionCall>(), 32);
        Ok(())
    }

    #[test]
    fn invalid_tags_and_strings_are_rejected() {
        assert!(parameter_type(10).is_err());
        assert!(return_type(-1).is_err());
        assert!(OwnedValue::new(Value::Void, true).is_err());
        assert!(OwnedValue::new(Value::String("a\0b".into()), true).is_err());
        assert!(unsafe { string(ptr::null()) }.is_err());
        assert!(unsafe { slice::<u8>(ptr::null(), 1) }.is_err());
    }
}
