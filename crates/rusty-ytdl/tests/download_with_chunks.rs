#[tokio::test]
async fn download_with_chunks() {
    use rusty_ytdl::{Video, VideoOptions, VideoQuality};

    let url = "https://www.youtube.com/watch?v=dQw4w9WgXcQ";
    let video_options = VideoOptions {
        quality: VideoQuality::Highest,
        ..Default::default()
    };
    let video = Video::new_with_options(url, video_options).unwrap();
    let stream = video
        .stream()
        .await
        .expect("live stream resolution must succeed");
    let first_chunk = stream
        .chunk()
        .await
        .expect("first chunk request must succeed")
        .expect("stream must contain at least one chunk");

    assert!(!first_chunk.is_empty());
}
