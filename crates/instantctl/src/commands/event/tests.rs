use super::*;
use serde_json::json;
use std::{
    cell::Cell,
    future::{pending, poll_fn},
    pin::Pin,
    sync::atomic::{AtomicUsize, Ordering},
    task::Poll,
};

fn event(id: &str, seconds: Option<f64>) -> Event {
    serde_json::from_value(json!({"id":id,"occurrenceTime":seconds})).unwrap()
}

fn options(tail: usize) -> FollowOptions {
    FollowOptions {
        since: None,
        tail,
        interval: Duration::ZERO,
    }
}

fn ids(events: &[Event]) -> Vec<&str> {
    events.iter().map(|event| event.id.as_str()).collect()
}

#[test]
fn since_uses_offsets_and_durations_and_rejects_future_before_requests() {
    let now: Timestamp = "2026-10-07T12:00:00Z".parse().unwrap();
    for (value, expected) in [
        ("2026-10-07T14:00:00+03:00", "2026-10-07T11:00:00Z"),
        ("2h30m", "2026-10-07T09:30:00Z"),
        ("1d", "2026-10-06T12:00:00Z"),
        ("P1W", "2026-09-30T12:00:00Z"),
        ("0s", "2026-10-07T12:00:00Z"),
    ] {
        assert_eq!(
            parse_since(Some(value), now).unwrap().unwrap().to_string(),
            expected
        );
    }
    for value in [
        "2026-10-07T12:00:00.000000001Z",
        "-1h",
        "1h ago",
        "2026-02-30T00:00:00Z",
        "1 month",
        "",
    ] {
        assert_eq!(
            parse_since(Some(value), now).unwrap_err().kind,
            ErrorKind::Usage,
            "{value}"
        );
    }
    assert_eq!(parse_since(None, now).unwrap(), None);
}

#[test]
fn epoch_seconds_filter_and_order_are_correct_and_unknown_times_are_not_invented() {
    let events = vec![
        event("later", Some(1002.0)),
        event("unknown", None),
        event("early", Some(1000.0)),
    ];
    assert_eq!(
        ids(&ordered(&events, None).unwrap()),
        ["unknown", "early", "later"]
    );
    assert_eq!(
        ids(&ordered(&events, Some(Timestamp::from_second(1001).unwrap())).unwrap()),
        ["later"]
    );
    assert!(event_rows(&[event("unknown", None)]).unwrap()[0]["occurrence_time"].is_null());
    assert_eq!(
        event_time(&event("fraction", Some(1000.5)))
            .unwrap()
            .unwrap()
            .as_nanosecond(),
        1_000_500_000_000
    );
    assert_eq!(
        event_time(&event("bad", Some(f64::MAX))).unwrap_err().kind,
        ErrorKind::Unverified
    );
}

#[test]
fn cursor_tails_first_poll_and_dedupes_repeats_backfills_and_same_second_arrivals() {
    let mut cursor = Cursor::default();
    let first = vec![
        event("c", Some(3.0)),
        event("a", Some(1.0)),
        event("b", Some(2.0)),
    ];
    assert_eq!(ids(&cursor.take(&first, &options(2)).unwrap()), ["b", "c"]);
    let second = vec![
        event("c", Some(3.0)),
        event("backfill", Some(2.0)),
        event("same-second", Some(3.0)),
        event("d", Some(4.0)),
    ];
    assert_eq!(
        ids(&cursor.take(&second, &options(2)).unwrap()),
        ["same-second", "d"]
    );
    assert!(cursor.take(&second, &options(2)).unwrap().is_empty());
    assert!(
        cursor
            .take(
                &[event("a", Some(5.0)), event("unknown", None)],
                &options(2)
            )
            .unwrap()
            .is_empty()
    );
    let mut no_history = Cursor::default();
    assert!(no_history.take(&first, &options(0)).unwrap().is_empty());
    assert_eq!(
        ids(&no_history
            .take(&[event("new", Some(4.0))], &options(0))
            .unwrap()),
        ["new"]
    );
}

