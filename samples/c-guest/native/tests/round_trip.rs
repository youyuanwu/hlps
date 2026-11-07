use std::process::Command;
use std::sync::{Arc, Mutex};

use anyhow::{Result, bail};
use hyperlight_process::protocol::{CallError, CallResult, FunctionCall, Value, ValueType};
use hyperlight_process::{Runner, RunnerBuilder};

fn spawn() -> Result<Runner> {
    Runner::spawn(&mut Command::new(env!(
        "CARGO_BIN_EXE_hyperlight-c-guest-native-runner"
    )))
}

fn builder() -> RunnerBuilder {
    RunnerBuilder::from_executable(env!("CARGO_BIN_EXE_hyperlight-c-guest-native-runner"))
}

fn call(name: &str, arguments: Vec<Value>, return_type: ValueType) -> FunctionCall {
    FunctionCall {
        function: name.into(),
        arguments,
        return_type,
    }
}

fn acknowledge(call: &FunctionCall) -> CallResult {
    assert_eq!(call.function, "HostLog");
    assert_eq!(call.return_type, ValueType::Bool);
    assert!(matches!(call.arguments.as_slice(), [Value::String(_)]));
    CallResult::Value(Value::Bool(true))
}

#[test]
fn registration_and_integer_extremes() -> Result<()> {
    let mut runner = spawn()?;
    let functions = runner.functions();
    assert_eq!(functions.len(), 2);
    assert_eq!(functions[0].name, "Add");
    assert_eq!(functions[0].parameters, [ValueType::Int, ValueType::Int]);
    assert_eq!(functions[0].return_type, ValueType::Long);
    assert_eq!(functions[1].name, "Greet");
    for (left, right, expected) in [
        (20, 22, 42),
        (i32::MAX, i32::MAX, 4_294_967_294),
        (i32::MIN, i32::MIN, -4_294_967_296),
        (i32::MIN, i32::MAX, -1),
    ] {
        let result = runner.call_with(
            call(
                "Add",
                vec![Value::Int(left), Value::Int(right)],
                ValueType::Long,
            ),
            |_| panic!("Add unexpectedly called the host"),
        )?;
        assert_eq!(result, Value::Long(expected));
    }
    runner.shutdown()
}

#[test]
fn greeting_boundaries_and_error_recovery() -> Result<()> {
    let mut runner = spawn()?;
    let boundary = 255 - "Hello, ! This is a Hyperlight C guest.".len();
    for name in ["Hyperlight".to_owned(), String::new(), "x".repeat(boundary)] {
        let expected = format!("Hello, {name}! This is a Hyperlight C guest.");
        let mut logged = Vec::new();
        let result = runner.call_with(
            call("Greet", vec![Value::String(name)], ValueType::String),
            |call| {
                logged.push(call.arguments.clone());
                acknowledge(call)
            },
        )?;
        assert_eq!(result, Value::String(expected.clone()));
        assert_eq!(logged, [vec![Value::String(expected)]]);
    }
    let error = runner
        .call_with(
            call(
                "Greet",
                vec![Value::String("x".repeat(boundary + 1))],
                ValueType::String,
            ),
            |_| panic!("oversized greeting called the host"),
        )
        .unwrap_err();
    assert!(error.to_string().contains("255-byte limit"));
    let error = runner
        .call_with(
            call(
                "Greet",
                vec![Value::String("a\0b".into())],
                ValueType::String,
            ),
            |_| panic!("invalid string called the host"),
        )
        .unwrap_err();
    assert!(error.to_string().contains("NUL"));
    assert_eq!(
        runner.call_with(
            call(
                "Greet",
                vec![Value::String("again".into())],
                ValueType::String
            ),
            acknowledge
        )?,
        Value::String("Hello, again! This is a Hyperlight C guest.".into())
    );
    runner.shutdown()
}

#[test]
fn invalid_calls_and_host_errors_are_not_successes() -> Result<()> {
    let mut runner = spawn()?;
    for invalid in [
        call("Missing", vec![], ValueType::Void),
        call("Add", vec![Value::Int(1)], ValueType::Long),
        call("Add", vec![Value::Long(1), Value::Int(2)], ValueType::Long),
        call("Add", vec![Value::Int(1), Value::Int(2)], ValueType::Int),
    ] {
        assert!(
            runner
                .call_with(invalid, |_| panic!("invalid guest call reached the host"))
                .is_err()
        );
    }
    let greet = call(
        "Greet",
        vec![Value::String("Hyperlight".into())],
        ValueType::String,
    );
    let error = runner
        .call_with(greet.clone(), |_| CallResult::Value(Value::Bool(false)))
        .unwrap_err();
    assert!(error.to_string().contains("did not acknowledge"));
    let error = runner
        .call_with(greet.clone(), |_| CallResult::Value(Value::Long(1)))
        .unwrap_err();
    assert!(error.to_string().contains("expected Bool"));
    let error = runner
        .call_with(greet.clone(), |_| {
            CallResult::Error(CallError {
                code: 123,
                message: "parent refused".into(),
            })
        })
        .unwrap_err();
    assert!(error.to_string().contains("parent refused"));
    assert!(runner.call_with(greet, acknowledge).is_ok());
    runner.shutdown()
}

