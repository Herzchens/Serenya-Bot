#[tokio::test]
async fn get_info() {
    use rusty_ytdl::Video;

    let url = "https://www.youtube.com/watch?v=dQw4w9WgXcQ";

    let video = Video::new(url).unwrap();

    let video_info = video.get_info().await.unwrap();

    assert!(!video_info.formats.is_empty());
}
