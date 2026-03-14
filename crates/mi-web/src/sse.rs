use axum::extract::State;
use axum::response::sse::{Event, Sse};
use futures_core::Stream;
use mi_core::MiningStats;
use std::convert::Infallible;
use std::sync::Arc;
use std::time::Duration;
use tokio_stream::wrappers::IntervalStream;
use tokio_stream::StreamExt;

/// SSE endpoint that pushes stats every second.
pub async fn stats_stream(
    State(stats): State<Arc<MiningStats>>,
) -> Sse<impl Stream<Item = Result<Event, Infallible>>> {
    let stream = IntervalStream::new(tokio::time::interval(Duration::from_secs(2))).map(
        move |_| {
            let snapshot = stats.snapshot();
            let json = serde_json::to_string(&snapshot).unwrap_or_default();
            Ok(Event::default().data(json))
        },
    );

    Sse::new(stream).keep_alive(
        axum::response::sse::KeepAlive::new()
            .interval(Duration::from_secs(15))
            .text("ping"),
    )
}