#[test]
fn follow_formats_are_framed_and_unknown_alert_fields_remain_null() {
    let events = vec![event("a", None), event("b", Some(1.0))];
    let mut out = Vec::new();
    write_follow(&mut out, Format::Json, &events).unwrap();
    let text = String::from_utf8(out).unwrap();
    let rows: Vec<Value> = text
        .lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect();
    assert_eq!(rows.len(), 2);
    assert_eq!(rows[0]["id"], "a");
    assert!(rows[0]["state"].is_null());
    let mut yaml = Vec::new();
    write_follow(&mut yaml, Format::Yaml, &events).unwrap();
    let documents = serde_yaml_ng::Deserializer::from_slice(&yaml);
    assert_eq!(documents.count(), 2);
    let mut table = Vec::new();
    write_follow(&mut table, Format::Table, &events).unwrap();
    assert!(
        String::from_utf8(table)
            .unwrap()
            .contains("OCCURRENCE_TIME")
    );
    let alerts: Vec<Alert> =
        serde_json::from_value(json!([{"id":"a","severity":"new-value"}])).unwrap();
    let rows = alert_rows(&alerts).unwrap();
    assert_eq!(rows[0]["severity"], "new-value");
    assert!(rows[0]["cleared_time"].is_null());
}

#[tokio::test]
async fn transient_failure_warns_once_recovers_and_auth_failure_stops() {
    for transient in [ErrorKind::RetryLater, ErrorKind::General] {
        let calls = Cell::new(0);
        let mut out = Vec::new();
        let mut warnings = Vec::new();
        let result = follow(
            || {
                let call = calls.get();
                calls.set(call + 1);
                async move {
                    match call {
                        0 => Err(Error::new(transient, "temporary")),
                        1 => Ok(vec![event("new", Some(1.0))]),
                        _ => Err(Error::new(ErrorKind::Auth, "expired")),
                    }
                }
            },
            pending(),
            options(20),
            Format::Json,
            &mut out,
            &mut warnings,
        )
        .await
        .unwrap_err();
        assert_eq!(
            result.downcast_ref::<Error>().unwrap().kind,
            ErrorKind::Auth
        );
        assert_eq!(calls.get(), 3);
        assert_eq!(String::from_utf8(warnings).unwrap().lines().count(), 1);
        assert_eq!(serde_json::from_slice::<Value>(&out).unwrap()["id"], "new");
    }
}

#[derive(Default)]
struct Flushed {
    bytes: Vec<u8>,
    flushes: usize,
}
impl Write for Flushed {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        self.bytes.extend_from_slice(bytes);
        Ok(bytes.len())
    }
    fn flush(&mut self) -> io::Result<()> {
        self.flushes += 1;
        Ok(())
    }
}

