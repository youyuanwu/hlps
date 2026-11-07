use std::cell::RefCell;
use std::collections::BTreeMap;
use std::ffi::{CString, c_char, c_int};
use std::io::{BufReader, BufWriter, Read, Write};
use std::ptr;

use anyhow::{Context, Result, bail, ensure};

use crate::abi::{self, ByteChunk, ByteChunks, ByteVector, GuestFunction, OwnedValue, ReturnValue};
use crate::protocol::{
    CallError, CallResult, FunctionCall, FunctionSignature, Message, PROTOCOL_VERSION, Value,
    ValueType, read_message, write_message,
};

const GUEST_ERROR: i32 = 15;
const HOST_ERROR: i32 = 17;

#[derive(Clone)]
struct RegisteredFunction {
    signature: FunctionSignature,
    callback: GuestFunction,
}

#[derive(Default)]
struct State {
    functions: BTreeMap<String, RegisteredFunction>,
    error: Option<CallError>,
    fatal: Option<anyhow::Error>,
    // C return pointers must remain stable when subsequent helpers append values.
    #[allow(clippy::vec_box)]
    returns: Vec<Box<OwnedValue>>,
    host_return: Option<Value>,
    next_callback: u64,
    ready: bool,
    input: Option<Box<dyn Read>>,
    output: Option<Box<dyn Write>>,
}

thread_local! {
    static STATE: RefCell<State> = RefCell::new(State::default());
}

fn record_error(code: i32, message: String) {
    eprintln!("native guest error {code}: {message}");
    STATE.with_borrow_mut(|state| {
        if state.error.is_none() {
            state.error = Some(CallError { code, message });
        }
    });
}

fn send(message: &Message) -> Result<()> {
    STATE.with_borrow_mut(|state| {
        write_message(
            state
                .output
                .as_mut()
                .context("runner output is unavailable")?,
            message,
        )
    })
}

fn receive() -> Result<Option<Message>> {
    STATE.with_borrow_mut(|state| {
        read_message(
            state
                .input
                .as_mut()
                .context("runner input is unavailable")?,
        )
    })
}

/// Serve requests using a statically linked guest's initialization function.
///
/// # Safety
/// The initializer and registered callbacks must obey the Hyperlight 0.19 C ABI,
/// run on this thread, and keep all foreign pointers valid for their documented lifetime.
/// Guest code must be trusted; it executes with this process's full privileges.
pub unsafe fn run(initialize: unsafe extern "C" fn()) -> Result<()> {
    unsafe {
        run_with_io(
            initialize,
            Box::new(BufReader::new(std::io::stdin())),
            Box::new(BufWriter::new(std::io::stdout())),
        )
    }
}

unsafe fn run_with_io(
    initialize: unsafe extern "C" fn(),
    input: Box<dyn Read>,
    output: Box<dyn Write>,
) -> Result<()> {
    STATE.with_borrow_mut(|state| {
        *state = State {
            input: Some(input),
            output: Some(output),
            next_callback: 1,
            ..State::default()
        }
    });
    let result = unsafe { serve(initialize) };
    STATE.with_borrow_mut(|state| *state = State::default());
    result
}

unsafe fn serve(initialize: unsafe extern "C" fn()) -> Result<()> {
    unsafe { initialize() };
    let functions = STATE.with_borrow_mut(|state| -> Result<Vec<_>> {
        if let Some(error) = state.error.take() {
            return Err(error.into());
        }
        Ok(state
            .functions
            .values()
            .map(|function| function.signature.clone())
            .collect())
    })?;
    send(&Message::Hello {
        protocol_version: PROTOCOL_VERSION,
        functions,
    })?;
    STATE.with_borrow_mut(|state| state.ready = true);
    while let Some(message) = receive()? {
        match message {
            Message::Call { id, call } => {
                let result = match unsafe { execute(&call) } {
                    Ok(value) => CallResult::Value(value),
                    Err(error) => CallResult::Error(error),
                };
                if let Some(error) = STATE.with_borrow_mut(|state| state.fatal.take()) {
                    return Err(error);
                }
                send(&Message::Result { id, result })?;
            }
            Message::Shutdown => return Ok(()),
            message => bail!("unexpected message to native runner: {message:?}"),
        }
    }
    Ok(())
}

