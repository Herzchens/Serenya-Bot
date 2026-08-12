use crate::core::loop_mode::LoopMode;
use crate::utils::{Context, Error, SerenyaError};

#[derive(Clone, Copy)]
struct LoopReply {
    title: &'static str,
    response: &'static str,
    color: u32,
}

async fn update_loop_mode_then_reply<F, Fut, E>(
    player_lock: std::sync::Arc<tokio::sync::RwLock<crate::core::GuildPlayer>>,
    mode: Option<String>,
    reply: F,
) -> Result<(), E>
where
    F: FnOnce(LoopReply) -> Fut,
    Fut: std::future::Future<Output = Result<(), E>>,
{
    let mut player = player_lock.write().await;
    let next_mode = if let Some(m) = mode {
        match m.to_lowercase().as_str() {
            "off" | "none" => Some(LoopMode::Off),
            "track" | "song" | "one" => Some(LoopMode::Track),
            "queue" | "all" => Some(LoopMode::Queue),
            _ => None,
        }
    } else {
        Some(match player.loop_mode {
            LoopMode::Off => LoopMode::Track,
            LoopMode::Track => LoopMode::Queue,
            LoopMode::Queue => LoopMode::Off,
        })
    };

    let response = if let Some(next_mode) = next_mode {
        player.loop_mode = next_mode;
        match player.loop_mode {
            LoopMode::Off => LoopReply {
                title: "🔁 Loop Mode",
                response: "Loop mode is now **Off**.",
                color: 0x5865F2,
            },
            LoopMode::Track => LoopReply {
                title: "🔂 Loop Mode",
                response: "Loop mode is now **Track** (repeating current song).",
                color: 0x5865F2,
            },
            LoopMode::Queue => LoopReply {
                title: "🔁 Loop Mode",
                response: "Loop mode is now **Queue** (repeating entire queue).",
                color: 0x5865F2,
            },
        }
    } else {
        LoopReply {
            title: "❌ Error",
            response: "Invalid loop mode. Use 'off', 'track', or 'queue'.",
            color: 0xED4245,
        }
    };

    drop(player);
    reply(response).await
}

/// Change the loop mode (off, track, queue).
#[poise::command(
    slash_command,
    prefix_command,
    rename = "loop",
    aliases("repeat"),
    check = "crate::discord::checks::require_same_voice_channel"
)]
pub async fn loop_cmd(
    ctx: Context<'_>,
    #[description = "Loop mode: off, track, queue"] mode: Option<String>,
) -> Result<(), Error> {
    let guild_id = ctx
        .guild_id()
        .ok_or_else(|| SerenyaError::Config("This command can only be used in a server.".into()))?;

    let player_lock = ctx
        .data()
        .guild_players
        .get(&guild_id)
        .map(|entry| entry.value().clone())
        .ok_or_else(|| SerenyaError::NotFound("No player active in this server.".into()))?;

    update_loop_mode_then_reply(player_lock, mode, |reply| async move {
        let embed =
            crate::discord::embeds::playback_status_embed(reply.title, reply.response, reply.color);
        ctx.send(poise::CreateReply::default().embed(embed)).await?;
        Ok::<(), Error>(())
    })
    .await
}

#[cfg(test)]
mod lock_scope_tests {
    use super::update_loop_mode_then_reply;
    use crate::core::GuildPlayer;
    use std::sync::Arc;
    use std::time::Duration;
    use tokio::sync::{RwLock, oneshot};

    #[tokio::test]
    async fn loop_reply_does_not_hold_player_write_lock() {
        let player = Arc::new(RwLock::new(GuildPlayer::new()));
        let task_player = Arc::clone(&player);
        let (entered_tx, entered_rx) = oneshot::channel();
        let (release_tx, release_rx) = oneshot::channel();

        let task = tokio::spawn(async move {
            update_loop_mode_then_reply(
                task_player,
                Some("track".to_owned()),
                move |_| async move {
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
        task.await.expect("loop test task should join").unwrap();

        assert!(
            writer_acquired,
            "Discord reply await must not retain the guild player write lock"
        );
    }
}
