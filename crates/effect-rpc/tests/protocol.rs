use effect_rpc::{ConnectionOptions, Router, serve_connection};
use futures::{SinkExt, StreamExt, channel::mpsc, stream};
use serde_json::{Value, json};
use std::sync::Arc;
use std::time::Duration;

struct Ctx;

fn router() -> Router<Ctx> {
    Router::new()
        .unary("echo", |_c: Arc<Ctx>, v: Value| async move { Ok::<_, Value>(v) })
        .unary("fail", |_c: Arc<Ctx>, _: Value| async move {
            Err::<Value, _>(json!({"_tag": "Nope", "message": "no"}))
        })
        .unary("sleep", |_c: Arc<Ctx>, _: Value| async move {
            tokio::time::sleep(Duration::from_secs(30)).await;
            Ok::<_, Value>(json!(null))
        })
        .stream("count", |_c: Arc<Ctx>, n: u32| {
            stream::iter((0..n).map(|i| Ok::<_, Value>(json!(i))))
        })
        .unary("panic", |_c: Arc<Ctx>, _: Value| async move {
            if true {
                panic!("boom");
            }
            Ok::<Value, Value>(json!(null))
        })
}

struct Harness {
    to_server: mpsc::UnboundedSender<Result<String, String>>,
    from_server: mpsc::UnboundedReceiver<String>,
}

impl Harness {
    fn start(options: ConnectionOptions) -> Self {
        let (to_server, incoming) = mpsc::unbounded::<Result<String, String>>();
        let (outgoing, from_server) = mpsc::unbounded::<String>();
        tokio::spawn(serve_connection(
            Arc::new(router()),
            Arc::new(Ctx),
            incoming,
            outgoing.sink_map_err(|_| ()),
            options,
        ));
        Self { to_server, from_server }
    }
    fn send(&self, v: Value) {
        self.to_server.unbounded_send(Ok(v.to_string())).unwrap();
    }
    async fn recv(&mut self) -> Value {
        let frame = tokio::time::timeout(Duration::from_secs(2), self.from_server.next())
            .await
            .expect("timed out waiting for server frame")
            .expect("server closed");
        serde_json::from_str(&frame).unwrap()
    }
}

fn req(id: &str, tag: &str, payload: Value) -> Value {
    json!({"_tag": "Request", "id": id, "tag": tag, "payload": payload, "headers": []})
}

#[tokio::test]
async fn unary_success_and_failure() {
    let mut h = Harness::start(Default::default());
    h.send(req("1", "echo", json!({"a": 1})));
    assert_eq!(
        h.recv().await,
        json!({"_tag": "Exit", "requestId": "1", "exit": {"_tag": "Success", "value": {"a": 1}}})
    );
    h.send(req("2", "fail", json!(null)));
    assert_eq!(
        h.recv().await,
        json!({"_tag": "Exit", "requestId": "2", "exit": {"_tag": "Failure", "cause": [
            {"_tag": "Fail", "error": {"_tag": "Nope", "message": "no"}}
        ]}})
    );
}

#[tokio::test]
async fn unknown_tag_and_panic_are_defects() {
    let mut h = Harness::start(Default::default());
    h.send(req("1", "missing", json!(null)));
    let v = h.recv().await;
    assert_eq!(v["exit"]["cause"][0]["_tag"], "Die");
    h.send(req("2", "panic", json!(null)));
    let v = h.recv().await;
    assert_eq!(v["exit"]["cause"][0], json!({"_tag": "Die", "defect": "boom"}));
}

#[tokio::test]
async fn ping_pong_and_batched_frames() {
    let mut h = Harness::start(Default::default());
    h.send(json!([{"_tag": "Ping"}, req("1", "echo", json!(7))]));
    let a = h.recv().await;
    let b = h.recv().await;
    let mut tags = vec![a["_tag"].clone(), b["_tag"].clone()];
    tags.sort_by_key(|t| t.to_string());
    assert_eq!(tags, vec![json!("Exit"), json!("Pong")]);
}

#[tokio::test]
async fn stream_waits_for_ack_between_chunks() {
    let mut h = Harness::start(ConnectionOptions { max_chunk_values: 2, ..Default::default() });
    h.send(req("9", "count", json!(5)));
    assert_eq!(h.recv().await, json!({"_tag": "Chunk", "requestId": "9", "values": [0, 1]}));
    // No ack yet: nothing else may arrive.
    assert!(
        tokio::time::timeout(Duration::from_millis(150), h.from_server.next()).await.is_err(),
        "server must wait for Ack"
    );
    h.send(json!({"_tag": "Ack", "requestId": "9"}));
    assert_eq!(h.recv().await["values"], json!([2, 3]));
    h.send(json!({"_tag": "Ack", "requestId": "9"}));
    assert_eq!(h.recv().await["values"], json!([4]));
    h.send(json!({"_tag": "Ack", "requestId": "9"}));
    assert_eq!(
        h.recv().await,
        json!({"_tag": "Exit", "requestId": "9", "exit": {"_tag": "Success", "value": null}})
    );
}

#[tokio::test]
async fn interrupt_replies_with_interrupt_exit() {
    let mut h = Harness::start(Default::default());
    h.send(req("5", "sleep", json!(null)));
    h.send(json!({"_tag": "Interrupt", "requestId": "5"}));
    let v = h.recv().await;
    assert_eq!(v["requestId"], "5");
    assert_eq!(v["exit"]["cause"][0]["_tag"], "Interrupt");
}

#[tokio::test]
async fn notifications_get_no_reply_and_discard_acks_immediately() {
    let mut h = Harness::start(Default::default());
    let mut n = req("1", "echo", json!(1));
    n["isNotification"] = json!(true);
    h.send(n);
    let mut d = req("2", "sleep", json!(null));
    d["discard"] = json!(true);
    h.send(d);
    assert_eq!(
        h.recv().await,
        json!({"_tag": "Exit", "requestId": "2", "exit": {"_tag": "Success", "value": null}})
    );
}

#[tokio::test]
async fn a_client_that_stops_reading_holds_up_its_streams_instead_of_queueing_without_limit() {
    use std::sync::atomic::{AtomicUsize, Ordering};
    let pulled = Arc::new(AtomicUsize::new(0));
    let counter = pulled.clone();
    let router = Router::new().stream("forever", move |_c: Arc<Ctx>, _: Value| {
        let counter = counter.clone();
        stream::repeat_with(move || {
            counter.fetch_add(1, Ordering::Relaxed);
            Ok::<_, Value>(json!("x"))
        })
    });
    let (to_server, incoming) = mpsc::unbounded::<Result<String, String>>();
    // A socket whose buffer filled up: the first send never completes.
    let (outgoing, _never_read) = mpsc::channel::<String>(0);
    let options = ConnectionOptions { supports_ack: false, max_chunk_values: 16, ..Default::default() };
    tokio::spawn(serve_connection(Arc::new(router), Arc::new(Ctx), incoming, outgoing.sink_map_err(|_| ()), options));
    to_server.unbounded_send(Ok(req("1", "forever", json!(null)).to_string())).unwrap();
    tokio::time::sleep(Duration::from_millis(300)).await;
    let first = pulled.load(Ordering::Relaxed);
    tokio::time::sleep(Duration::from_millis(300)).await;
    assert_eq!(pulled.load(Ordering::Relaxed), first, "the stream stopped being pulled");
    assert!(first < 300 * 16, "at most a queue's worth of chunks was produced ({first} values)");
}