fn failure(code: i32, message: impl Into<String>) -> CallError {
    CallError {
        code,
        message: message.into(),
    }
}

unsafe fn execute(call: &FunctionCall) -> std::result::Result<Value, CallError> {
    STATE.with_borrow_mut(|state| {
        state.error = None;
        state.host_return = None;
        state.returns.clear();
    });
    let function = STATE
        .with_borrow(|state| state.functions.get(&call.function).cloned())
        .ok_or_else(|| failure(4, format!("unknown guest function: {}", call.function)))?;
    if call.arguments.len() != function.signature.parameters.len() {
        return Err(failure(5, "incorrect number of guest arguments"));
    }
    for (index, (argument, expected)) in call
        .arguments
        .iter()
        .zip(&function.signature.parameters)
        .enumerate()
    {
        if argument.value_type() != *expected {
            return Err(failure(
                14,
                format!(
                    "argument {index}: expected {expected:?}, got {:?}",
                    argument.value_type()
                ),
            ));
        }
    }
    if call.return_type != function.signature.return_type {
        return Err(failure(
            14,
            "requested return type differs from the registered signature",
        ));
    }
    let arguments = call
        .arguments
        .iter()
        .cloned()
        .map(|value| OwnedValue::new(value, true))
        .collect::<Result<Vec<_>>>()
        .map_err(|error| failure(GUEST_ERROR, error.to_string()))?;
    let parameters: Vec<_> = arguments.iter().map(|argument| argument.raw).collect();
    let name = CString::new(call.function.as_bytes())
        .map_err(|error| failure(GUEST_ERROR, error.to_string()))?;
    let ffi_call = abi::FunctionCall {
        function_name: name.as_ptr(),
        parameters: parameters.as_ptr(),
        parameters_len: parameters.len(),
        return_type: abi::return_tag(call.return_type),
    };
    let returned = unsafe { (function.callback)(&ffi_call) };
    if let Some(error) = STATE.with_borrow_mut(|state| state.error.take()) {
        return Err(error);
    }
    if returned.is_null() {
        return Err(failure(
            GUEST_ERROR,
            "guest returned NULL without reporting an error",
        ));
    }
    let value = unsafe { abi::decode(*returned, false) }
        .map_err(|error| failure(GUEST_ERROR, error.to_string()))?;
    if value.value_type() != call.return_type {
        return Err(failure(
            14,
            format!(
                "guest returned {:?}, expected {:?}",
                value.value_type(),
                call.return_type
            ),
        ));
    }
    Ok(value)
}

#[unsafe(no_mangle)]
/// # Safety
/// Names must be live NUL-terminated UTF-8 strings. The type array must contain
/// `param_no` valid entries. The callback must obey the C guest function ABI.
pub unsafe extern "C" fn hl_register_function_definition(
    name: *const c_char,
    callback: Option<GuestFunction>,
    param_no: usize,
    parameter_types: *const c_int,
    return_type: c_int,
) {
    let registration = (|| -> Result<RegisteredFunction> {
        let name = unsafe { abi::string(name)? };
        ensure!(!name.is_empty(), "empty guest function name");
        let parameters = unsafe { abi::slice(parameter_types, param_no)? }
            .iter()
            .map(|tag| abi::parameter_type(*tag))
            .collect::<Result<_>>()?;
        Ok(RegisteredFunction {
            signature: FunctionSignature {
                name,
                parameters,
                return_type: abi::return_type(return_type)?,
            },
            callback: callback.context("null guest callback")?,
        })
    })();
    match registration {
        Ok(function) => {
            let duplicate = STATE.with_borrow_mut(|state| {
                if state.functions.contains_key(&function.signature.name) {
                    true
                } else {
                    state
                        .functions
                        .insert(function.signature.name.clone(), function);
                    false
                }
            });
            if duplicate {
                record_error(GUEST_ERROR, "duplicate guest function registration".into());
            }
        }
        Err(error) => record_error(GUEST_ERROR, error.to_string()),
    }
}

