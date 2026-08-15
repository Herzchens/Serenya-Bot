#[tokio::test]
async fn get_info_with_options() {
    use rusty_ytdl::{choose_format, Video, VideoOptions, VideoQuality};

    let url = "https://www.youtube.com/watch?v=dQw4w9WgXcQ";

    let video_options = VideoOptions {
        quality: VideoQuality::Lowest,
        ..Default::default()
    };

    let video = Video::new_with_options(url, video_options.clone()).unwrap();

    let video_info = video.get_info().await.unwrap();

    assert!(!video_info.formats.is_empty());
    assert!(choose_format(&video_info.formats, &video_options).is_ok());
}
