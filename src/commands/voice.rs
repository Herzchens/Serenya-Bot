use crate::utils::{Context, Error, SerenyaError};

async fn record_join_state_then_reply<F, Fut, E>(
    player_lock: std::sync::Arc<tokio::sync::RwLock<crate::core::GuildPlayer>>,
    channel_id: poise::serenity_prelude::ChannelId,
    announce_channel: poise::serenity_prelude::ChannelId,
    reply: F,
) -> Result<(), E>
where
    F: FnOnce() -> Fut,
    Fut: std::future::Future<Output = Result<(), E>>,
{
    let mut player = player_lock.write().await;
    player.voice_channel = Some(channel_id);
    player.announce_channel = Some(announce_channel);
    drop(player);
    reply().await
}

async fn leave_disconnect_then_cleanup<D, DFut, C, CFut, E>(
    has_handler: bool,
    disconnect: D,
    cleanup: C,
) -> Result<(), E>
where
    D: FnOnce() -> DFut,
    DFut: std::future::Future<Output = Result<(), E>>,
    C: FnOnce() -> CFut,
    CFut: std::future::Future<Output = ()>,
{
    if has_handler {
        disconnect().await?;
    }
    cleanup().await;
    Ok(())
}

fn begin_intentional_voice_disconnect(
    player: &mut crate::core::GuildPlayer,
) -> Option<poise::serenity_prelude::ChannelId> {
    let previous = player.voice_channel;
    player.bot_voice_generation = player.bot_voice_generation.wrapping_add(1);
    player.voice_channel = None;
    previous
}

fn rollback_failed_intentional_voice_disconnect(
    player: &mut crate::core::GuildPlayer,
    previous: Option<poise::serenity_prelude::ChannelId>,
) {
    player.bot_voice_generation = player.bot_voice_generation.wrapping_add(1);
    player.voice_channel = previous;
}

/// Join the user's voice channel.
#[poise::command(slash_command, prefix_command, aliases("j"))]
pub async fn join(ctx: Context<'_>) -> Result<(), Error> {
    tracing::info!("Entering join command");
    let guild_id = ctx
        .guild_id()
        .ok_or_else(|| SerenyaError::Config("This command can only be used in a server.".into()))?;

    let channel_id = {
        let guild = ctx
            .guild()
            .ok_or_else(|| SerenyaError::NotFound("Guild not found".into()))?;
        guild
            .voice_states
            .get(&ctx.author().id)
            .and_then(|state| state.channel_id)
            .ok_or_else(|| {
                SerenyaError::Voice("You must be in a voice channel to use this command.".into())
            })?
    };

    let manager = songbird::get(ctx.serenity_context())
        .await
        .ok_or_else(|| SerenyaError::Voice("Songbird manager not initialized.".into()))?
        .clone();

    tracing::info!("Voice connect start: joining channel {:?}", channel_id);
    manager
        .join(guild_id, channel_id)
        .await
        .map_err(|err| SerenyaError::Voice(format!("Failed to join voice channel: {err}")))?;
    tracing::info!("Voice connect complete: channel {:?}", channel_id);
    let _ = crate::audio::quality::apply_bitrate(ctx, guild_id, channel_id).await;

    let player_lock = ctx
        .data()
        .guild_players
        .entry(guild_id)
        .or_insert_with(|| {
            std::sync::Arc::new(tokio::sync::RwLock::new(crate::core::GuildPlayer::new()))
        })
        .clone();

    record_join_state_then_reply(player_lock, channel_id, ctx.channel_id(), || async move {
        tracing::info!("Join completed successfully for channel {:?}", channel_id);
        ctx.say(format!("🔊 Joined <#{channel_id}>")).await?;
        Ok::<(), Error>(())
    })
    .await
}

