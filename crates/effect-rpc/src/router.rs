//! Typed handler registration for an RPC group (the Rust analogue of
//! `RpcGroup.toLayer({...handlers})`).

use crate::message::RequestEncoded;
use futures::stream::{BoxStream, StreamExt};
use serde::{Serialize, de::DeserializeOwned};
use serde_json::Value;
use std::collections::HashMap;
use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;

pub type BoxFuture<T> = Pin<Box<dyn Future<Output = T> + Send + 'static>>;

/// How a request failed: an expected, schema-encoded error (`Cause.fail`) or a
/// defect (`Cause.die`).
#[derive(Debug, Clone, PartialEq)]
pub enum Failure {
    Fail(Value),
    Die(Value),
}

impl Failure {
    pub fn die(message: impl Into<String>) -> Self {
        Failure::Die(Value::String(message.into()))
    }
    pub fn fail<E: Serialize>(error: &E) -> Self {
        match serde_json::to_value(error) {
            Ok(v) => Failure::Fail(v),
            Err(e) => Failure::die(format!("failed to encode error: {e}")),
        }
    }
}

pub enum Reply {
    Unary(BoxFuture<Result<Value, Failure>>),
    Stream(BoxStream<'static, Result<Value, Failure>>),
}

/// Request metadata available to handlers besides the decoded payload.
#[derive(Debug, Clone)]
pub struct RequestMeta {
    pub tag: String,
    pub headers: Vec<(String, String)>,
}

type HandlerFn<C> = Arc<dyn Fn(Arc<C>, RequestMeta, Value) -> Result<Reply, Failure> + Send + Sync>;
type GuardFn<C> = Arc<dyn Fn(&C, &RequestMeta, &Value) -> Result<(), Failure> + Send + Sync>;

/// A set of RPC handlers sharing a per-connection context `C`.
pub struct Router<C> {
    handlers: HashMap<String, HandlerFn<C>>,
    guard: Option<GuardFn<C>>,
    /// Tags whose calls get no `ws.rpc.<tag>` span.
    untraced: std::collections::HashSet<String>,
    /// Tags registered with [`Router::stream`].
    streams: std::collections::HashSet<String>,
}

impl<C> Default for Router<C> {
    fn default() -> Self {
        Self {
            handlers: HashMap::new(),
            guard: None,
            untraced: Default::default(),
            streams: Default::default(),
        }
    }
}

fn decode<I: DeserializeOwned>(tag: &str, payload: Value) -> Result<I, Failure> {
    serde_json::from_value(payload)
        .map_err(|e| Failure::die(format!("Invalid payload for {tag}: {e}")))
}

fn encode<O: Serialize + 'static>(value: O) -> Result<Value, Failure> {
    // Most handlers already answer with a `Value`: pass it on instead of
    // serializing it into a copy of itself.
    match (Box::new(value) as Box<dyn std::any::Any>).downcast::<Value>() {
        Ok(v) => Ok(*v),
        Err(other) => {
            let value = other.downcast::<O>().expect("the value is an O");
            serde_json::to_value(*value).map_err(|e| Failure::die(format!("failed to encode success: {e}")))
        }
    }
}

impl<C: Send + Sync + 'static> Router<C> {
    pub fn new() -> Self {
        Self::default()
    }

    /// Runs before every handler (e.g. t3code's `RpcScopeAuthorization` middleware).
    pub fn guard(
        mut self,
        f: impl Fn(&C, &RequestMeta, &Value) -> Result<(), Failure> + Send + Sync + 'static,
    ) -> Self {
        self.guard = Some(Arc::new(f));
        self
    }

    pub fn unary<I, O, E, F, Fut>(mut self, tag: &str, f: F) -> Self
    where
        I: DeserializeOwned + Send + 'static,
        O: Serialize + Send + 'static,
        E: Serialize + Send + 'static,
        F: Fn(Arc<C>, I) -> Fut + Send + Sync + 'static,
        Fut: Future<Output = Result<O, E>> + Send + 'static,
    {
        let f = Arc::new(f);
        let handler: HandlerFn<C> = Arc::new(move |ctx, meta, payload| {
            let input: I = decode(&meta.tag, payload)?;
            let fut = f(ctx, input);
            Ok(Reply::Unary(Box::pin(async move {
                match fut.await {
                    Ok(o) => encode(o),
                    Err(e) => Err(Failure::fail(&e)),
                }
            })))
        });
        self.insert(tag, handler)
    }

    pub fn stream<I, O, E, F, S>(mut self, tag: &str, f: F) -> Self
    where
        I: DeserializeOwned + Send + 'static,
        O: Serialize + Send + 'static,
        E: Serialize + Send + 'static,
        F: Fn(Arc<C>, I) -> S + Send + Sync + 'static,
        S: futures::Stream<Item = Result<O, E>> + Send + 'static,
    {
        let handler: HandlerFn<C> = Arc::new(move |ctx, meta, payload| {
            let input: I = decode(&meta.tag, payload)?;
            let s = f(ctx, input).map(|item| match item {
                Ok(o) => encode(o),
                Err(e) => Err(Failure::fail(&e)),
            });
            Ok(Reply::Stream(s.boxed()))
        });
        self.streams.insert(tag.to_owned());
        self.insert(tag, handler)
    }

    /// Registers a raw handler working on JSON values directly.
    pub fn raw(
        mut self,
        tag: &str,
        f: impl Fn(Arc<C>, RequestMeta, Value) -> Result<Reply, Failure> + Send + Sync + 'static,
    ) -> Self {
        let handler: HandlerFn<C> = Arc::new(f);
        self.insert(tag, handler)
    }

    fn insert(&mut self, tag: &str, handler: HandlerFn<C>) -> Self {
        assert!(
            self.handlers.insert(tag.to_owned(), handler).is_none(),
            "duplicate RPC handler for {tag}"
        );
        std::mem::take(self)
    }

    /// Calls of these tags get no span (t3code's `RPC_METHODS_WITH_TRACING_DISABLED`).
    pub fn untraced(mut self, tags: &[&str]) -> Self {
        self.untraced.extend(tags.iter().map(|t| (*t).to_owned()));
        self
    }

    /// Whether a call of `tag` runs in a `ws.rpc.<tag>` span.
    pub fn traced(&self, tag: &str) -> bool {
        !self.untraced.contains(tag)
    }

    /// Whether `tag` answers with a stream of items (it was registered with [`Router::stream`]).
    pub fn is_stream(&self, tag: &str) -> bool {
        self.streams.contains(tag)
    }

    pub fn has(&self, tag: &str) -> bool {
        self.handlers.contains_key(tag)
    }

    pub fn tags(&self) -> impl Iterator<Item = &str> {
        self.handlers.keys().map(String::as_str)
    }

    /// Starts the request's handler. Its payload is moved to the handler
    /// (left `null` in `request`).
    pub fn dispatch(&self, ctx: Arc<C>, request: &mut RequestEncoded) -> Result<Reply, Failure> {
        let meta = RequestMeta {
            tag: request.tag.clone(),
            headers: request.headers.clone(),
        };
        let Some(handler) = self.handlers.get(&request.tag) else {
            return Err(Failure::die(format!("Unknown request tag: {}", request.tag)));
        };
        if let Some(guard) = &self.guard {
            guard(&ctx, &meta, &request.payload)?;
        }
        handler(ctx, meta, std::mem::take(&mut request.payload))
    }
}
