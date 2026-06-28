use super::*;
use models::PlaylistTrack;

fn get_temp_db_path() -> PathBuf {
    let rand_num: u32 = rand::random();
    std::env::temp_dir().join(format!("test_db_{}.yml", rand_num))
}

#[tokio::test]
async fn test_db_settings() -> Result<(), Box<dyn std::error::Error>> {
    let db_path = get_temp_db_path();
    let manager = DatabaseManager::load(&db_path).await?;

    // Verify default settings
    let guild_settings = manager.get_guild_settings(12345).await;
    assert!(guild_settings.announce_track);
    assert_eq!(guild_settings.total_songs_played, 0);

    let user_settings = manager.get_user_settings(67890).await;
    assert_eq!(user_settings.quality, "balanced");

    // Update settings
    let mut guild_settings = guild_settings;
    guild_settings.announce_track = false;
    manager.update_guild_settings(12345, guild_settings).await;

    let mut user_settings = user_settings;
    user_settings.quality = "quality".to_owned();
    manager.update_user_settings(67890, user_settings).await;

    // Verify update
    let updated_guild = manager.get_guild_settings(12345).await;
    assert!(!updated_guild.announce_track);

    let updated_user = manager.get_user_settings(67890).await;
    assert_eq!(updated_user.quality, "quality");

    let _ = tokio::fs::remove_file(&db_path).await;
    Ok(())
}

#[tokio::test]
async fn test_db_playlists() -> Result<(), Box<dyn std::error::Error>> {
    let db_path = get_temp_db_path();
    let manager = DatabaseManager::load(&db_path).await?;

    // Create playlist
    manager.create_playlist(12345, "my_list", 3).await?;
    let names = manager.get_user_playlist_names(12345).await;
    assert_eq!(names, vec!["my_list"]);

    // Add track
    let track = PlaylistTrack {
        title: "Track 1".to_owned(),
        url: "https://example.com/1".to_owned(),
        duration_secs: Some(180),
    };
    manager.add_to_playlist(12345, "my_list", track, 2).await?;

    let playlist = manager
        .get_user_playlist(12345, "my_list")
        .await
        .ok_or("playlist not found")?;
    assert_eq!(playlist.tracks.len(), 1);
    assert_eq!(playlist.tracks[0].title, "Track 1");

    // Check limit enforce
    let track2 = PlaylistTrack {
        title: "Track 2".to_owned(),
        url: "https://example.com/2".to_owned(),
        duration_secs: None,
    };
    manager.add_to_playlist(12345, "my_list", track2, 2).await?;

    let track3 = PlaylistTrack {
        title: "Track 3".to_owned(),
        url: "https://example.com/3".to_owned(),
        duration_secs: None,
    };
    assert!(
        manager
            .add_to_playlist(12345, "my_list", track3, 2)
            .await
            .is_err()
    );

    // Test remove_from_playlist
    manager.remove_from_playlist(12345, "my_list", 1).await?;
    let playlist = manager
        .get_user_playlist(12345, "my_list")
        .await
        .ok_or("playlist not found")?;
    assert_eq!(playlist.tracks.len(), 1);
    assert_eq!(playlist.tracks[0].title, "Track 2");

    // Test rename_playlist
    manager
        .rename_playlist(12345, "my_list", "new_list")
        .await?;
    let names = manager.get_user_playlist_names(12345).await;
    assert_eq!(names, vec!["new_list"]);

    // Test rename duplicate error
    manager.create_playlist(12345, "another_list", 3).await?;
    assert!(
        manager
            .rename_playlist(12345, "new_list", "another_list")
            .await
            .is_err()
    );

    let _ = tokio::fs::remove_file(&db_path).await;
    Ok(())
}
