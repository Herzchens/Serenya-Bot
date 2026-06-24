# Serenya Bot

Serenya Bot is a Rust-based Discord music bot built for stable playback, low latency, and multi-guild operation. It uses `poise` and `serenity` for Discord commands, `songbird` for voice, FFmpeg for the audio pipeline, and a native Rust YouTube resolver to reduce reliance on Python `yt-dlp` for YouTube playback.

## Current Project Status

- Main crate version: `serenya` `1.1.1` (Optimized version over 1.1.0 workspace ytdl resolver).
- Rust edition: `2024`.
- Declared Rust toolchain: `rust-version = "1.96.0"`.
- Workspace members:
  - `serenya`: the main Discord bot.
  - `crates/youtube_resolver`: the native Rust YouTube stream resolver.
  - `crates/rusty-ytdl`: the internal Rust YouTube parsing foundation used by the native resolver.

## Key Features

- Slash commands and prefix commands.
- Per-guild queue, player state, settings, and playlists.
- Playback from YouTube, YouTube Music, Spotify, Deezer, Apple Music, SoundCloud, and direct audio URLs.
- Spotify track, playlist, album, and artist top-track imports.
- User-owned playlists stored in `database.yml`.
- Queue controls: skip, previous, jump, move, remove, clear, shuffle, and loop.
- Playback controls: seek, forward, rewind, and replay.
- Optional 8D audio effect through FFmpeg.
- Per-guild audio quality setting.
- Per-guild now-playing announcements.
- Metadata cache, stream cache, negative cache, resolver timeouts, and resolver concurrency limits.
- Graceful shutdown and atomic database writes through `.tmp` and `.bak` files.
- Typed domain errors through `SerenyaError`, converted to boxed framework errors only at command boundaries when needed.

## Performance & Optimizations (v1.1.1)

Version 1.1.1 introduces significant performance optimizations and architectural improvements over the v1.1.0 baseline:
- **Zero Deadlocks**: Safely resolved deadlocks caused by concurrent `DashMap` read locks in the monitor and statistics threads. 
- **Non-blocking Execution**: Eliminated `tokio` runtime thread-blocking during `ffmpeg` child process spawning using `tokio::task::spawn_blocking`, and moved synchronous startup initialization tasks (like `configure_path`) out of the main async context.
- **Lock-free Data Access**: Integrated `arc-swap` to eliminate `RwLock` bottlenecks when querying dynamic runtime and Spotify configuration settings.
- **Memory & Allocation Efficiency**: Upgraded the `Track` core data structure from `String` allocations to lightweight `Option<String>` and `Arc<ResolvedStream>` patterns to drastically reduce allocation overhead on metadata resolution operations.
- **O(N) Secret Redaction**: Replaced costly sequential `replace()` calls in the logging module with the `aho-corasick` crate, delivering optimal time-complexity when filtering logging secrets.

## Native YouTube Resolver

Serenya includes the internal `youtube_resolver` crate as the native replacement layer for YouTube stream resolution.

### What The Resolver Does

- Calls the Innertube player API through anonymous clients.
- Rotates clients in this order:
  1. `ANDROID_VR`
  2. `WEB_SAFARI`
  3. `IOS`
  4. `ANDROID`
  5. `TVHTML5`
- Implements a custom `format_selector` for selecting audio formats.
- Prefers audio-only, non-DRM WebM Opus before M4A/AAC.
- Uses `boa_engine` in `js_solver` to handle signature deciphering and `n`-parameter throttling when required.
- Uses a SHA-1 hash of `player_url` as the cache-key prefix to avoid collisions between different `base.js` versions.
- Uses `stream_probe` to range-read a stream before playback and catch HTTP 403 responses or throttled streams early.
- Returns `ResolvedStream` metadata, including URL, client kind, user agent, MIME type, bitrate, and resolve source so FFmpeg can use stream-specific headers.

### What The Resolver Does Not Do

- It does not use PO tokens.
- It does not use YouTube cookies.
- It does not send `serviceIntegrityDimensions`.
- It does not fall back to Python `yt-dlp` for YouTube stream playback.

`yt-dlp` still exists in the project for the installer and for non-YouTube URL fallback. The YouTube playback path intentionally uses the native resolver and direct public stream fallback instead of Python `yt-dlp`.

## Audio Architecture

```text
User command
  -> audio::resolver
  -> metadata providers and ranking
  -> Track queue
  -> audio::source
  -> youtube_resolver / SoundCloud native / non-YouTube yt-dlp fallback
  -> FFmpeg input with stream-specific headers
  -> Songbird voice playback
```

Important modules:

- `src/audio/resolver.rs`: handles user input, playlists, URLs, search, and metadata mirroring to playable sources.
- `src/audio/providers.rs`: metadata providers for Spotify, Deezer, Apple Music, YouTube, SoundCloud, and related flows.
- `src/audio/source.rs`: stream URL resolution, stream caching, and FFmpeg input creation.
- `src/audio/runtime.rs`: resolver timeouts, semaphores, negative cache, and degraded mode.
- `crates/youtube_resolver/src`: native YouTube resolver implementation.
- `src/core/queue.rs` and `src/core/track.rs`: queue and track models.
- `src/database`: YAML persistence.
- `src/commands`: slash and prefix command handlers.

