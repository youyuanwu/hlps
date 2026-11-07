//! Hyperlight's typed function traits with native IPC value conversions.

use anyhow::{Result, bail, ensure};
use hyperlight_common::flatbuffer_wrappers::function_types::ParameterType;
use hyperlight_common::for_each_tuple;
use hyperlight_common::func::{ParameterValue, ResultType, ReturnType, ReturnValue};

pub use hyperlight_common::func::{
    Bytes, ParameterTuple, SupportedParameterType, SupportedReturnType,
};

use crate::protocol::{FunctionCall, Value, ValueType};

/// A typed host callback accepting up to 32 arguments and a plain or fallible return.
pub struct HostFunction<Output: SupportedReturnType, Args: ParameterTuple> {
    function: Box<dyn FnMut(Args) -> Result<Output> + Send>,
}

impl<Output: SupportedReturnType, Args: ParameterTuple> HostFunction<Output, Args> {
    pub fn call(&mut self, arguments: Args) -> Result<Output> {
        (self.function)(arguments)
    }
}

macro_rules! impl_host_function {
    ([$N:expr] ($($argument:ident: $Parameter:ident),*)) => {
        impl<F, R, $($Parameter),*> From<F> for HostFunction<R::ReturnType, ($($Parameter,)*)>
        where
            F: FnMut($($Parameter),*) -> R + Send + 'static,
            ($($Parameter,)*): ParameterTuple,
            R: ResultType<anyhow::Error>,
        {
            fn from(mut function: F) -> Self {
                Self {
                    function: Box::new(move |($($argument,)*)| {
                        function($($argument),*).into_result()
                    }),
                }
            }
        }
    };
}

for_each_tuple!(impl_host_function);

pub(crate) struct RegisteredHostFunction {
    return_type: ValueType,
    function: Box<dyn FnMut(Vec<Value>) -> Result<Value> + Send>,
}

impl RegisteredHostFunction {
    pub fn call(&mut self, call: FunctionCall) -> Result<Value> {
        ensure!(
            call.return_type == self.return_type,
            "host return type mismatch: registered {:?}, requested {:?}",
            self.return_type,
            call.return_type
        );
        (self.function)(call.arguments)
    }
}

impl<Output: SupportedReturnType, Args: ParameterTuple> From<HostFunction<Output, Args>>
    for RegisteredHostFunction
{
    fn from(mut function: HostFunction<Output, Args>) -> Self {
        Self {
            return_type: Output::TYPE.into(),
            function: Box::new(move |arguments| {
                let arguments = arguments
                    .into_iter()
                    .map(ParameterValue::try_from)
                    .collect::<Result<_>>()?;
                let arguments = Args::from_value(arguments)?;
                Ok(SupportedReturnType::into_value(function.call(arguments)?).into())
            }),
        }
    }
}

impl From<ParameterValue> for Value {
    fn from(value: ParameterValue) -> Self {
        match value {
            ParameterValue::Int(value) => Self::Int(value),
            ParameterValue::UInt(value) => Self::UInt(value),
            ParameterValue::Long(value) => Self::Long(value),
            ParameterValue::ULong(value) => Self::ULong(value),
            ParameterValue::Float(value) => Self::Float(value),
            ParameterValue::Double(value) => Self::Double(value),
            ParameterValue::String(value) => Self::String(value),
            ParameterValue::Bool(value) => Self::Bool(value),
            ParameterValue::VecBytes(value) => Self::Bytes(value),
            ParameterValue::ByteChunks(value) => {
                Self::ByteChunks(value.into_iter().map(|chunk| chunk.to_vec()).collect())
            }
        }
    }
}

impl TryFrom<Value> for ParameterValue {
    type Error = anyhow::Error;

    fn try_from(value: Value) -> Result<Self> {
        Ok(match value {
            Value::Int(value) => Self::Int(value),
            Value::UInt(value) => Self::UInt(value),
            Value::Long(value) => Self::Long(value),
            Value::ULong(value) => Self::ULong(value),
            Value::Float(value) => Self::Float(value),
            Value::Double(value) => Self::Double(value),
            Value::String(value) => Self::String(value),
            Value::Bool(value) => Self::Bool(value),
            Value::Bytes(value) => Self::VecBytes(value),
            Value::ByteChunks(value) => {
                Self::ByteChunks(value.into_iter().map(Bytes::from).collect())
            }
            Value::Void => bail!("Void is not a host parameter type"),
        })
    }
}

impl From<ReturnValue> for Value {
    fn from(value: ReturnValue) -> Self {
        match value {
            ReturnValue::Int(value) => Self::Int(value),
            ReturnValue::UInt(value) => Self::UInt(value),
            ReturnValue::Long(value) => Self::Long(value),
            ReturnValue::ULong(value) => Self::ULong(value),
            ReturnValue::Float(value) => Self::Float(value),
            ReturnValue::Double(value) => Self::Double(value),
            ReturnValue::String(value) => Self::String(value),
            ReturnValue::Bool(value) => Self::Bool(value),
            ReturnValue::VecBytes(value) => Self::Bytes(value),
            ReturnValue::ByteChunks(value) => {
                Self::ByteChunks(value.into_iter().map(|chunk| chunk.to_vec()).collect())
            }
            ReturnValue::Void(()) => Self::Void,
        }
    }
}

