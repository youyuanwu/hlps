use std::cell::RefCell;
use std::process::Command;
use std::sync::{Arc, Mutex};

use anyhow::{Result, bail};
use hyperlight_process::protocol::{FunctionCall, Value, ValueType};
use hyperlight_process::{InProcessRunner, InProcessRunnerBuilder, RunnerBuilder};

unsafe extern "C" {
    fn hyperlight_main();
}

fn builder() -> InProcessRunnerBuilder {
    InProcessRunnerBuilder::new(hyperlight_main)
}

#[test]
fn direct_c_guest_registration_calls_and_boundaries() -> Result<()> {
    let messages = Arc::new(Mutex::new(Vec::new()));
    let host_messages = Arc::clone(&messages);
    let mut runner = unsafe {
        builder()
            .host_function("HostLog", move |message: String| {
                host_messages.lock().unwrap().push(message);
                Ok(true)
            })
            .build()?
    };
    assert_eq!(runner.functions().len(), 2);
    assert_eq!(runner.functions()[0].name, "Add");
    assert_eq!(
        runner.functions()[0].parameters,
        [ValueType::Int, ValueType::Int]
    );
    assert_eq!(runner.functions()[0].return_type, ValueType::Long);
    for (left, right, expected) in [
        (20, 22, 42),
        (i32::MAX, i32::MAX, 4_294_967_294),
        (i32::MIN, i32::MIN, -4_294_967_296),
        (i32::MIN, i32::MAX, -1),
    ] {
        assert_eq!(runner.call::<i64>("Add", (left, right))?, expected);
    }
    let boundary = 255 - "Hello, ! This is a Hyperlight C guest.".len();
    for name in ["Hyperlight".to_owned(), String::new(), "x".repeat(boundary)] {
        let expected = format!("Hello, {name}! This is a Hyperlight C guest.");
        let greeting: String = runner.call("Greet", (name,))?;
        assert_eq!(greeting, expected);
        assert_eq!(messages.lock().unwrap().last(), Some(&expected));
    }
    assert_eq!(messages.lock().unwrap().len(), 3);
    assert!(
        runner
            .call::<String>("Greet", "x".repeat(boundary + 1))
            .unwrap_err()
            .to_string()
            .contains("255-byte limit")
    );
    assert!(
        runner
            .call::<String>("Greet", "a\0b".to_owned())
            .unwrap_err()
            .to_string()
            .contains("NUL")
    );
    assert!(runner.call::<i64>("Add", ()).is_err());
    assert!(runner.call::<i64>("Add", (1_i64, 2_i32)).is_err());
    assert!(runner.call::<i32>("Add", (1_i32, 2_i32)).is_err());
    assert!(runner.call::<()>("Missing", ()).is_err());
    assert_eq!(runner.call::<i64>("Add", (1_i32, 2_i32))?, 3);
    assert_eq!(
        runner.call_dynamic(FunctionCall {
            function: "Add".into(),
            arguments: vec![Value::Int(1), Value::Int(2)],
            return_type: ValueType::Long,
        })?,
        Value::Long(3)
    );
    Ok(())
}

#[test]
fn instances_keep_separate_host_callbacks_and_error_state() -> Result<()> {
    let mut calls = 0;
    let mut first = unsafe {
        builder()
            .host_function("HostLog", move |_: String| {
                calls += 1;
                calls != 2
            })
            .build()?
    };
    let mut second = unsafe {
        builder()
            .host_function("HostLog", |_: String| true)
            .build()?
    };
    let mut failing = unsafe {
        builder()
            .host_function("HostLog", |_: String| -> Result<bool> {
                bail!("in-process parent refused")
            })
            .build()?
    };
    assert!(first.call::<String>("Greet", "first".to_owned()).is_ok());
    assert!(second.call::<String>("Greet", "second".to_owned()).is_ok());
    assert!(
        failing
            .call::<String>("Greet", "failed".to_owned())
            .unwrap_err()
            .to_string()
            .contains("in-process parent refused")
    );
    assert!(
        first
            .call::<String>("Greet", "first again".to_owned())
            .unwrap_err()
            .to_string()
            .contains("did not acknowledge")
    );
    drop(second);
    assert!(
        first
            .call::<String>("Greet", "recovered".to_owned())
            .is_ok()
    );
    assert_eq!(failing.call::<i64>("Add", (1_i32, 2_i32))?, 3);
    Ok(())
}

