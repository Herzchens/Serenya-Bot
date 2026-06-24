use crate::core::loop_mode::LoopMode;
use crate::utils::{Context, Error, SerenyaError};

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
        .ok_or_else(|| SerenyaError::NotFound("No player active in this server.".into()))?;

    let mut player = player_lock.write().await;

    let next_mode = if let Some(m) = mode {
        match m.to_lowercase().as_str() {
            "off" | "none" => LoopMode::Off,
            "track" | "song" | "one" => LoopMode::Track,
            "queue" | "all" => LoopMode::Queue,
            _ => {
                let embed = crate::discord::embeds::playback_status_embed(
                    "❌ Error",
                    "Invalid loop mode. Use 'off', 'track', or 'queue'.",
                    0xED4245,
                );
                ctx.send(poise::CreateReply::default().embed(embed)).await?;
                return Ok(());
            }
        }
    } else {
        match player.loop_mode {
            LoopMode::Off => LoopMode::Track,
            LoopMode::Track => LoopMode::Queue,
            LoopMode::Queue => LoopMode::Off,
        }
    };

    player.loop_mode = next_mode;

    let (title, response) = match player.loop_mode {
        LoopMode::Off => ("🔁 Loop Mode", "Loop mode is now **Off**."),
        LoopMode::Track => (
            "🔂 Loop Mode",
            "Loop mode is now **Track** (repeating current song).",
        ),
        LoopMode::Queue => (
            "🔁 Loop Mode",
            "Loop mode is now **Queue** (repeating entire queue).",
        ),
    };

    let embed = crate::discord::embeds::playback_status_embed(title, response, 0x5865F2);
    ctx.send(poise::CreateReply::default().embed(embed)).await?;
    Ok(())
}