#[test]
fn missing_runner_is_reported() {
    assert!(Runner::spawn(&mut Command::new("/nonexistent/hyperlight-process")).is_err());
}

#[test]
fn sandbox_style_builder_and_typed_calls() -> Result<()> {
    let messages = Arc::new(Mutex::new(Vec::new()));
    let host_messages = Arc::clone(&messages);
    let mut runner = builder()
        .host_function("HostLog", move |message: String| {
            host_messages.lock().unwrap().push(message);
            Ok(true)
        })
        .build()?;
    assert_eq!(
        runner.call::<String>("Greet", "Hyperlight".to_owned())?,
        "Hello, Hyperlight! This is a Hyperlight C guest."
    );
    let greeting: String = runner.call("Greet", ("tuple".to_owned(),))?;
    assert_eq!(greeting, "Hello, tuple! This is a Hyperlight C guest.");
    assert_eq!(messages.lock().unwrap().len(), 2);
    assert_eq!(runner.call::<i64>("Add", (20_i32, 22_i32))?, 42);
    assert_eq!(
        runner.call::<i64>("Add", (i32::MAX, i32::MAX))?,
        4_294_967_294
    );
    assert_eq!(
        runner.call_dynamic(call(
            "Add",
            vec![Value::Int(-1), Value::Int(1)],
            ValueType::Long
        ))?,
        Value::Long(0)
    );
    runner.shutdown()
}

#[test]
fn typed_callbacks_remain_mutable_and_errors_are_recoverable() -> Result<()> {
    let mut calls = 0;
    let mut runner = builder()
        .host_function("HostLog", move |_: String| {
            calls += 1;
            calls != 2
        })
        .build()?;
    assert!(runner.call::<String>("Greet", "first".to_owned()).is_ok());
    assert!(
        runner
            .call::<String>("Greet", "second".to_owned())
            .unwrap_err()
            .to_string()
            .contains("did not acknowledge")
    );
    assert!(runner.call::<String>("Greet", "third".to_owned()).is_ok());
    assert!(runner.call::<i64>("Add", ()).is_err());
    assert!(runner.call::<i64>("Add", (1_i64, 2_i32)).is_err());
    assert!(runner.call::<i32>("Add", (1_i32, 2_i32)).is_err());
    assert!(runner.call::<()>("Missing", ()).is_err());
    assert_eq!(runner.call::<i64>("Add", (1_i32, 2_i32))?, 3);
    runner.shutdown()
}

#[test]
fn registered_callbacks_reject_bad_signatures_and_propagate_errors() -> Result<()> {
    let mut wrong_arguments = builder()
        .host_function("HostLog", |_: i32| -> bool {
            panic!("wrong argument type invoked callback")
        })
        .build()?;
    assert!(
        wrong_arguments
            .call::<String>("Greet", "test".to_owned())
            .is_err()
    );
    wrong_arguments.shutdown()?;

    let mut wrong_return = builder()
        .host_function("HostLog", |_: String| -> i32 {
            panic!("wrong return type invoked callback")
        })
        .build()?;
    let error = wrong_return
        .call::<String>("Greet", "test".to_owned())
        .unwrap_err();
    assert!(error.to_string().contains("host return type mismatch"));
    wrong_return.shutdown()?;

    let mut failing = builder()
        .host_function("HostLog", |_: String| -> Result<bool> {
            bail!("typed parent refused")
        })
        .build()?;
    let error = failing
        .call::<String>("Greet", "test".to_owned())
        .unwrap_err();
    assert!(error.to_string().contains("typed parent refused"));
    failing.shutdown()
}

#[test]
fn missing_registered_callback_is_an_explicit_error() -> Result<()> {
    let mut runner = builder().build()?;
    let error = runner
        .call::<String>("Greet", "test".to_owned())
        .unwrap_err();
    assert!(
        error
            .to_string()
            .contains("unregistered host function: HostLog")
    );
    assert_eq!(runner.call::<i64>("Add", (20_i32, 22_i32))?, 42);
    runner.shutdown()
}

#[test]
fn duplicate_host_registration_uses_the_last_callback() -> Result<()> {
    let mut runner = builder()
        .host_function("HostLog", |_: String| false)
        .host_function("HostLog", |_: String| true)
        .build()?;
    assert!(runner.call::<String>("Greet", "test".to_owned()).is_ok());
    runner.shutdown()
}

#[test]
fn invalid_builder_names_fail_before_spawning() {
    for name in ["", "bad\0name"] {
        let result = RunnerBuilder::from_executable("/nonexistent/hyperlight-process")
            .host_function(name, || true)
            .build();
        assert!(
            result
                .err()
                .unwrap()
                .to_string()
                .contains("host function name")
        );
    }
}
