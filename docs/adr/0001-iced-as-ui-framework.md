# iced as UI framework

We need a GUI framework for a Rust desktop app targeting Linux and macOS. We chose **iced** over the other main candidates.

**egui** (immediate mode) was the simplest option but its look is deliberately custom and hard to polish into something that feels native. **tauri** would have given us full web-stack UI flexibility but introduces a JS runtime, a separate frontend build pipeline, and IPC overhead between the webview and the Rust backend — too heavy for a tool where every interaction should feel instant. **slint** uses a declarative DSL that is expressive but adds a compile-time language boundary. **gtk4-rs** is well-supported on Linux but its macOS story is poor.

iced's Elm-inspired message-passing model maps naturally onto a git client's state machine (selected file → load diff → update panel), and it is pure Rust with no foreign runtime. The main trade-off is that iced's ecosystem is younger than egui's, so some widgets may need to be written from scratch.
