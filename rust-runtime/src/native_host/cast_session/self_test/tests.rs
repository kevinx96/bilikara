use super::*;

#[test]
fn packaged_sample_is_pinned_and_corruption_is_rejected() {
    let bytes = include_bytes!("../../../../../static/cast-samples/sample-b.ts");
    assert!(samples::verified(bytes));
    let mut broken = bytes.to_vec();
    broken[0] ^= 1;
    assert!(!samples::verified(&broken));
    assert!(!samples::verified(&bytes[..bytes.len() - 1]));
}

pub(crate) async fn run(
    context: Arc<HostContext>,
    renderer: Option<Renderer>,
    qr: Option<String>,
    cancel: CancellationToken,
    send: mpsc::Sender<UserAnswers>,
    receive: mpsc::Receiver<UserAnswers>,
    timing: runner::Timing,
) -> diag::Report {
    let trace = Arc::new(
        SessionTrace::new(
            worker::platform(),
            if qr.is_some() {
                diag::TargetKind::Purek
            } else {
                diag::TargetKind::Generic
            },
            "0.8.0",
            "11",
        )
        .unwrap(),
    );
    with_app(|app| {
        let cast = &mut app.native().cast;
        cast.self_test = State {
            generation: 1,
            running: true,
            cancel: cancel.clone(),
            answers: Some(send),
            trace: Some(trace.clone()),
            ..Default::default()
        };
        Ok(())
    })
    .unwrap();
    runner::run(
        context,
        1,
        Request {
            renderer,
            qr,
            confirmed: false,
        },
        trace.clone(),
        cancel.clone(),
        receive,
        timing,
    )
    .await;
    trace.finish(cancel.is_cancelled());
    update(1, |s| {
        s.running = false;
        s.question = false;
    });
    trace.report()
}

pub(crate) async fn question() -> Value {
    loop {
        let (notify, state) = with_app(|app| {
            Ok((
                app.native().cast.self_test.changed.clone(),
                app.native().cast.self_test.snapshot(),
            ))
        })
        .unwrap();
        let notified = notify.notified();
        tokio::pin!(notified);
        notified.as_mut().enable();
        let state = with_app(|app| Ok(app.native().cast.self_test.snapshot())).unwrap_or(state);
        if state["question"] == true || (state["generation"] == 1 && state["running"] == false) {
            return state;
        }
        tokio::time::timeout(Duration::from_secs(45), notified)
            .await
            .expect("self-test must make progress");
    }
}