impl From<Value> for ReturnValue {
    fn from(value: Value) -> Self {
        match value {
            Value::Int(value) => Self::Int(value),
            Value::UInt(value) => Self::UInt(value),
            Value::Long(value) => Self::Long(value),
            Value::ULong(value) => Self::ULong(value),
            Value::Float(value) => Self::Float(value),
            Value::Double(value) => Self::Double(value),
            Value::String(value) => Self::String(value),
            Value::Bool(value) => Self::Bool(value),
            Value::Bytes(value) => Self::VecBytes(value),
            Value::ByteChunks(value) => {
                Self::ByteChunks(value.into_iter().map(Bytes::from).collect())
            }
            Value::Void => Self::Void(()),
        }
    }
}

impl From<ParameterType> for ValueType {
    fn from(value: ParameterType) -> Self {
        match value {
            ParameterType::Int => Self::Int,
            ParameterType::UInt => Self::UInt,
            ParameterType::Long => Self::Long,
            ParameterType::ULong => Self::ULong,
            ParameterType::Float => Self::Float,
            ParameterType::Double => Self::Double,
            ParameterType::String => Self::String,
            ParameterType::Bool => Self::Bool,
            ParameterType::VecBytes => Self::Bytes,
            ParameterType::ByteChunks => Self::ByteChunks,
        }
    }
}

impl From<ReturnType> for ValueType {
    fn from(value: ReturnType) -> Self {
        match value {
            ReturnType::Int => Self::Int,
            ReturnType::UInt => Self::UInt,
            ReturnType::Long => Self::Long,
            ReturnType::ULong => Self::ULong,
            ReturnType::Float => Self::Float,
            ReturnType::Double => Self::Double,
            ReturnType::String => Self::String,
            ReturnType::Bool => Self::Bool,
            ReturnType::VecBytes => Self::Bytes,
            ReturnType::ByteChunks => Self::ByteChunks,
            ReturnType::Void => Self::Void,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn round_trip<T: SupportedParameterType + SupportedReturnType + std::fmt::Debug + PartialEq>(
        value: T,
    ) -> Result<()> {
        let parameter = <T as SupportedParameterType>::into_value(value.clone());
        let encoded = Value::from(parameter);
        assert_eq!(
            encoded.value_type(),
            <T as SupportedParameterType>::TYPE.into()
        );
        assert_eq!(
            <T as SupportedParameterType>::from_value(ParameterValue::try_from(encoded)?)?,
            value
        );
        let encoded = Value::from(<T as SupportedReturnType>::into_value(value.clone()));
        assert_eq!(
            encoded.value_type(),
            <T as SupportedReturnType>::TYPE.into()
        );
        assert_eq!(
            <T as SupportedReturnType>::from_value(encoded.into())?,
            value
        );
        Ok(())
    }

    #[test]
    fn all_supported_types_convert_without_losing_values() -> Result<()> {
        round_trip(i32::MIN)?;
        round_trip(u32::MAX)?;
        round_trip(i64::MIN)?;
        round_trip(u64::MAX)?;
        round_trip(1.25_f32)?;
        round_trip(-2.5_f64)?;
        round_trip("hello".to_owned())?;
        round_trip(true)?;
        round_trip(vec![0_u8, 255])?;
        round_trip(vec![Bytes::new(), Bytes::from_static(b"bytes")])?;
        assert_eq!(Value::from(ReturnValue::Void(())), Value::Void);
        assert_eq!(ReturnValue::from(Value::Void), ReturnValue::Void(()));
        assert!(ParameterValue::try_from(Value::Void).is_err());
        Ok(())
    }

    #[test]
    fn callbacks_support_zero_multiple_and_mutable_arguments() -> Result<()> {
        let mut zero: HostFunction<bool, ()> = (|| true).into();
        assert!(zero.call(())?);
        let mut multiple: HostFunction<i64, (i32, u32)> =
            (|left: i32, right: u32| i64::from(left) + i64::from(right)).into();
        assert_eq!(multiple.call((-1, u32::MAX))?, 4_294_967_294);
        let mut count = 0;
        let mut mutable: HostFunction<i32, ()> = (move || {
            count += 1;
            Ok(count)
        })
        .into();
        assert_eq!(mutable.call(())?, 1);
        assert_eq!(mutable.call(())?, 2);
        Ok(())
    }

    #[test]
    fn callback_signature_errors_do_not_invoke_the_callback() {
        let function: HostFunction<bool, (i32,)> =
            (|_: i32| -> bool { panic!("invalid call reached callback") }).into();
        let mut registered = RegisteredHostFunction::from(function);
        for (arguments, return_type) in [
            (vec![], ValueType::Bool),
            (vec![Value::String("wrong".into())], ValueType::Bool),
            (vec![Value::Void], ValueType::Bool),
            (vec![Value::Int(1)], ValueType::Int),
        ] {
            assert!(
                registered
                    .call(FunctionCall {
                        function: "Typed".into(),
                        arguments,
                        return_type,
                    })
                    .is_err()
            );
        }
    }

    #[test]
    fn callback_errors_are_preserved() {
        let function: HostFunction<bool, ()> =
            (|| -> Result<bool> { bail!("host refused") }).into();
        let mut registered = RegisteredHostFunction::from(function);
        let error = registered
            .call(FunctionCall {
                function: "Fail".into(),
                arguments: vec![],
                return_type: ValueType::Bool,
            })
            .unwrap_err();
        assert!(error.to_string().contains("host refused"));
    }
}