#[tokio::test]
async fn shutdown_during_sleep_or_inflight_get_flushes_and_prevents_another_poll() {
    let (delay_poll_tx, delay_poll_rx) = tokio::sync::oneshot::channel();
    let (stop_tx, stop_rx) = tokio::sync::oneshot::channel();
    let mut delay_poll_tx = Some(delay_poll_tx);
    let mut stop_rx = Box::pin(stop_rx);
    let mut stop_polls = 0;
    let stop = poll_fn(move |cx| {
        stop_polls += 1;
        if stop_polls == 2 {
            let _ = delay_poll_tx.take().unwrap().send(());
        }
        match Pin::as_mut(&mut stop_rx).poll(cx) {
            Poll::Ready(Ok(result)) => Poll::Ready(result),
            Poll::Ready(Err(_)) => Poll::Ready(Err(io::Error::new(
                io::ErrorKind::BrokenPipe,
                "test shutdown channel closed",
            ))),
            Poll::Pending => Poll::Pending,
        }
    });
    let mut config = options(20);
    config.interval = Duration::from_secs(3600);
    let sleep_task = tokio::spawn(async move {
        let calls = AtomicUsize::new(0);
        let mut out = Flushed::default();
        follow(
            || {
                calls.fetch_add(1, Ordering::Relaxed);
                async { Ok(vec![event("a", Some(1.0))]) }
            },
            stop,
            config,
            Format::Json,
            &mut out,
            &mut Vec::new(),
        )
        .await?;
        Ok::<_, anyhow::Error>((calls.load(Ordering::Relaxed), out))
    });
    delay_poll_rx.await.unwrap();
    stop_tx.send(Ok(())).unwrap();
    let (calls, out) = sleep_task.await.unwrap().unwrap();
    assert_eq!(calls, 1);
    assert!(out.flushes >= 2); // Each emitted batch and clean shutdown flush.
    let before = out.flushes;

    let (fetch_started_tx, fetch_started_rx) = tokio::sync::oneshot::channel();
    let (fetch_dropped_tx, fetch_dropped_rx) = tokio::sync::oneshot::channel();
    let (stop_tx, stop_rx) = tokio::sync::oneshot::channel();
    let inflight_task = tokio::spawn(async move {
        let calls = AtomicUsize::new(0);
        let mut started_tx = Some(fetch_started_tx);
        let mut dropped_tx = Some(fetch_dropped_tx);
        let mut out = out;
        let stop = async move {
            stop_rx.await.unwrap_or_else(|_| {
                Err(io::Error::new(
                    io::ErrorKind::BrokenPipe,
                    "test shutdown channel closed",
                ))
            })
        };
        follow(
            || {
                calls.fetch_add(1, Ordering::Relaxed);
                let started = started_tx.take().unwrap();
                let dropped = dropped_tx.take().unwrap();
                async move {
                    let _drop_signal = DropSignal(Some(dropped));
                    let _ = started.send(());
                    pending::<Result<Vec<Event>, Error>>().await
                }
            },
            stop,
            options(20),
            Format::Json,
            &mut out,
            &mut Vec::new(),
        )
        .await?;
        Ok::<_, anyhow::Error>((calls.load(Ordering::Relaxed), out))
    });
    fetch_started_rx.await.unwrap();
    stop_tx.send(Ok(())).unwrap();
    let (calls, out) = inflight_task.await.unwrap().unwrap();
    fetch_dropped_rx.await.unwrap();
    assert_eq!(calls, 1);
    assert_eq!(out.flushes, before + 1);
}

struct DropSignal(Option<tokio::sync::oneshot::Sender<()>>);

impl Drop for DropSignal {
    fn drop(&mut self) {
        if let Some(sender) = self.0.take() {
            let _ = sender.send(());
        }
    }
}

#[tokio::test]
async fn broken_stdout_and_nontransient_errors_stop_without_retry() {
    struct Broken;
    impl Write for Broken {
        fn write(&mut self, _: &[u8]) -> io::Result<usize> {
            Err(io::ErrorKind::BrokenPipe.into())
        }
        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }
    let error = follow(
        || async { Ok(vec![event("a", Some(1.0))]) },
        pending(),
        options(20),
        Format::Json,
        &mut Broken,
        &mut Vec::new(),
    )
    .await
    .unwrap_err();
    assert_eq!(
        error.downcast_ref::<io::Error>().unwrap().kind(),
        io::ErrorKind::BrokenPipe
    );
    for kind in [
        ErrorKind::Unverified,
        ErrorKind::NotFound,
        ErrorKind::ClientError,
    ] {
        let calls = Cell::new(0);
        let error = follow(
            || {
                calls.set(calls.get() + 1);
                async move { Err(Error::new(kind, "failure")) }
            },
            pending(),
            options(20),
            Format::Json,
            &mut Vec::new(),
            &mut Vec::new(),
        )
        .await
        .unwrap_err();
        assert_eq!(error.downcast_ref::<Error>().unwrap().kind, kind);
        assert_eq!(calls.get(), 1);
    }
}