#[unsafe(no_mangle)]
/// # Safety
/// The call and every pointer-bearing parameter must be live for this call.
pub unsafe extern "C" fn hl_call_host_function(call: *const abi::FunctionCall) {
    if !STATE.with_borrow(|state| state.ready) {
        record_error(
            HOST_ERROR,
            "host callbacks are unavailable before the initialization handshake".into(),
        );
        return;
    }
    STATE.with_borrow_mut(|state| state.host_return = None);
    let call = match unsafe { abi::decode_call(call) } {
        Ok(call) => call,
        Err(error) => {
            record_error(HOST_ERROR, error.to_string());
            return;
        }
    };
    let exchange = (|| -> Result<CallResult> {
        let id = STATE.with_borrow_mut(|state| -> Result<u64> {
            let id = state.next_callback;
            state.next_callback = id.checked_add(1).context("host callback ID overflow")?;
            Ok(id)
        })?;
        send(&Message::HostCall {
            id,
            call: call.clone(),
        })?;
        match receive()?.context("parent exited during a host callback")? {
            Message::HostResult {
                id: response_id,
                result,
            } if response_id == id => Ok(result),
            message => bail!("unexpected host callback response: {message:?}"),
        }
    })();
    match exchange {
        Ok(CallResult::Value(value)) if value.value_type() == call.return_type => {
            STATE.with_borrow_mut(|state| state.host_return = Some(value));
        }
        Ok(CallResult::Value(value)) => record_error(
            HOST_ERROR,
            format!(
                "host function {} returned {:?}, expected {:?}",
                call.function,
                value.value_type(),
                call.return_type
            ),
        ),
        Ok(CallResult::Error(error)) => record_error(
            HOST_ERROR,
            format!("host function {}: {error}", call.function),
        ),
        Err(error) => {
            record_error(HOST_ERROR, error.to_string());
            STATE.with_borrow_mut(|state| state.fatal = Some(error));
        }
    }
}

#[unsafe(no_mangle)]
/// # Safety
/// `message` must be a live NUL-terminated UTF-8 string.
pub unsafe extern "C" fn hl_set_error(code: c_int, message: *const c_char) {
    match unsafe { abi::string(message) } {
        Ok(message) => {
            if code == 0 {
                STATE.with_borrow_mut(|state| state.error = None);
            } else {
                record_error(code, message);
            }
        }
        Err(error) => record_error(GUEST_ERROR, error.to_string()),
    }
}

fn store_return(value: Result<Value>) -> *mut ReturnValue {
    match value.and_then(|value| OwnedValue::new(value, false)) {
        Ok(value) => STATE.with_borrow_mut(|state| {
            let mut value = Box::new(value);
            let pointer = &mut value.raw as *mut ReturnValue;
            state.returns.push(value);
            pointer
        }),
        Err(error) => {
            record_error(GUEST_ERROR, error.to_string());
            ptr::null_mut()
        }
    }
}

fn take_host_return(expected: ValueType) -> Result<Value> {
    let value = STATE
        .with_borrow_mut(|state| state.host_return.take())
        .context("no unconsumed host return value")?;
    ensure!(
        value.value_type() == expected,
        "host return getter expected {expected:?}, got {:?}",
        value.value_type()
    );
    Ok(value)
}

macro_rules! scalar_api {
    ($result:ident, $getter:ident, $variant:ident, $ty:ty, $failure:expr) => {
        #[unsafe(no_mangle)]
        pub extern "C" fn $result(value: $ty) -> *mut ReturnValue {
            store_return(Ok(Value::$variant(value)))
        }

        #[unsafe(no_mangle)]
        pub extern "C" fn $getter() -> $ty {
            match take_host_return(ValueType::$variant) {
                Ok(Value::$variant(value)) => value,
                result => {
                    record_error(
                        HOST_ERROR,
                        format!(
                            "invalid host return for {}: {result:?}",
                            stringify!($getter)
                        ),
                    );
                    $failure
                }
            }
        }
    };
}

