use axum::extract::State;
use axum::response::sse::{Event, Sse};
use crate::routes::AppState;
use futures_core::Stream;
use std::convert::Infallible;
use std::time::Duration;
use tokio_stream::wrappers::IntervalStream;
use tokio_stream::StreamExt;

/// SSE endpoint that pushes stats every 2 seconds.
pub async fn stats_stream(
    State(state): State<AppState>,
) -> Sse<impl Stream<Item = Result<Event, Infallible>>> {
    let stream = IntervalStream::new(tokio::time::interval(Duration::from_secs(2))).map(
        move |_| {
            let snapshot = state.stats.snapshot();
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