#[test]
fn callback_configuration_and_signatures_are_validated() -> Result<()> {
    for name in ["", "bad\0name"] {
        assert!(
            unsafe { builder().host_function(name, || true).build() }
                .err()
                .unwrap()
                .to_string()
                .contains("host function name")
        );
    }
    let mut missing = unsafe { builder().build()? };
    assert!(
        missing
            .call::<String>("Greet", "test".to_owned())
            .unwrap_err()
            .to_string()
            .contains("unregistered host function")
    );
    let mut wrong_arguments = unsafe {
        builder()
            .host_function("HostLog", |_: i32| -> bool {
                panic!("wrong argument type invoked callback")
            })
            .build()?
    };
    assert!(
        wrong_arguments
            .call::<String>("Greet", "test".to_owned())
            .is_err()
    );
    let mut wrong_return = unsafe {
        builder()
            .host_function("HostLog", |_: String| -> i32 {
                panic!("wrong return type invoked callback")
            })
            .build()?
    };
    assert!(
        wrong_return
            .call::<String>("Greet", "test".to_owned())
            .unwrap_err()
            .to_string()
            .contains("host return type mismatch")
    );
    let mut replaced = unsafe {
        builder()
            .host_function("HostLog", |_: String| false)
            .host_function("HostLog", |_: String| true)
            .build()?
    };
    assert!(replaced.call::<String>("Greet", "test".to_owned()).is_ok());
    Ok(())
}

#[test]
fn callback_panic_does_not_unwind_through_c_and_poison_is_instance_local() -> Result<()> {
    let mut panicking = unsafe {
        builder()
            .host_function("HostLog", |_: String| -> bool {
                panic!("direct callback panicked")
            })
            .build()?
    };
    let error = panicking
        .call::<String>("Greet", "test".to_owned())
        .unwrap_err();
    assert!(error.to_string().contains("direct callback panicked"));
    assert!(
        panicking
            .call::<i64>("Add", (1_i32, 2_i32))
            .unwrap_err()
            .to_string()
            .contains("unusable")
    );
    let mut healthy = unsafe { builder().build()? };
    assert_eq!(healthy.call::<i64>("Add", (1_i32, 2_i32))?, 3);
    Ok(())
}

thread_local! {
    static OTHER_RUNNER: RefCell<Option<InProcessRunner>> = const { RefCell::new(None) };
}

#[test]
fn recursive_execution_and_initialization_are_rejected_without_losing_state() -> Result<()> {
    OTHER_RUNNER.with_borrow_mut(|runner| *runner = Some(unsafe { builder().build().unwrap() }));
    let outer_builder = builder().host_function("HostLog", |_: String| -> Result<bool> {
        let error = OTHER_RUNNER.with_borrow_mut(|runner| {
            runner
                .as_mut()
                .unwrap()
                .call::<i64>("Add", (1_i32, 2_i32))
                .unwrap_err()
        });
        assert!(error.to_string().contains("reentrant"));
        let error = unsafe { builder().build() }.err().unwrap();
        assert!(error.to_string().contains("reentrant"));
        Ok(true)
    });
    let mut outer = unsafe { outer_builder.build()? };
    assert!(outer.call::<String>("Greet", "outer".to_owned()).is_ok());
    assert_eq!(
        OTHER_RUNNER.with_borrow_mut(|runner| {
            runner.as_mut().unwrap().call::<i64>("Add", (1_i32, 2_i32))
        })?,
        3
    );
    OTHER_RUNNER.with_borrow_mut(|runner| *runner = None);
    assert_eq!(outer.call::<i64>("Add", (2_i32, 3_i32))?, 5);
    Ok(())
}

#[test]
fn direct_and_subprocess_results_match() -> Result<()> {
    let mut direct = unsafe {
        builder()
            .host_function("HostLog", |_: String| true)
            .build()?
    };
    let mut subprocess = RunnerBuilder::from_command(Command::new(env!(
        "CARGO_BIN_EXE_hyperlight-c-guest-native-runner"
    )))
    .host_function("HostLog", |_: String| true)
    .build()?;
    assert_eq!(
        direct.call::<String>("Greet", "parity".to_owned())?,
        subprocess.call::<String>("Greet", "parity".to_owned())?
    );
    assert_eq!(
        direct.call::<i64>("Add", (i32::MIN, i32::MAX))?,
        subprocess.call::<i64>("Add", (i32::MIN, i32::MAX))?
    );
    subprocess.shutdown()
}

#[test]
fn direct_demo_completes() -> Result<()> {
    let output = Command::new(env!("CARGO_BIN_EXE_hyperlight-c-guest-inprocess-demo")).output()?;
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(String::from_utf8(output.stdout)?.contains("All in-process guest checks passed."));
    Ok(())
}
