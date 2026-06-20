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
- `ffmpeg` for seek restart and optional 8D audio mode.
- `yt-dlp` is currently disabled in the runtime while native Rust and lightweight resolver paths are evaluated.

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
  max_queue_size: 1000
  max_playlist_import: 500
  max_user_playlists: 25
  max_tracks_per_user_playlist: 500

spotify:
  enabled: true
  sp_dc: ${SPOTIFY_SP_DC}
  enable_track: true
  enable_playlist: true
  enable_album: true
  enable_artist_top_tracks: true
  enable_text_search: true
  max_playlist_import: 500
  max_album_import: 500
  max_artist_top_tracks: 50
  market: US

audio:
  default_quality: balanced
  modes:
    - performance
    - balanced
    - quality

resolver:
  max_concurrent_ytdlp: 2
  max_concurrent_resolves_per_guild: 1
  global_search_timeout_ms: 1800
  deezer_timeout_ms: 800
  apple_music_timeout_ms: 800
  youtube_music_timeout_ms: 1000
  soundcloud_timeout_ms: 1500
  spotify_timeout_ms: 800
  youtube_timeout_ms: 1500
  ytsearch_timeout_ms: 3000
  yt_dlp_timeout_seconds: 10
  prefetch_timeout_seconds: 8
  prefetch_when_remaining_seconds: 10
  query_cache_ttl_seconds: 3600
  metadata_cache_ttl_seconds: 86400
  stream_cache_ttl_seconds: 3600
  negative_cache_ttl_seconds: 1800
  auto_pick_threshold: 0.90
  perfect_threshold: 0.97
```

Ensure your `DISCORD_TOKEN` environment variable is set.

Spotify imports use an `sp_dc` Web Player session cookie to request a user-scoped Spotify Web API token, then page through playlists/albums until the configured cap is reached or Spotify reports no next page. Keep the real cookie out of Git; use a local `config.yml` value or an environment-substituted secret. Text search shows a user-selectable provider list instead of auto-picking, ordered as Deezer, Apple Music, Spotify, SoundCloud, then YouTube fallbacks.

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
