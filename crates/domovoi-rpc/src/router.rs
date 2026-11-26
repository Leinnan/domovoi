//! The methods a connection can call, each registered with its own input,
//! output and error types.

use crate::message::Request;
use futures::stream::{BoxStream, StreamExt};
use serde::{Serialize, de::DeserializeOwned};
use serde_json::Value;
use std::collections::HashMap;
use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;

pub type BoxFuture<T> = Pin<Box<dyn Future<Output = T> + Send + 'static>>;

/// How a request failed: with an error the method declares (`Fail`), or
/// with one it does not (`Die`: a bug, or a request it could not read).
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

/// An answer as it is sent: its JSON text. It is written from the handler's
/// own type once, and shared, so that what goes to every subscriber (a live
/// event) is written for all of them at once.
pub type Encoded = Arc<serde_json::value::RawValue>;

/// `value` as it is sent.
pub fn encoded<O: Serialize + ?Sized>(value: &O) -> Result<Encoded, serde_json::Error> {
    serde_json::value::to_raw_value(value).map(Arc::from)
}

pub enum Reply {
    Unary(BoxFuture<Result<Encoded, Failure>>),
    Stream(BoxStream<'static, Result<Encoded, Failure>>),
}

/// What a guard and a raw handler are told of a request besides its payload.
#[derive(Debug, Clone)]
pub struct RequestMeta {
    pub tag: String,
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

fn encode<O: Serialize + 'static>(value: O) -> Result<Encoded, Failure> {
    // A handler that answers with text already written (one event for every
    // subscriber) has it passed on.
    if let Some(written) = (&value as &dyn std::any::Any).downcast_ref::<Encoded>() {
        return Ok(written.clone());
    }
    encoded(&value).map_err(|e| Failure::die(format!("failed to encode success: {e}")))
}

impl<C: Send + Sync + 'static> Router<C> {
    pub fn new() -> Self {
        Self::default()
    }

    /// Runs before every handler: whether the caller may call the method.
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

    /// Calls of these tags get no span.
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
    pub fn dispatch(&self, ctx: Arc<C>, request: &mut Request) -> Result<Reply, Failure> {
        let meta = RequestMeta { tag: request.tag.clone() };
        let Some(handler) = self.handlers.get(&request.tag) else {
            return Err(Failure::die(format!("Unknown request tag: {}", request.tag)));
        };
        if let Some(guard) = &self.guard {
            guard(&ctx, &meta, &request.payload)?;
        }
        handler(ctx, meta, std::mem::take(&mut request.payload))
    }
}
