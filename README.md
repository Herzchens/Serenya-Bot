# Serenya Bot

Serenya is a Rust-based, multi-guild Discord music bot designed for high performance, safety, and stability.

## Features

- **Slash + Prefix Commands**: Support for modern Discord interaction.
- **Graceful Shutdown**: Safe teardown sequence via cancellation tokens to stop background tasks and write final database states to disk.
- **Atomic Persistence**: Thread-safe atomic file writes with `.tmp` and `.bak` backup strategies to prevent data corruption.
- **Strict Error Handling**: Custom typed domain errors (`SerenyaError`) propagated safely up to Poise boundaries.
- **Low Memory Footprint**: Tailored for minimal resource environments.

## Tech Stack

- **Runtime**: Rust (latest stable), Tokio (async runtime)
- **Discord Integration**: Poise + Serenity
- **Voice / Audio**: Songbird
- **Serialization**: Serde, serde-saphyr (YAML)
- **Logging**: Tracing

## Getting Started

### Prerequisites

- Rust 1.85+
- Visual Studio Build Tools (on Windows) or GCC/Clang (on Linux)
- CMake
- `yt-dlp` and `ffmpeg` (for audio playback in future phases)

### Configuration

Create a `config.yml` based on the template:

```yaml
bot:
  token: ${DISCORD_TOKEN}
  prefix: "s!"
  owner: 123456789012345678
  instance_id: "serenya-1"
  display_name: "Serenya"

playback:
  stay_in_voice: true
  announce_track: true
  max_queue_size: 500
  max_playlist_import: 100
  max_user_playlists: 25
  max_tracks_per_user_playlist: 500

audio:
  default_quality: balanced
  modes:
    - performance
    - balanced
    - quality
```

Ensure your `DISCORD_TOKEN` environment variable is set.

### Running the Bot

Run the bot using:

```bash
cargo run
```

### Verification & Testing

To run the test suite:

```bash
cargo test
```

To check formatting:

```bash
cargo fmt --check
```

To run clippy linting:

```bash
cargo clippy -- -D warnings
```