use super::*;
use std::sync::Mutex;

#[tokio::test]
async fn running_dsh_is_reused_without_preparing_or_opening_it() {
    start_dsh_background(
        true,
        async { panic!("must not configure an existing process") },
        |_| async { panic!("must not open or activate an existing process") },
    )
    .await
    .unwrap();
}

#[tokio::test]
async fn stopped_dsh_prepares_bridge_then_requests_hidden_background_launch_once() {
    let calls = Mutex::new(Vec::new());
    start_dsh_background(
        false,
        async {
            calls.lock().unwrap().push("prepare");
            Ok(())
        },
        |args| {
            assert_eq!(args, &["-g", "-j", "-a"]);
            async {
                calls.lock().unwrap().push("open");
                Ok(())
            }
        },
    )
    .await
    .unwrap();
    assert_eq!(*calls.lock().unwrap(), ["prepare", "open"]);
}

#[tokio::test]
async fn failed_bridge_preparation_never_opens_dsh() {
    let error = start_dsh_background(
        false,
        async { Err("synthetic preparation failure".into()) },
        |_| async { panic!("must not open with an unprepared bridge") },
    )
    .await
    .unwrap_err();
    assert_eq!(error, "synthetic preparation failure");
}

#[tokio::test]
async fn failed_hidden_launch_is_returned_without_retrying_or_using_foreground_launch() {
    let calls = Mutex::new(0);
    let error = start_dsh_background(false, async { Ok(()) }, |_| async {
        *calls.lock().unwrap() += 1;
        Err("synthetic launch failure".into())
    })
    .await
    .unwrap_err();
    assert_eq!(error, "synthetic launch failure");
    assert_eq!(*calls.lock().unwrap(), 1);
}
