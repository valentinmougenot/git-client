# Dedicated thread for git2 operations

All git2 calls run in a single dedicated background thread (the Git Worker) that communicates with the iced UI via two `std::sync::mpsc` channels: the UI sends `GitCommand` values to the worker; the worker sends `GitEvent` values back, consumed by an iced `Subscription`.

The alternative was to use `iced::Task` with `tokio::task::spawn_blocking`. We rejected this because git2's SSH callbacks are `!Send` — they capture `&mut Callbacks` which cannot safely cross thread boundaries in the way tokio's work-stealing scheduler requires. A single pinned thread sidesteps this entirely. It also avoids adding tokio as an explicit dependency and keeps the threading model trivial to reason about: one operation runs at a time, in order.

The trade-off is that long operations (push, pull) block the worker for their duration. This is acceptable for v1 because the UI remains responsive (the iced runtime runs on the main thread, independent of the worker), and we show an in-progress indicator in the Status Bar while waiting.