/// Leave the voice channel and clear queue state.
#[poise::command(slash_command, prefix_command, aliases("l"))]
pub async fn leave(ctx: Context<'_>) -> Result<(), Error> {
    tracing::info!("Entering leave command");
    let guild_id = ctx
        .guild_id()
        .ok_or_else(|| SerenyaError::Config("This command can only be used in a server.".into()))?;

    let manager = songbird::get(ctx.serenity_context())
        .await
        .ok_or_else(|| SerenyaError::Voice("Songbird manager not initialized.".into()))?
        .clone();

    tracing::info!("Voice disconnect start: leaving voice channel");
    let has_handler = manager.get(guild_id).is_some();
    let player_lock_for_leave = ctx
        .data()
        .guild_players
        .get(&guild_id)
        .map(|entry| entry.value().clone());
    let previous_voice_channel = if let Some(ref player_lock) = player_lock_for_leave {
        let mut player = player_lock.write().await;
        begin_intentional_voice_disconnect(&mut player)
    } else {
        None
    };

    let leave_result = leave_disconnect_then_cleanup(
        has_handler,
        || async { manager.remove(guild_id).await },
        || async {
            if let Some(player_lock) = ctx
                .data()
                .guild_players
                .get(&guild_id)
                .map(|r| r.value().clone())
            {
                crate::audio::events::finalize_interrupted_playback_stats(
                    ctx.data().database.as_ref(),
                    guild_id,
                    &player_lock,
                )
                .await;
                let mut player = player_lock.write().await;
                player.reset();
                player.voice_channel = None;
                player.announce_channel = None;
                tracing::info!("Reset guild player state and dropped track handle");
            }
            ctx.data().guild_players.remove(&guild_id);
        },
    )
    .await;

    if let Err(err) = leave_result {
        if let Some(player_lock) = player_lock_for_leave {
            let mut player = player_lock.write().await;
            rollback_failed_intentional_voice_disconnect(&mut player, previous_voice_channel);
        }
        return Err(err.into());
    }
    tracing::info!("Voice disconnect complete");

    crate::audio::runtime::cleanup_guild(guild_id.get());
    tracing::info!("Leave completed successfully");

    ctx.say("👋 Left voice channel and cleared state.").await?;
    Ok(())
}

#[cfg(test)]
mod lock_scope_tests {
    use super::record_join_state_then_reply;
    use crate::core::GuildPlayer;
    use poise::serenity_prelude::ChannelId;
    use std::sync::Arc;
    use std::time::Duration;
    use tokio::sync::{RwLock, oneshot};

    #[tokio::test]
    async fn join_reply_does_not_hold_player_write_lock() {
        let player = Arc::new(RwLock::new(GuildPlayer::new()));
        let task_player = Arc::clone(&player);
        let (entered_tx, entered_rx) = oneshot::channel();
        let (release_tx, release_rx) = oneshot::channel();

        let task = tokio::spawn(async move {
            record_join_state_then_reply(
                task_player,
                ChannelId::new(10),
                ChannelId::new(20),
                move || async move {
                    let _ = entered_tx.send(());
                    let _ = release_rx.await;
                    Ok::<(), ()>(())
                },
            )
            .await
        });

        entered_rx.await.expect("reply hook should start");
        let writer_acquired = tokio::time::timeout(Duration::from_millis(500), player.write())
            .await
            .is_ok();
        let _ = release_tx.send(());
        task.await.expect("join test task should join").unwrap();

        assert!(
            writer_acquired,
            "Discord reply await must not retain the guild player write lock"
        );
    }
}

#[cfg(test)]
mod leave_disconnect_failure_tests {
    use super::{
        begin_intentional_voice_disconnect, leave_disconnect_then_cleanup,
        rollback_failed_intentional_voice_disconnect,
    };
    use crate::core::GuildPlayer;
    use poise::serenity_prelude::ChannelId;
    use std::sync::Arc;
    use std::sync::atomic::{AtomicBool, Ordering};

    #[test]
    fn intentional_leave_marker_suppresses_recovery_and_rolls_back_on_failure() {
        let mut player = GuildPlayer::new();
        player.voice_channel = Some(ChannelId::new(55));
        let previous_generation = player.bot_voice_generation;
        let previous = begin_intentional_voice_disconnect(&mut player);
        assert_eq!(previous, Some(ChannelId::new(55)));
        assert_eq!(player.voice_channel, None);
        assert_ne!(player.bot_voice_generation, previous_generation);
        rollback_failed_intentional_voice_disconnect(&mut player, previous);
        assert_eq!(player.voice_channel, Some(ChannelId::new(55)));
    }

    #[tokio::test]
    async fn failed_leave_preserves_player_and_stats_state_for_retry() {
        let cleaned = Arc::new(AtomicBool::new(false));
        let cleanup_flag = Arc::clone(&cleaned);

        let result = leave_disconnect_then_cleanup(
            true,
            || async { Err::<(), &'static str>("gateway unavailable") },
            move || async move { cleanup_flag.store(true, Ordering::SeqCst) },
        )
        .await;

        assert_eq!(result, Err("gateway unavailable"));
        assert!(
            !cleaned.load(Ordering::SeqCst),
            "failed /leave must not finalize/reset/remove local playback state before Songbird can leave"
        );
    }

    #[tokio::test]
    async fn successful_leave_commits_destructive_cleanup() {
        let cleaned = Arc::new(AtomicBool::new(false));
        let cleanup_flag = Arc::clone(&cleaned);

        let result = leave_disconnect_then_cleanup(
            true,
            || async { Ok::<(), &'static str>(()) },
            move || async move { cleanup_flag.store(true, Ordering::SeqCst) },
        )
        .await;

        assert_eq!(result, Ok(()));
        assert!(cleaned.load(Ordering::SeqCst));
    }
}
