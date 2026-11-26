//! Terminal RPCs backed by `crate::terminal::Terminals`.

use super::Never;
use crate::state::ConnCtx;
use crate::terminal::{Attachment, Failure, Grid, InteractiveViewer, Terminals};
use domovoi_rpc::Router;
use futures::StreamExt;
use futures::stream::BoxStream;
use serde::{Deserialize, Serialize};
use serde::de::IgnoredAny;
use std::collections::HashMap;
use std::sync::Arc;
use domovoi_model::typed::{TerminalAttachEvent, TerminalMetadataEvent};
use tokio::sync::broadcast::error::RecvError;

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct OpenInput {
    thread_id: String,
    terminal_id: String,
    cwd: String,
    cols: Option<u16>,
    rows: Option<u16>,
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

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct ReopenInput {
    thread_id: String,
    terminal_id: String,
    new_terminal_id: String,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct RenameInput {
    thread_id: String,
    terminal_id: String,
    label: String,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct TerminalActionError {
    message: String,
}

fn lookup_error(thread_id: &str, terminal_id: &str) -> Failure {
    Failure::SessionLookup { thread_id: thread_id.to_owned(), terminal_id: terminal_id.to_owned() }
}

/// Why the terminal `input` names did not open.
fn open_error(input: &OpenInput, e: crate::terminal::TerminalError) -> Failure {
    Failure::Cwd { thread_id: input.thread_id.clone(), terminal_id: input.terminal_id.clone(), cwd: input.cwd.clone(), reason: e.0 }
}

/// The terminal as it is, then what happens to it after that, until it is closed.
fn follow(terminals: Terminals, (snapshot, mut rx): Attachment, viewer: Option<InteractiveViewer>) -> BoxStream<'static, Result<TerminalAttachEvent, Failure>> {
    async_stream::stream! {
        let _viewer = viewer;
        let key = (snapshot.thread_id.clone(), snapshot.terminal_id.clone());
        let mut after = snapshot.sequence.unwrap_or(0);
        yield Ok(TerminalAttachEvent::Snapshot { snapshot: Box::new(snapshot) });
        loop {
            match rx.recv().await {
                Ok(ev) if ev.sequence().unwrap_or(0) <= after => {}
                Ok(ev) => {
                    let closed = matches!(ev, TerminalAttachEvent::Closed { .. });
                    yield Ok(ev);
                    if closed { return; }
                }
                Err(RecvError::Lagged(_)) => {
                    let Some((snapshot, receiver)) = terminals.recover(&key, &rx) else { return };
                    after = snapshot.sequence.unwrap_or(0);
                    rx = receiver;
                    yield Ok(TerminalAttachEvent::Snapshot { snapshot: Box::new(snapshot) });
                }
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
    once_read(move || {
        let attached = Grid::new(input.cols, input.rows).and_then(|grid| {
            let attachment = terminals.open_sized(&input.thread_id, &input.terminal_id, &input.cwd, &input.env, grid)?;
            terminals.attach_viewer(attachment, ctx.connection_id, grid)
        });
        match attached {
            Ok((attachment, viewer)) => follow(terminals, attachment, Some(viewer)),
            Err(e) => futures::stream::iter([Err(open_error(&input, e))]).boxed(),
        }
    })
}

/// `terminal.observe`: a running terminal's snapshot and output, read-only.
fn observe(ctx: Arc<ConnCtx>, input: TerminalRef) -> BoxStream<'static, Result<TerminalAttachEvent, Failure>> {
    let terminals = ctx.state.terminals.clone();
    once_read(move || match terminals.observe(&input.thread_id, &input.terminal_id) {
        Some(attachment) => follow(terminals, attachment, None),
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

fn details(ctx: Arc<ConnCtx>) -> BoxStream<'static, Result<Vec<crate::terminal::TerminalDetail>, Never>> {
    let terminals = ctx.state.terminals.clone();
    async_stream::stream! {
        let mut changes = terminals.details_updates();
        yield Ok(terminals.details());
        while changes.recv().await != Err(RecvError::Closed) {
            yield Ok(terminals.details());
        }
    }
    .boxed()
}

pub fn register(r: Router<ConnCtx>) -> Router<ConnCtx> {
    r.unary("terminal.open", |ctx: Arc<ConnCtx>, i: OpenInput| async move {
        Grid::new(i.cols, i.rows)
            .and_then(|grid| ctx.state.terminals.open_sized(&i.thread_id, &i.terminal_id, &i.cwd, &i.env, grid))
            .map(|(snapshot, _)| snapshot).map_err(|e| open_error(&i, e))
    })
    .stream("terminal.attach", attach)
    .stream("terminal.observe", observe)
    .unary("domovoi.terminals.reopen", |ctx: Arc<ConnCtx>, input: ReopenInput| async move {
        ctx.state.terminals.reopen(&input.thread_id, &input.terminal_id, &input.new_terminal_id)
            .map_err(|error| TerminalActionError { message: error.0 })
    })
    .unary("domovoi.terminals.rename", |ctx: Arc<ConnCtx>, input: RenameInput| async move {
        ctx.state.terminals.rename(&input.thread_id, &input.terminal_id, &input.label)
            .map_err(|error| TerminalActionError { message: error.0 })
    })
    .stream("domovoi.terminals.subscribe", |ctx, _: IgnoredAny| details(ctx))
    .stream("subscribeTerminalMetadata", |ctx, _: IgnoredAny| metadata(ctx))
    // Writes and resizes run in the handler's synchronous part, which the
    // connection calls in arrival order; inside the spawned future, two writes
    // (a paste, then Enter) could reach the PTY reordered.
    .unary("terminal.write", |ctx: Arc<ConnCtx>, i: WriteInput| {
        let r = ctx.state.terminals.write(&i.thread_id, &i.terminal_id, &i.data).map_err(|_| lookup_error(&i.thread_id, &i.terminal_id));
        async move { r }
    })
    .unary("terminal.resize", |ctx: Arc<ConnCtx>, i: ResizeInput| {
        let r = ctx.state.terminals.resize_viewer(&i.thread_id, &i.terminal_id, i.cols, i.rows, Some(ctx.connection_id)).map_err(|_| lookup_error(&i.thread_id, &i.terminal_id));
        async move { r }
    })
    .unary("terminal.clear", |ctx: Arc<ConnCtx>, i: TerminalRef| async move {
        ctx.state.terminals.clear(&i.thread_id, &i.terminal_id).map_err(|_| lookup_error(&i.thread_id, &i.terminal_id))
    })
    .unary("terminal.close", |ctx: Arc<ConnCtx>, i: TerminalRef| async move {
        ctx.state.terminals.close(&i.thread_id, &i.terminal_id).map_err(|_| lookup_error(&i.thread_id, &i.terminal_id))
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn lag_recovers_with_a_new_snapshot_and_does_not_follow_a_replacement() {
        let terminals = Terminals::default();
        let dir = std::env::temp_dir();
        let open = || terminals.open("thread", "terminal", dir.to_str().unwrap(), &HashMap::new()).unwrap();
        let mut stream = follow(terminals.clone(), open(), None);
        let Some(Ok(TerminalAttachEvent::Snapshot { snapshot })) = stream.next().await else {
            panic!("expected first snapshot");
        };
        // Generate enough ordered events to overrun the broadcast window
        // without relying on how a PTY splits writes into chunks.
        for _ in 0..1100 {
            terminals.clear("thread", "terminal").unwrap();
        }
        let Some(Ok(TerminalAttachEvent::Snapshot { snapshot: recovered })) = stream.next().await else {
            panic!("expected replacement snapshot");
        };
        assert!(recovered.sequence > snapshot.sequence);
        terminals.clear("thread", "terminal").unwrap();
        let event = stream.next().await.unwrap().unwrap();
        assert!(event.sequence() > recovered.sequence);
        for _ in 0..1100 {
            terminals.clear("thread", "terminal").unwrap();
        }
        terminals.close("thread", "terminal").unwrap();
        let _replacement = open();
        assert!(stream.next().await.is_none());
        terminals.shutdown();
    }
}
