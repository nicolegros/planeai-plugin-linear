use serde_json::{json, Value};
use std::collections::HashMap;
use std::fmt;
use std::future::Future;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use tokio::io::{AsyncBufRead, AsyncBufReadExt, AsyncReadExt, AsyncWrite, AsyncWriteExt};
use tokio::sync::{mpsc, oneshot};

/// Frames are newline-delimited and capped at 64 KiB including the newline.
pub const MAX_FRAME_BYTES: usize = 64 * 1024;
pub const CANCELLED: i64 = -32800;
pub const INVALID_REQUEST: i64 = -32600;
pub const METHOD_NOT_FOUND: i64 = -32601;
pub const PLUGIN_ERROR: i64 = -32000;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RpcError {
    pub code: i64,
    pub message: String,
}

impl RpcError {
    pub fn new(code: i64, message: impl Into<String>) -> Self {
        Self {
            code,
            message: message.into(),
        }
    }

    pub fn failed(message: impl Into<String>) -> Self {
        Self::new(PLUGIN_ERROR, message)
    }
}

impl fmt::Display for RpcError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.message)
    }
}

impl From<String> for RpcError {
    fn from(message: String) -> Self {
        Self::failed(message)
    }
}

/// One inbound frame, classified at the protocol boundary.
#[derive(Debug, PartialEq)]
enum Frame {
    Request {
        jsonrpc_ok: bool,
        id: Value,
        method: String,
        params: Value,
    },
    Notification {
        method: String,
        params: Value,
    },
    Response {
        id: Value,
        outcome: Result<Value, RpcError>,
    },
    Invalid(String),
}

fn classify(line: &str) -> Frame {
    let value: Value = match serde_json::from_str(line) {
        Ok(value) => value,
        Err(error) => return Frame::Invalid(format!("malformed JSON-RPC frame: {error}")),
    };
    let jsonrpc_ok = value.get("jsonrpc").and_then(Value::as_str) == Some("2.0");
    let params = value.get("params").cloned().unwrap_or(Value::Null);
    match (value.get("method").and_then(Value::as_str), value.get("id")) {
        (Some(method), Some(id)) => Frame::Request {
            jsonrpc_ok,
            id: id.clone(),
            method: method.to_string(),
            params,
        },
        (Some(method), None) => Frame::Notification {
            method: method.to_string(),
            params,
        },
        (None, Some(id)) if jsonrpc_ok => {
            let outcome = match (value.get("result"), value.get("error")) {
                (Some(result), None) => Ok(result.clone()),
                (None, Some(error)) => Err(RpcError::new(
                    error.get("code").and_then(Value::as_i64).unwrap_or(-32603),
                    error
                        .get("message")
                        .and_then(Value::as_str)
                        .unwrap_or("host request failed"),
                )),
                _ => Err(RpcError::new(
                    -32603,
                    "host response must carry exactly one of result or error",
                )),
            };
            Frame::Response {
                id: id.clone(),
                outcome,
            }
        }
        _ => Frame::Invalid("frame is neither a request nor a response".to_string()),
    }
}

/// Reads one frame. An oversized frame is skipped through its newline and reported as an error.
async fn read_frame<R: AsyncBufRead + Unpin>(
    reader: &mut R,
) -> std::io::Result<Option<Result<String, String>>> {
    let mut frame = Vec::new();
    let read = (&mut *reader)
        .take(MAX_FRAME_BYTES as u64)
        .read_until(b'\n', &mut frame)
        .await?;
    if read == 0 {
        return Ok(None);
    }
    if !frame.ends_with(b"\n") {
        if read < MAX_FRAME_BYTES {
            return Ok(Some(Err("frame was not newline terminated".to_string())));
        }
        let mut rest = Vec::new();
        reader.read_until(b'\n', &mut rest).await?;
        return Ok(Some(Err("frame exceeded the 64 KiB limit".to_string())));
    }
    Ok(Some(
        String::from_utf8(frame).map_err(|error| format!("frame is not UTF-8: {error}")),
    ))
}