scalar_api!(
    hl_result_from_Int,
    hl_get_host_return_value_as_Int,
    Int,
    i32,
    0
);
scalar_api!(
    hl_result_from_UInt,
    hl_get_host_return_value_as_UInt,
    UInt,
    u32,
    0
);
scalar_api!(
    hl_result_from_Long,
    hl_get_host_return_value_as_Long,
    Long,
    i64,
    0
);
scalar_api!(
    hl_result_from_ULong,
    hl_get_host_return_value_as_ULong,
    ULong,
    u64,
    0
);
scalar_api!(
    hl_result_from_Float,
    hl_get_host_return_value_as_Float,
    Float,
    f32,
    0.0
);
scalar_api!(
    hl_result_from_Double,
    hl_get_host_return_value_as_Double,
    Double,
    f64,
    0.0
);
scalar_api!(
    hl_result_from_Bool,
    hl_get_host_return_value_as_Bool,
    Bool,
    bool,
    false
);

#[unsafe(no_mangle)]
pub extern "C" fn hl_result_from_Void() -> *mut ReturnValue {
    store_return(Ok(Value::Void))
}

#[unsafe(no_mangle)]
/// # Safety
/// `value` must point to a live NUL-terminated UTF-8 string.
pub unsafe extern "C" fn hl_result_from_String(value: *const c_char) -> *mut ReturnValue {
    store_return(unsafe { abi::string(value) }.map(Value::String))
}

#[unsafe(no_mangle)]
/// # Safety
/// `data` must reference `len` readable bytes, unless `len` is zero.
pub unsafe extern "C" fn hl_result_from_Bytes(data: *const u8, len: usize) -> *mut ReturnValue {
    store_return(unsafe { abi::slice(data, len) }.map(|bytes| Value::Bytes(bytes.to_vec())))
}

#[unsafe(no_mangle)]
/// # Safety
/// Every chunk must reference its declared number of readable bytes.
pub unsafe extern "C" fn hl_result_from_ByteChunks(value: ByteChunks) -> *mut ReturnValue {
    let raw = ReturnValue {
        tag: 10,
        value: abi::Payload { byte_chunks: value },
    };
    store_return(unsafe { abi::decode(raw, false) })
}

fn host_pointer<T>(result: Result<*mut T>) -> *mut T {
    match result {
        Ok(pointer) => pointer,
        Err(error) => {
            record_error(HOST_ERROR, error.to_string());
            ptr::null_mut()
        }
    }
}

#[unsafe(no_mangle)]
pub extern "C" fn hl_get_host_return_value_as_String() -> *const c_char {
    host_pointer((|| {
        let Value::String(value) = take_host_return(ValueType::String)? else {
            bail!("expected string")
        };
        let string = CString::new(value).context("host string contains interior NUL bytes")?;
        abi::malloc_copy(string.as_bytes_with_nul()).map(|pointer| pointer.cast::<c_char>())
    })())
}

#[unsafe(no_mangle)]
pub extern "C" fn hl_get_host_return_value_as_VecBytes() -> *mut ByteVector {
    host_pointer((|| {
        let Value::Bytes(value) = take_host_return(ValueType::Bytes)? else {
            bail!("expected bytes")
        };
        let data = abi::malloc_copy(&value)?;
        match abi::malloc_copy(&[ByteVector {
            data,
            len: value.len(),
        }]) {
            Ok(pointer) => Ok(pointer),
            Err(error) => {
                unsafe { libc::free(data.cast()) };
                Err(error)
            }
        }
    })())
}

#[unsafe(no_mangle)]
pub extern "C" fn hl_get_host_return_value_as_ByteChunks() -> *mut ByteChunks {
    host_pointer((|| {
        let Value::ByteChunks(value) = take_host_return(ValueType::ByteChunks)? else {
            bail!("expected byte chunks")
        };
        let mut chunks: Vec<ByteChunk> = Vec::new();
        let allocated = (|| -> Result<*mut ByteChunks> {
            for bytes in &value {
                chunks.push(ByteChunk {
                    data: abi::malloc_copy(bytes)?,
                    len: bytes.len(),
                });
            }
            let data = abi::malloc_copy(&chunks)?;
            match abi::malloc_copy(&[ByteChunks {
                chunks: data,
                count: chunks.len(),
            }]) {
                Ok(pointer) => Ok(pointer),
                Err(error) => {
                    unsafe { libc::free(data.cast()) };
                    Err(error)
                }
            }
        })();
        if allocated.is_err() {
            for chunk in chunks {
                unsafe { libc::free(chunk.data.cast_mut().cast()) };
            }
        }
        allocated
    })())
}

