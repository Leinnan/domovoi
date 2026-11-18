//! Terminal RPCs backed by `crate::terminal::Terminals`.

use super::Never;
use crate::state::ConnCtx;
use crate::terminal::Failure;
use effect_rpc::Router;
use futures::StreamExt;
use futures::stream::BoxStream;
use serde::Deserialize;
use serde::de::IgnoredAny;
use std::collections::HashMap;
use std::sync::Arc;
use domovoi_model::typed::{TerminalAttachEvent, TerminalMetadataEvent, TerminalSessionSnapshot};
use tokio::sync::broadcast;
use tokio::sync::broadcast::error::RecvError;

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct OpenInput {
    thread_id: String,
    terminal_id: String,
    cwd: String,
    #[serde(default)]
    env: HashMap<String, String>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct TerminalRef {
    thread_id: String,
    terminal_id: String,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct WriteInput {
    thread_id: String,
    terminal_id: String,
    data: String,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct ResizeInput {
    thread_id: String,
    terminal_id: String,
    cols: u16,
    rows: u16,
}

fn lookup_error(thread_id: &str, terminal_id: &str) -> Failure {
    Failure::SessionLookup { thread_id: thread_id.to_owned(), terminal_id: terminal_id.to_owned() }
}

/// Why the terminal `input` names did not open.
fn open_error(input: &OpenInput, e: crate::terminal::TerminalError) -> Failure {
    Failure::Cwd { thread_id: input.thread_id.clone(), terminal_id: input.terminal_id.clone(), cwd: input.cwd.clone(), reason: e.0 }
}

/// The terminal as it is, then what happens to it after that, until it is closed.
fn follow(snapshot: TerminalSessionSnapshot, mut rx: broadcast::Receiver<TerminalAttachEvent>) -> BoxStream<'static, Result<TerminalAttachEvent, Failure>> {
    async_stream::stream! {
        let after = snapshot.sequence.unwrap_or(0);
        yield Ok(TerminalAttachEvent::Snapshot { snapshot: Box::new(snapshot) });
        loop {
            match rx.recv().await {
                Ok(ev) if ev.sequence().unwrap_or(0) <= after => {}
                Ok(ev) => {
                    let closed = matches!(ev, TerminalAttachEvent::Closed { .. });
                    yield Ok(ev);
                    if closed { return; }
                }
                Err(RecvError::Lagged(_)) => continue,
                Err(RecvError::Closed) => return,
            }
        }
    }
    .boxed()
}

/// The stream `open` gives, opened when the stream is first read and not before.
fn once_read<T: Send + 'static>(open: impl FnOnce() -> BoxStream<'static, T> + Send + 'static) -> BoxStream<'static, T> {
    futures::stream::once(async move { open() }).flatten().boxed()
}

fn attach(ctx: Arc<ConnCtx>, input: OpenInput) -> BoxStream<'static, Result<TerminalAttachEvent, Failure>> {
    let terminals = ctx.state.terminals.clone();
    once_read(move || match terminals.open(&input.thread_id, &input.terminal_id, &input.cwd, &input.env) {
        Ok((snapshot, rx)) => follow(snapshot, rx),
        Err(e) => futures::stream::iter([Err(open_error(&input, e))]).boxed(),
    })
}

/// `terminal.observe`: a running terminal's snapshot and output, read-only.
fn observe(ctx: Arc<ConnCtx>, input: TerminalRef) -> BoxStream<'static, Result<TerminalAttachEvent, Failure>> {
    let terminals = ctx.state.terminals.clone();
    once_read(move || match terminals.observe(&input.thread_id, &input.terminal_id) {
        Some((snapshot, rx)) => follow(snapshot, rx),
        None => futures::stream::iter([Err(lookup_error(&input.thread_id, &input.terminal_id))]).boxed(),
    })
}

fn metadata(ctx: Arc<ConnCtx>) -> BoxStream<'static, Result<TerminalMetadataEvent, Never>> {
    let terminals = ctx.state.terminals.clone();
    async_stream::stream! {
        let mut rx = terminals.metadata_updates();
        yield Ok(TerminalMetadataEvent::Snapshot { terminals: terminals.summaries() });
        loop {
            match rx.recv().await {
                Ok(ev) => yield Ok(ev),
                Err(RecvError::Lagged(_)) => yield Ok(TerminalMetadataEvent::Snapshot { terminals: terminals.summaries() }),
                Err(RecvError::Closed) => return,
            }
        }
    }
    .boxed()
}

pub fn register(r: Router<ConnCtx>) -> Router<ConnCtx> {
    r.unary("terminal.open", |ctx: Arc<ConnCtx>, i: OpenInput| async move {
        ctx.state.terminals.open(&i.thread_id, &i.terminal_id, &i.cwd, &i.env).map(|(snapshot, _)| snapshot).map_err(|e| open_error(&i, e))
    })
    .stream("terminal.attach", attach)
    .stream("terminal.observe", observe)
    .stream("subscribeTerminalMetadata", |ctx, _: IgnoredAny| metadata(ctx))
    // Writes and resizes run in the handler's synchronous part, which the
    // connection calls in arrival order; inside the spawned future, two writes
    // (a paste, then Enter) could reach the PTY reordered.
    .unary("terminal.write", |ctx: Arc<ConnCtx>, i: WriteInput| {
        let r = ctx.state.terminals.write(&i.thread_id, &i.terminal_id, &i.data).map_err(|_| lookup_error(&i.thread_id, &i.terminal_id));
        async move { r }
    })
    .unary("terminal.resize", |ctx: Arc<ConnCtx>, i: ResizeInput| {
        let r = ctx.state.terminals.resize(&i.thread_id, &i.terminal_id, i.cols, i.rows).map_err(|_| lookup_error(&i.thread_id, &i.terminal_id));
        async move { r }
    })
    .unary("terminal.clear", |ctx: Arc<ConnCtx>, i: TerminalRef| async move {
        ctx.state.terminals.clear(&i.thread_id, &i.terminal_id).map_err(|_| lookup_error(&i.thread_id, &i.terminal_id))
    })
    .unary("terminal.close", |ctx: Arc<ConnCtx>, i: TerminalRef| async move {
        ctx.state.terminals.close(&i.thread_id, &i.terminal_id).map_err(|_| lookup_error(&i.thread_id, &i.terminal_id))
    })
}