## Requirements

### Required

- A Rust toolchain compatible with the `rust-version` declared in `Cargo.toml`.
- Visual Studio Build Tools on Windows, or GCC/Clang on Linux.
- CMake.
- FFmpeg in `PATH`, or an FFmpeg binary next to the bot executable.
- A Discord bot token.

### Recommended

- `yt-dlp` in `PATH` if you want fallback support for some non-YouTube URLs.
- A secondary Spotify account for `sp_dc` if you use Spotify playlist, album, or artist imports.

## Configuration

Copy the example config:

```powershell
Copy-Item config.example.yml config.yml
```

On Linux or macOS:

```bash
cp config.example.yml config.yml
```

Main config sections:

- `bot`: token, prefix, owner ID, instance ID, and display name.
- `logging`: log level and webhook logging.
- `spotify`: Spotify provider flags, `sp_dc`, market, and import limits.
- `playback`: queue limits, playlist limits, and announcement settings.
- `resolver`: timeouts, concurrency limits, cache TTLs, and ranking thresholds.
- `emojis`: custom embed emojis.

### Tokens And Secrets

`config.yml` contains real secrets and must not be committed. The repository ignores `config.yml`, `database.yml`, `document/`, and `rule.md`.

Do not share:

- Discord bot token.
- Spotify `sp_dc`.
- Webhook URL.
- `database.yml` if it contains real server or user data.

### Getting Spotify `sp_dc`

Spotify imports use the Web Player `sp_dc` cookie to obtain richer metadata.