pub fn is_scalar_id(id: &Value) -> bool {
    matches!(id, Value::Null | Value::String(_) | Value::Number(_))
}

pub fn encode_response(id: Value, outcome: Result<Value, RpcError>) -> String {
    let frame = |id: Value, outcome: Result<Value, RpcError>| -> String {
        match outcome {
            Ok(result) => json!({ "jsonrpc": "2.0", "id": id, "result": result }),
            Err(error) => json!({
                "jsonrpc": "2.0",
                "id": id,
                "error": { "code": error.code, "message": error.message },
            }),
        }
        .to_string()
    };
    let encoded = frame(id.clone(), outcome);
    if encoded.len() < MAX_FRAME_BYTES {
        return encoded;
    }
    let oversized = || {
        Err(RpcError::failed(
            "plugin response exceeded the maximum frame size",
        ))
    };
    let fallback = frame(id, oversized());
    if fallback.len() < MAX_FRAME_BYTES {
        return fallback;
    }
    frame(Value::Null, oversized())
}

type PendingCalls = Arc<Mutex<HashMap<String, oneshot::Sender<Result<Value, RpcError>>>>>;

/// Nested JSON-RPC calls from the sidecar to PlaneAI, correlated by `plugin:N` ids.
#[derive(Clone)]
pub struct HostClient {
    outgoing: mpsc::UnboundedSender<String>,
    pending: PendingCalls,
    next_id: Arc<AtomicU64>,
}

impl HostClient {
    fn new(outgoing: mpsc::UnboundedSender<String>) -> Self {
        Self {
            outgoing,
            pending: Arc::default(),
            next_id: Arc::new(AtomicU64::new(0)),
        }
    }

    pub async fn call(&self, method: &str, params: Value) -> Result<Value, RpcError> {
        let id = format!("plugin:{}", self.next_id.fetch_add(1, Ordering::SeqCst) + 1);
        let request = json!({ "jsonrpc": "2.0", "id": id, "method": method, "params": params });
        let encoded = request.to_string();
        if encoded.len() >= MAX_FRAME_BYTES {
            return Err(RpcError::failed(
                "host RPC request exceeded the maximum frame size",
            ));
        }
        let (sender, receiver) = oneshot::channel();
        self.pending.lock().unwrap().insert(id.clone(), sender);
        let _forget = ForgetOnDrop {
            pending: &self.pending,
            id: &id,
        };
        self.outgoing
            .send(encoded)
            .map_err(|_| RpcError::failed("plugin RPC output is closed"))?;
        receiver
            .await
            .unwrap_or_else(|_| Err(RpcError::failed("host closed plugin RPC input")))
    }

    fn deliver(&self, id: &Value, outcome: Result<Value, RpcError>) -> bool {
        let Some(id) = id.as_str() else { return false };
        match self.pending.lock().unwrap().remove(id) {
            Some(sender) => {
                let _ = sender.send(outcome);
                true
            }
            None => false,
        }
    }

    fn fail_all(&self) {
        self.pending.lock().unwrap().clear();
    }
}

struct ForgetOnDrop<'a> {
    pending: &'a PendingCalls,
    id: &'a str,
}

impl Drop for ForgetOnDrop<'_> {
    fn drop(&mut self) {
        self.pending.lock().unwrap().remove(self.id);
    }
}

pub trait Handler {
    fn handle(
        &mut self,
        method: &str,
        params: Value,
        host: &HostClient,
    ) -> impl Future<Output = Result<Value, RpcError>>;
}

struct InboundRequest {
    jsonrpc_ok: bool,
    id: Value,
    method: String,
    params: Value,
}