#[unsafe(no_mangle)]
/// # Safety
/// `value` must be null or an unfreed pointer returned by the ByteChunks getter.
pub unsafe extern "C" fn hl_free_byte_chunks(value: *mut ByteChunks) {
    if value.is_null() {
        return;
    }
    let descriptor = unsafe { *value };
    let chunks = match unsafe { abi::slice(descriptor.chunks, descriptor.count) } {
        Ok(chunks) => chunks,
        Err(error) => {
            record_error(GUEST_ERROR, error.to_string());
            return;
        }
    };
    for chunk in chunks {
        unsafe { libc::free(chunk.data.cast_mut().cast()) };
    }
    unsafe {
        libc::free(descriptor.chunks.cast_mut().cast());
        libc::free(value.cast());
    }
}

#[unsafe(no_mangle)]
/// # Safety
/// `message` and `file` must be live NUL-terminated UTF-8 strings.
pub unsafe extern "C" fn hl_log(
    level: usize,
    message: *const c_char,
    line: i32,
    file: *const c_char,
) {
    let decoded = (|| -> Result<_> {
        Ok((unsafe { abi::string(message)? }, unsafe {
            abi::string(file)?
        }))
    })();
    match decoded {
        Ok((message, file)) => eprintln!("guest log {level} {file}:{line}: {message}"),
        Err(error) => record_error(GUEST_ERROR, error.to_string()),
    }
}

#[unsafe(no_mangle)]
pub extern "C" fn hl_abort_with_code(code: i32) -> ! {
    eprintln!("native guest aborted with code {code}");
    std::process::exit(if code == 0 { 1 } else { code })
}