1. Open Chrome or Firefox in a secondary profile or private window.
2. Open [Spotify Web Player](https://open.spotify.com/) and sign in.
3. Open Developer Tools.
4. Go to the Application or Storage tab.
5. Select Cookies for `https://open.spotify.com`.
6. Find the cookie named `sp_dc`.
7. Copy its value into `spotify.sp_dc` in `config.yml`.

Use a secondary account where possible. This cookie grants access to your Spotify Web session.

## Running From Source

> [!WARNING]
> Pre-built release binaries are compiled with `-C target-cpu=native` to maximize performance (accelerating audio decoding and JS execution).
> If a downloaded release binary crashes or fails to run on your system (e.g., triggering `Illegal Instruction` or `core dumped`), please clone the repository and build the binary yourself on the target machine:
> ```bash
> cargo build --release
> # Or compile with Profile-Guided Optimization (PGO) for maximum performance (see the PGO section below)
> ```

```powershell
cargo run --release
```

Build a release binary:

```powershell
cargo build --release
```

The Windows binary is generated at:

```text
target/release/serenya.exe
```

On Linux:

```bash
cargo run --release
```

If FFmpeg or `yt-dlp` is missing from `PATH`, the runtime installer will try to download a matching binary. For production servers, installing dependencies through the system package manager is usually easier to maintain.

## Profile-Guided Optimization (PGO)

To achieve the highest possible runtime performance, Serenya-Bot supports **Profile-Guided Optimization (PGO)** with target CPU optimizations. This process profiles the application under a realistic workload to optimize hot execution paths (such as audio decoding, JS signature solver engine, and JSON parsing).

### Prerequisites
Make sure you have the `llvm-tools` component installed via `rustup` since it is required to merge profiling data:
```powershell
rustup component add llvm-tools
```

### Windows (PowerShell)
Run the interactive script:
```powershell
.\build-pgo.ps1
```
1. The script compiles an instrumented version of the bot.
2. Start the bot (`.\target\pgo-gen\serenya.exe`) and interact with it (play some tracks, search songs, etc.) to gather profiling logs.
3. Close the bot, go back to the PowerShell window, and press **Enter** to continue.
4. The script merges the logs and compiles the final optimized binary at `.\target\pgo-use\serenya.exe`.

### Linux (Bash)
Make the build script executable and run it:
```bash
chmod +x build-pgo.sh
./build-pgo.sh
```
1. The script builds the instrumented version.
2. Start the bot (`./target/pgo-gen/serenya`) and execute music playback commands in Discord to gather real runtime profiles.
3. Stop the bot, go back to the terminal, and press **Enter** to merge profiles and compile the final optimized binary at `./target/pgo-use/serenya`.

## Discord Commands

### Playback

| Command | Description |
| --- | --- |
| `/play <query>` | Plays a song, URL, personal playlist, Spotify playlist, or Spotify album. |
| `/pause` | Pauses the current track. |
| `/resume` | Resumes playback. |
| `/stop` | Stops playback and clears the queue. |
| `/skip` | Skips the current track. |
| `/previous` | Plays the previous track. |
| `/seek <time>` | Seeks to a specific timestamp. |
| `/forward <seconds>` | Moves playback forward. |
| `/rewind <seconds>` | Moves playback backward. |
| `/replay` | Restarts the current track. |
| `/join` | Makes the bot join your voice channel. |
| `/leave` | Makes the bot leave the voice channel. |

### Queue

| Command | Description |
| --- | --- |
| `/queue` | Shows the current queue with pagination. |
| `/jump <position>` | Jumps to a queue position. |
| `/remove <position>` | Removes a track by queue position. |
| `/move <from> <to>` | Moves a track inside the queue. |
| `/clear` | Clears queued tracks. |
| `/shuffle` | Shuffles upcoming tracks. |
| `/loop <mode>` | Sets loop mode: off, track, or queue. |

### Search And Info

| Command | Description |
| --- | --- |
| `/search <query>` | Searches providers and shows a selectable result menu. |
| `/nowplaying` | Shows the current track. |
| `/songinfo` | Shows detailed metadata for the current track. |
| `/lyrics [query]` | Finds lyrics for the current song or a custom query. |

### Personal Playlists

| Command | Description |
| --- | --- |
| `/playlist create <name>` | Creates a personal playlist. |
| `/playlist add <name> <query>` | Resolves a query or URL and adds it to a playlist. |
| `/playlist play <name>` | Enqueues the entire playlist. |
| `/playlist list` | Lists your playlists. |
| `/playlist remove <name> <position>` | Removes a track from a playlist. |
| `/playlist rename <old> <new>` | Renames a playlist. |
| `/playlist delete <name>` | Deletes a playlist. |

### Settings And Utilities

| Command | Description |
| --- | --- |
| `/8d <on/off>` | Toggles the 8D effect for the guild. |
| `/quality <mode>` | Changes audio quality. |
| `/announce_track <on/off>` | Toggles now-playing announcements. |
| `/prefix <new_prefix>` | Changes the guild prefix. |
| `/cleanup` | Resets player state if the bot gets stuck. |
| `/stats` | Shows runtime statistics. |
| `/ping` | Checks latency. |
| `/about` | Shows bot information. |
| `/help` | Shows command help. |
| `/invite` | Shows the bot invite link. |
| `/support` | Shows support information. |
| `/reload` | Reloads config. Owner only. |

## Testing

Recommended gates before pushing:

```powershell
cargo fmt --check
cargo check
cargo clippy -- -D warnings
cargo test
cargo test --workspace
```

Native YouTube resolver tests:

```powershell
cargo test --package youtube_resolver --lib -- --nocapture
```

Spotify playlist to YouTube mirror to native stream probe integration test:

```powershell
cargo test --package serenya -- audio::resolver::tests::test_spotify_playlist_resolution --nocapture
```

The Spotify integration test needs stable network access and valid Spotify configuration if the active test path needs a session cookie.

## Troubleshooting

### The Bot Does Not Join Voice Or Play Audio

- Check that the bot has Connect and Speak permissions in the voice channel.
- Check that FFmpeg is available in `PATH`.
- Check gateway intents and the bot token.
- Run `/cleanup` if guild player state is stuck.

### YouTube Streams Fail With 403

- The native resolver probes streams and rotates clients automatically.
- If a cached stream fails with 403, playback invalidates the cached stream.
- Do not add PO tokens or YouTube cookies. This project intentionally avoids both approaches.

### Spotify Playlist Import Fails

- Check `spotify.enabled` and `spotify.enable_playlist`.
- Check that `spotify.sp_dc` is still valid.
- Lower `spotify.max_playlist_import` for very large playlists.
- Public Spotify editorial/system playlists may use the embed fallback path.

### Search Is Slow

- Review timeout values in the `resolver` config section.
- Disable providers you do not need on weak networks or small VPS instances.
- Check DNS and outbound HTTPS connectivity.

## Thanks And References

Serenya builds on ideas, algorithms, and libraries from the Rust, Discord, and audio communities:

- **rusty-ytdl**: Special thanks to the child/fork repository [Herzchens/rusty_ytdl](https://github.com/Herzchens/rusty_ytdl). The workspace crate `crates/rusty-ytdl` is an important foundation for YouTube parsing.
- **yt-dlp**: Thanks to the [yt-dlp](https://github.com/yt-dlp/yt-dlp) project for public research into YouTube stream extraction, signature deciphering, and player cipher edge cases.
- **Aho-Corasick**: The [aho-corasick](https://crates.io/crates/aho-corasick) algorithm is used for optimal \(O(N)\) multi-pattern log redaction.
- **ArcSwap**: The [arc-swap](https://crates.io/crates/arc-swap) utility is used for lock-free fast RCU (Read-Copy-Update) configuration updates.
- Thanks to the developers of `serenity`, `poise`, `songbird`, `tokio`, `reqwest`, `boa_engine`, `moka`, and `tracing`.

## License

See [LICENSE](LICENSE).