/// Serves requests one at a time until input closes or a valid `plugin.shutdown` is answered.
pub async fn serve<R, W, H>(input: R, mut output: W, handler: &mut H) -> std::io::Result<()>
where
    R: AsyncBufRead + Unpin + Send + 'static,
    W: AsyncWrite + Unpin + Send + 'static,
    H: Handler,
{
    let (outgoing, mut frames) = mpsc::unbounded_channel::<String>();
    let writer = tokio::spawn(async move {
        while let Some(frame) = frames.recv().await {
            output.write_all(frame.as_bytes()).await?;
            output.write_all(b"\n").await?;
            output.flush().await?;
        }
        Ok::<_, std::io::Error>(())
    });
    let host = HostClient::new(outgoing.clone());
    let (requests_tx, mut requests) = mpsc::unbounded_channel::<InboundRequest>();
    let (cancel_tx, mut cancellations) = mpsc::unbounded_channel::<Value>();
    let reader_host = host.clone();
    let reader = tokio::spawn(async move {
        let mut input = input;
        loop {
            let line = match read_frame(&mut input).await {
                Ok(Some(Ok(line))) => line,
                Ok(Some(Err(error))) => {
                    eprintln!("linear ignored an invalid JSON-RPC frame: {error}");
                    continue;
                }
                Ok(None) => break,
                Err(error) => {
                    eprintln!("linear stopped reading JSON-RPC input: {error}");
                    break;
                }
            };
            match classify(&line) {
                Frame::Request {
                    jsonrpc_ok,
                    id,
                    method,
                    params,
                } => {
                    let _ = requests_tx.send(InboundRequest {
                        jsonrpc_ok,
                        id,
                        method,
                        params,
                    });
                }
                Frame::Notification { method, params } if method == "$/cancelRequest" => {
                    let _ = cancel_tx.send(params.get("id").cloned().unwrap_or(Value::Null));
                }
                Frame::Notification { method, .. } => {
                    eprintln!("linear ignored notification {method}")
                }
                Frame::Response { id, outcome } => {
                    if !reader_host.deliver(&id, outcome) {
                        eprintln!("linear ignored a response to unknown request {id}");
                    }
                }
                Frame::Invalid(error) => eprintln!("linear ignored {error}"),
            }
        }
        reader_host.fail_all();
    });

    while let Some(request) = requests.recv().await {
        if !is_scalar_id(&request.id) {
            let _ = outgoing.send(encode_response(
                Value::Null,
                Err(RpcError::new(
                    INVALID_REQUEST,
                    "expected a scalar JSON-RPC id",
                )),
            ));
            continue;
        }
        if !request.jsonrpc_ok {
            let _ = outgoing.send(encode_response(
                request.id,
                Err(RpcError::new(INVALID_REQUEST, "expected jsonrpc 2.0")),
            ));
            continue;
        }
        let shutdown = request.method == "plugin.shutdown";
        let outcome = {
            let work = handler.handle(&request.method, request.params, &host);
            tokio::pin!(work);
            loop {
                tokio::select! {
                    outcome = &mut work => break outcome,
                    Some(cancelled) = cancellations.recv() => {
                        if cancelled == request.id {
                            break Err(RpcError::new(CANCELLED, "request cancelled"));
                        }
                    }
                }
            }
        };
        let _ = outgoing.send(encode_response(request.id, outcome));
        if shutdown {
            break;
        }
    }
    reader.abort();
    drop(host);
    drop(outgoing);
    writer.await.map_err(std::io::Error::other)?
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};

    struct Echo;

    impl Handler for Echo {
        async fn handle(
            &mut self,
            method: &str,
            params: Value,
            host: &HostClient,
        ) -> Result<Value, RpcError> {
            match method {
                "plugin.shutdown" => Ok(json!({ "stopping": true })),
                "echo" => Ok(params),
                "ask.host" => host.call("host.task.get", json!({ "key": "ENG-1" })).await,
                "hang" => std::future::pending().await,
                "big" => Ok(json!({ "data": "x".repeat(MAX_FRAME_BYTES) })),
                _ => Err(RpcError::new(METHOD_NOT_FOUND, "method not found")),
            }
        }
    }

    struct Pipe {
        to_plugin: tokio::io::WriteHalf<tokio::io::DuplexStream>,
        from_plugin: tokio::io::Lines<BufReader<tokio::io::ReadHalf<tokio::io::DuplexStream>>>,
        served: tokio::task::JoinHandle<std::io::Result<()>>,
    }

    impl Pipe {
        fn start() -> Self {
            let (plugin, test) = tokio::io::duplex(1 << 20);
            let (plugin_read, plugin_write) = tokio::io::split(plugin);
            let (test_read, test_write) = tokio::io::split(test);
            let served = tokio::spawn(async move {
                serve(BufReader::new(plugin_read), plugin_write, &mut Echo).await
            });
            Self {
                to_plugin: test_write,
                from_plugin: BufReader::new(test_read).lines(),
                served,
            }
        }

        async fn send(&mut self, frame: &str) {
            self.to_plugin
                .write_all(format!("{frame}\n").as_bytes())
                .await
                .unwrap();
        }

        async fn next(&mut self) -> Value {
            let line = self.from_plugin.next_line().await.unwrap().unwrap();
            serde_json::from_str(&line).unwrap()
        }
    }

    #[tokio::test]
    async fn host_callback_uses_a_correlated_nested_json_rpc_request() {
        let mut pipe = Pipe::start();
        pipe.send(r#"{"jsonrpc":"2.0","id":7,"method":"ask.host"}"#)
            .await;
        let nested = pipe.next().await;
        assert_eq!(
            nested,
            json!({ "jsonrpc": "2.0", "id": "plugin:1", "method": "host.task.get", "params": { "key": "ENG-1" } })
        );
        pipe.send(r#"{"jsonrpc":"2.0","id":"plugin:999","result":{"stale":true}}"#)
            .await;
        pipe.send(r#"{"jsonrpc":"2.0","id":"plugin:1","result":{"task":null}}"#)
            .await;
        assert_eq!(
            pipe.next().await,
            json!({ "jsonrpc": "2.0", "id": 7, "result": { "task": null } })
        );
    }

    #[tokio::test]
    async fn host_errors_reach_the_caller_with_their_code() {
        let mut pipe = Pipe::start();
        pipe.send(r#"{"jsonrpc":"2.0","id":1,"method":"ask.host"}"#)
            .await;
        pipe.next().await;
        pipe.send(
            r#"{"jsonrpc":"2.0","id":"plugin:1","error":{"code":-32003,"message":"not granted"}}"#,
        )
        .await;
        assert_eq!(
            pipe.next().await,
            json!({ "jsonrpc": "2.0", "id": 1, "error": { "code": -32003, "message": "not granted" } })
        );
    }

    #[tokio::test]
    async fn cancel_request_answers_the_original_request_with_32800() {
        let mut pipe = Pipe::start();
        pipe.send(r#"{"jsonrpc":"2.0","id":3,"method":"hang"}"#)
            .await;
        pipe.send(r#"{"jsonrpc":"2.0","method":"$/cancelRequest","params":{"id":2}}"#)
            .await;
        pipe.send(r#"{"jsonrpc":"2.0","method":"$/cancelRequest","params":{"id":3}}"#)
            .await;
        assert_eq!(
            pipe.next().await,
            json!({ "jsonrpc": "2.0", "id": 3, "error": { "code": -32800, "message": "request cancelled" } })
        );
        pipe.send(r#"{"jsonrpc":"2.0","id":4,"method":"echo","params":"after"}"#)
            .await;
        assert_eq!(
            pipe.next().await,
            json!({ "jsonrpc": "2.0", "id": 4, "result": "after" })
        );
    }

    #[tokio::test]
    async fn non_scalar_ids_and_wrong_versions_are_rejected() {
        let mut pipe = Pipe::start();
        pipe.send(r#"{"jsonrpc":"2.0","id":["x"],"method":"plugin.shutdown"}"#)
            .await;
        assert_eq!(
            pipe.next().await,
            json!({ "jsonrpc": "2.0", "id": null, "error": { "code": -32600, "message": "expected a scalar JSON-RPC id" } })
        );
        pipe.send(r#"{"jsonrpc":"1.0","id":2,"method":"echo"}"#)
            .await;
        assert_eq!(
            pipe.next().await,
            json!({ "jsonrpc": "2.0", "id": 2, "error": { "code": -32600, "message": "expected jsonrpc 2.0" } })
        );
        pipe.send("not json").await;
        pipe.send(r#"{"jsonrpc":"2.0","id":3,"method":"echo","params":1}"#)
            .await;
        assert_eq!(
            pipe.next().await,
            json!({ "jsonrpc": "2.0", "id": 3, "result": 1 })
        );
    }

    #[tokio::test]
    async fn malformed_shutdown_request_does_not_terminate_the_plugin_but_a_valid_one_does() {
        let mut pipe = Pipe::start();
        pipe.send(r#"{"jsonrpc":"2.0","id":{"x":1},"method":"plugin.shutdown"}"#)
            .await;
        assert_eq!(pipe.next().await["error"]["code"], -32600);
        pipe.send(r#"{"jsonrpc":"2.0","id":2,"method":"plugin.shutdown"}"#)
            .await;
        assert_eq!(
            pipe.next().await,
            json!({ "jsonrpc": "2.0", "id": 2, "result": { "stopping": true } })
        );
        pipe.served.await.unwrap().unwrap();
    }

    #[tokio::test]
    async fn oversized_input_frames_are_skipped() {
        let mut pipe = Pipe::start();
        let big = format!(
            r#"{{"jsonrpc":"2.0","id":1,"method":"echo","params":"{}"}}"#,
            "x".repeat(MAX_FRAME_BYTES)
        );
        pipe.send(&big).await;
        pipe.send(r#"{"jsonrpc":"2.0","id":2,"method":"echo","params":"small"}"#)
            .await;
        assert_eq!(
            pipe.next().await,
            json!({ "jsonrpc": "2.0", "id": 2, "result": "small" })
        );
    }

    #[tokio::test]
    async fn oversized_responses_are_replaced_with_a_bounded_error_frame() {
        let mut pipe = Pipe::start();
        pipe.send(r#"{"jsonrpc":"2.0","id":1,"method":"big"}"#)
            .await;
        assert_eq!(
            pipe.next().await,
            json!({ "jsonrpc": "2.0", "id": 1, "error": { "code": -32000, "message": "plugin response exceeded the maximum frame size" } })
        );
    }

    #[test]
    fn oversized_response_fallback_bounds_a_long_scalar_id() {
        let frame = encode_response(
            json!("x".repeat(MAX_FRAME_BYTES)),
            Ok(json!({ "data": "x".repeat(MAX_FRAME_BYTES) })),
        );
        assert!(frame.len() < MAX_FRAME_BYTES);
        assert_eq!(
            serde_json::from_str::<Value>(&frame).unwrap(),
            json!({ "jsonrpc": "2.0", "id": null, "error": { "code": -32000, "message": "plugin response exceeded the maximum frame size" } })
        );
    }

    #[tokio::test]
    async fn closing_input_fails_a_pending_host_call() {
        let mut pipe = Pipe::start();
        pipe.send(r#"{"jsonrpc":"2.0","id":1,"method":"ask.host"}"#)
            .await;
        pipe.next().await;
        pipe.to_plugin.shutdown().await.unwrap();
        assert_eq!(
            pipe.next().await,
            json!({ "jsonrpc": "2.0", "id": 1, "error": { "code": -32000, "message": "host closed plugin RPC input" } })
        );
    }
}