#[unsafe(no_mangle)]
/// # Safety
/// `message` must be a live NUL-terminated UTF-8 string.
pub unsafe extern "C" fn hl_abort_with_code_and_message(code: i32, message: *const c_char) -> ! {
    match unsafe { abi::string(message) } {
        Ok(message) => eprintln!("native guest abort: {message}"),
        Err(error) => eprintln!("native guest abort with invalid message: {error}"),
    }
    hl_abort_with_code(code)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;
    use std::rc::Rc;

    #[derive(Clone)]
    struct Output(Rc<RefCell<Vec<u8>>>);

    impl Write for Output {
        fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
            self.0.borrow_mut().extend_from_slice(bytes);
            Ok(bytes.len())
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }

    fn values() -> Vec<Value> {
        vec![
            Value::Int(i32::MIN),
            Value::UInt(u32::MAX),
            Value::Long(i64::MIN),
            Value::ULong(u64::MAX),
            Value::Float(1.25),
            Value::Double(-2.5),
            Value::String("owned string".into()),
            Value::Bool(true),
            Value::Void,
            Value::Bytes(vec![0, 255]),
            Value::ByteChunks(vec![vec![], vec![1, 2]]),
        ]
    }

    unsafe extern "C" fn initialize() {
        for value in values() {
            for (prefix, callback) in [
                ("Echo", echo as GuestFunction),
                ("Bounce", bounce as GuestFunction),
            ] {
                let name = CString::new(format!("{prefix}{:?}", value.value_type())).unwrap();
                let parameter = OwnedValue::new(value.clone(), value != Value::Void).unwrap();
                let types = if value == Value::Void {
                    vec![]
                } else {
                    vec![parameter.raw.tag]
                };
                unsafe {
                    hl_register_function_definition(
                        name.as_ptr(),
                        Some(callback),
                        types.len(),
                        types.as_ptr(),
                        abi::return_tag(value.value_type()),
                    )
                };
            }
        }
    }

    fn result(value: Value) -> *mut ReturnValue {
        match value {
            Value::Int(value) => hl_result_from_Int(value),
            Value::UInt(value) => hl_result_from_UInt(value),
            Value::Long(value) => hl_result_from_Long(value),
            Value::ULong(value) => hl_result_from_ULong(value),
            Value::Float(value) => hl_result_from_Float(value),
            Value::Double(value) => hl_result_from_Double(value),
            Value::Bool(value) => hl_result_from_Bool(value),
            Value::Void => hl_result_from_Void(),
            Value::String(value) => unsafe {
                hl_result_from_String(CString::new(value).unwrap().as_ptr())
            },
            Value::Bytes(value) => unsafe { hl_result_from_Bytes(value.as_ptr(), value.len()) },
            Value::ByteChunks(value) => {
                let chunks: Vec<_> = value
                    .iter()
                    .map(|bytes| ByteChunk {
                        data: bytes.as_ptr(),
                        len: bytes.len(),
                    })
                    .collect();
                unsafe {
                    hl_result_from_ByteChunks(ByteChunks {
                        chunks: chunks.as_ptr(),
                        count: chunks.len(),
                    })
                }
            }
        }
    }

    unsafe extern "C" fn echo(call: *const abi::FunctionCall) -> *mut ReturnValue {
        let call = unsafe { abi::decode_call(call) }.unwrap();
        result(call.arguments.into_iter().next().unwrap_or(Value::Void))
    }

    unsafe extern "C" fn bounce(call: *const abi::FunctionCall) -> *mut ReturnValue {
        let call = unsafe { &*call };
        let host_call = abi::FunctionCall {
            function_name: c"ParentEcho".as_ptr(),
            parameters: call.parameters,
            parameters_len: call.parameters_len,
            return_type: call.return_type,
        };
        unsafe { hl_call_host_function(&host_call) };
        let value = match abi::return_type(call.return_type).unwrap() {
            ValueType::Int => Value::Int(hl_get_host_return_value_as_Int()),
            ValueType::UInt => Value::UInt(hl_get_host_return_value_as_UInt()),
            ValueType::Long => Value::Long(hl_get_host_return_value_as_Long()),
            ValueType::ULong => Value::ULong(hl_get_host_return_value_as_ULong()),
            ValueType::Float => Value::Float(hl_get_host_return_value_as_Float()),
            ValueType::Double => Value::Double(hl_get_host_return_value_as_Double()),
            ValueType::Bool => Value::Bool(hl_get_host_return_value_as_Bool()),
            ValueType::Void => Value::Void,
            ValueType::String => {
                let pointer = hl_get_host_return_value_as_String();
                if pointer.is_null() {
                    return ptr::null_mut();
                }
                let value = unsafe { abi::string(pointer) }.unwrap();
                unsafe { libc::free(pointer.cast_mut().cast()) };
                Value::String(value)
            }
            ValueType::Bytes => {
                let pointer = hl_get_host_return_value_as_VecBytes();
                if pointer.is_null() {
                    return ptr::null_mut();
                }
                let descriptor = unsafe { *pointer };
                let bytes = unsafe { abi::slice(descriptor.data, descriptor.len) }
                    .unwrap()
                    .to_vec();
                unsafe {
                    libc::free(descriptor.data.cast());
                    libc::free(pointer.cast());
                }
                Value::Bytes(bytes)
            }
            ValueType::ByteChunks => {
                let pointer = hl_get_host_return_value_as_ByteChunks();
                if pointer.is_null() {
                    return ptr::null_mut();
                }
                let raw = ReturnValue {
                    tag: 10,
                    value: abi::Payload {
                        byte_chunks: unsafe { *pointer },
                    },
                };
                let value = unsafe { abi::decode(raw, false) }.unwrap();
                unsafe { hl_free_byte_chunks(pointer) };
                value
            }
        };
        result(value)
    }

    fn run_fixture(messages: &[Message]) -> Result<Vec<Message>> {
        let mut input = Vec::new();
        for message in messages {
            write_message(&mut input, message)?;
        }
        let output = Output(Rc::new(RefCell::new(Vec::new())));
        unsafe {
            run_with_io(
                initialize,
                Box::new(Cursor::new(input)),
                Box::new(output.clone()),
            )?
        };
        let mut data = Cursor::new(output.0.borrow().clone());
        let mut messages = Vec::new();
        while let Some(message) = read_message(&mut data)? {
            messages.push(message);
        }
        Ok(messages)
    }

    fn call_message(id: u64, prefix: &str, value: &Value) -> Message {
        Message::Call {
            id,
            call: FunctionCall {
                function: format!("{prefix}{:?}", value.value_type()),
                arguments: if *value == Value::Void {
                    vec![]
                } else {
                    vec![value.clone()]
                },
                return_type: value.value_type(),
            },
        }
    }

    #[test]
    fn all_registered_types_and_host_getters_round_trip() -> Result<()> {
        let values = values();
        let mut requests = Vec::new();
        for (index, value) in values.iter().enumerate() {
            let id = index as u64 * 2 + 1;
            requests.push(call_message(id, "Echo", value));
            requests.push(call_message(id + 1, "Bounce", value));
            requests.push(Message::HostResult {
                id: index as u64 + 1,
                result: CallResult::Value(value.clone()),
            });
        }
        requests.push(Message::Shutdown);
        let mut replies = run_fixture(&requests)?.into_iter();
        let Some(Message::Hello {
            functions,
            protocol_version,
        }) = replies.next()
        else {
            panic!("missing handshake")
        };
        assert_eq!(protocol_version, PROTOCOL_VERSION);
        assert_eq!(functions.len(), values.len() * 2);
        for (index, value) in values.iter().enumerate() {
            let id = index as u64 * 2 + 1;
            assert_eq!(
                replies.next(),
                Some(Message::Result {
                    id,
                    result: CallResult::Value(value.clone())
                })
            );
            let Some(Message::HostCall {
                id: callback_id,
                call,
            }) = replies.next()
            else {
                panic!("missing host callback")
            };
            assert_eq!(callback_id, index as u64 + 1);
            assert_eq!(call.function, "ParentEcho");
            assert_eq!(call.return_type, value.value_type());
            assert_eq!(
                call.arguments,
                if *value == Value::Void {
                    vec![]
                } else {
                    vec![value.clone()]
                }
            );
            assert_eq!(
                replies.next(),
                Some(Message::Result {
                    id: id + 1,
                    result: CallResult::Value(value.clone())
                })
            );
        }
        assert!(replies.next().is_none());
        Ok(())
    }

    #[test]
    fn mismatched_host_result_and_missing_result_are_errors() -> Result<()> {
        let request = call_message(1, "Bounce", &Value::Bool(true));
        let replies = run_fixture(&[
            request.clone(),
            Message::HostResult {
                id: 1,
                result: CallResult::Value(Value::Long(42)),
            },
            Message::Shutdown,
        ])?;
        assert!(
            matches!(replies.last(), Some(Message::Result { result: CallResult::Error(error), .. })
            if error.code == HOST_ERROR && error.message.contains("expected Bool"))
        );
        assert!(run_fixture(&[request]).is_err());
        Ok(())
    }

    #[test]
    fn invalid_registration_is_reported_before_handshake() {
        unsafe extern "C" fn invalid() {
            unsafe {
                hl_register_function_definition(c"Invalid".as_ptr(), None, 0, ptr::null(), 8)
            };
        }
        let output = Output(Rc::new(RefCell::new(Vec::new())));
        let result = unsafe {
            run_with_io(
                invalid,
                Box::new(Cursor::new(Vec::new())),
                Box::new(output.clone()),
            )
        };
        assert!(result.is_err());
        assert!(output.0.borrow().is_empty());
    }

    #[test]
    fn initialization_host_callback_fails_without_waiting_for_ipc() {
        unsafe extern "C" fn invalid() {
            let call = abi::FunctionCall {
                function_name: c"TooEarly".as_ptr(),
                parameters: ptr::null(),
                parameters_len: 0,
                return_type: 8,
            };
            unsafe { hl_call_host_function(&call) };
        }
        let output = Output(Rc::new(RefCell::new(Vec::new())));
        let result = unsafe {
            run_with_io(
                invalid,
                Box::new(Cursor::new(Vec::new())),
                Box::new(output.clone()),
            )
        };
        assert!(
            result
                .unwrap_err()
                .to_string()
                .contains("initialization handshake")
        );
        assert!(output.0.borrow().is_empty());
    }
}
