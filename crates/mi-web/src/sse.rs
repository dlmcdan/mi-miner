use axum::extract::State;
use axum::response::sse::{Event, Sse};
use crate::routes::AppState;
use futures_core::Stream;
use std::convert::Infallible;
use std::time::Duration;
use tokio_stream::wrappers::{BroadcastStream, IntervalStream};
use tokio_stream::StreamExt;

/// SSE endpoint that pushes stats every 2 seconds and block-found events in real time.
pub async fn stats_stream(
    State(state): State<AppState>,
) -> Sse<impl Stream<Item = Result<Event, Infallible>>> {
    let stats = state.stats.clone();

    // Stats stream: snapshot every 2 seconds
    let stats_stream = IntervalStream::new(tokio::time::interval(Duration::from_secs(2)))
        .map(move |_| {
            let snapshot = stats.snapshot();
            let json = serde_json::to_string(&snapshot).unwrap_or_default();
            Ok(Event::default().data(json))
        });

    // Block-found stream: fires on each block discovery
    let block_rx = state.block_events.subscribe();
    let block_stream = BroadcastStream::new(block_rx).filter_map(|result| match result {
        Ok(block_num) => Some(Ok(Event::default()
            .event("block_found")
            .data(block_num.to_string()))),
        Err(_) => None,
    });

    let merged = stats_stream.merge(block_stream);

    Sse::new(merged).keep_alive(
        axum::response::sse::KeepAlive::new()
            .interval(Duration::from_secs(15))
            .text("ping"),
    )
}
