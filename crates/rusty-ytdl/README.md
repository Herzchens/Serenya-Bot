# <div align="center"> rusty_ytdl (Workspace Fork) </div>

<div align="center">

[![Released API docs](https://img.shields.io/badge/docs.rs-rusty__ytdl-C36241?style=for-the-badge&logo=docs.rs)](https://docs.rs/rusty_ytdl)

</div>

This project is a customized fork of [rusty_ytdl](https://github.com/Mithronn/rusty_ytdl) originally by Mithronn (I am not the original owner). It is modified to be integrated directly into the `Serenya-Bot` cargo workspace. Special thanks to the developers of [yt-dlp](https://github.com/yt-dlp/yt-dlp) for their decryption logic.

Youtube searching and downloading module written in **pure Rust**.
Download videos **blazing-fast** without getting stuck on Youtube download speed (Downloads 20MB video files in just 10 seconds!)

## Overview

- [Workspace Integration](#workspace-integration)
- [Features](#features)
- [Usage](#usage)
- [Limitations](#limitations)

## Workspace Integration

This version of `rusty_ytdl` has been tailored for integration into a multi-crate Rust workspace:
* **MSRV (Minimum Supported Rust Version):** `1.96.0` (as defined in [Cargo.toml](file:///C:/Users/Herzchen/Desktop/Serenya-Bot/crates/rusty-ytdl/Cargo.toml#L14)).
* **Workspace Dependency Sharing:** Leverages the root workspace manifest to share common dependencies (`tokio`, `reqwest`, `serde`, `regex`, etc.) using `{ workspace = true }`.

## Features

- Download live and non-live videos
- Search with query (Video, Playlist, Channel)
- Blocking and asynchronous API
- Proxy, IPv6, and cookie support on request
- Built-in FFmpeg audio and video filter apply support (Non-live videos only) [Example](examples/download_with_ffmpeg.rs)
- [CLI](https://crates.io/crates/rusty_ytdl-cli)

# Usage

```rust,ignore
use rusty_ytdl::Video;

#[tokio::main]
async fn main() {
  let video_url = "https://www.youtube.com/watch?v=FZ8BxMU3BYc"; // FZ8BxMU3BYc works too!
  let video = Video::new(video_url).unwrap();

  let stream = video.stream().await.unwrap();

  while let Some(chunk) = stream.chunk().await.unwrap() {
    // Do what you want with chunks
    println!("{:#?}", chunk);
  }

  // Or direct download to path
  let path = std::path::Path::new(r"test.mp3");

  video.download(path).await.unwrap();

  //
  // Or with options
  //

  let video_options = VideoOptions {
    quality: VideoQuality::Lowest,
    filter: VideoSearchOptions::Audio,
    ..Default::default()
  };

  let video = Video::new_with_options(video_url, video_options).unwrap();

  let stream = video.stream().await.unwrap();

  while let Some(chunk) = stream.chunk().await.unwrap() {
    // Do what you want with chunks
    println!("{:#?}", chunk);
  }

  // Or direct download to path
  let path = std::path::Path::new(r"test.mp3");

  video.download(path).await.unwrap();
}
```

or get only video informations

```rust,ignore
use rusty_ytdl::Video;
use rusty_ytdl::{choose_format, VideoOptions};

#[tokio::main]
async fn main() {
  let video_url = "https://www.youtube.com/watch?v=FZ8BxMU3BYc"; // FZ8BxMU3BYc works too!
  // Also works with live videos!!
  let video = Video::new(video_url).unwrap();

  let video_info = video.get_info().await.unwrap();
  println!("{:#?}", video_info);

  /*
  VideoInfo {
    dash_manifest_url: Option<String>,
    hls_manifest_url: Option<String>,
    video_details: VideoDetails,
    formats: Vec<VideoFormat>,
    related_videos: Vec<RelatedVideo>
  }
  */

  let video_options = VideoOptions {
    quality: VideoQuality::Lowest,
    filter: VideoSearchOptions::Audio,
      ..Default::default()
  };

  let format = choose_format(&video_info.unwrap().formats, &video_options);

  println!("{:#?}", format);

  // Or with options
  let video = Video::new_with_options(video_url, video_options.clone()).unwrap();

  let format = choose_format(&video_info.formats, &video_options);

  let video_info = video.get_info().await.unwrap();

  println!("{:#?}", video_info);
}
```

For more examples, check [examples](examples/)

## Limitations

rusty_ytdl cannot download videos that fall into the following

- Regionally restricted (requires a [proxy](examples/proxy.rs))
- Private (if you have access, requires [cookies](examples/cookies.rs))
- Rentals (if you have access, requires [cookies](examples/cookies.rs))
- YouTube Premium content (if you have access, requires [cookies](examples/cookies.rs))
- Only [HLS Livestreams](https://en.wikipedia.org/wiki/HTTP_Live_Streaming) are currently supported. Other formats will not be fetched

Generated download links are valid for 6 hours, and may only be downloadable from the same IP address.

### Ratelimits

When doing too many requests YouTube might block. This will result in your requests getting denied with HTTP Status Code 429. The following steps might help you:

- Use proxies (you can find an example [proxy](examples/proxy.rs))
- Extend on the Proxy Idea by rotating (IPv6)Addresses (you can find an example [IPv6](examples/ipv6.rs))
- Use cookies (you can find an example [cookies](examples/cookies.rs))
  - for this to take effect you have to first wait for the current ratelimit to expire!
- Wait it out

# Installation (Workspace Local)

Since this is a workspace-integrated crate, do not add it from crates.io. Instead, add it via relative path dependency:

```toml
[dependencies]
rusty_ytdl = { path = "crates/rusty-ytdl", default-features = false, features = ["rustls"] }
```
