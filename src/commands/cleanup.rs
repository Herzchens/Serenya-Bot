use crate::utils::{Context, Error, SerenyaError};
use poise::serenity_prelude as serenity;

fn cleanup_matches_message(content: &str, is_bot_message: bool, prefix: &str) -> bool {
    is_bot_message || content.starts_with(prefix)
}

async fn check_cleanup_permissions(ctx: Context<'_>) -> Result<bool, Error> {
    let author_id = ctx.author().id.get();
    let owner_id = ctx.data().config().bot.owner;
    if author_id == owner_id {
        return Ok(true);
    }

    if let Some(guild_id) = ctx.guild_id() {
        let member = guild_id.member(ctx, ctx.author().id).await?;
        if let Some(guild) = ctx.guild() {
            let permissions = guild.member_permissions(&member);
            if permissions.administrator() {
                return Ok(true);
            }
        }
    }

    Err(SerenyaError::Permission(
        "You must have the `Administrator` permission or be the bot owner to use this command."
            .into(),
    )
    .into())
}

/// Delete bot messages in the current channel.
#[poise::command(slash_command, prefix_command, rename = "cleanup", aliases("clean"))]
pub async fn cleanup(
    ctx: Context<'_>,
    #[description = "Number of messages to scan (default: 100)"]
    #[min = 1]
    #[max = 2000]
    amount: Option<u32>,
) -> Result<(), Error> {
    check_cleanup_permissions(ctx).await?;

    ctx.defer_ephemeral().await?;

    let channel_id = ctx.channel_id();
    let bot_id = ctx.cache().current_user().id;

    let (scanned_count, deleted_count, skipped_old) = {
        let mut messages = Vec::new();
        let mut before_id = None;

        let limit_to_scan = amount.unwrap_or(100);
        let mut remaining = limit_to_scan;

        while remaining > 0 {
            let count_to_fetch = remaining.min(100) as u8;
            let mut builder = serenity::GetMessages::new().limit(count_to_fetch);
            if let Some(id) = before_id {
                builder = builder.before(id);
            }

            let fetched = channel_id.messages(ctx, builder).await?;
            if fetched.is_empty() {
                break;
            }

            before_id = fetched.last().map(|m| m.id);
            remaining -= fetched.len() as u32;
            messages.extend(fetched);

            if before_id.is_none() {
                break;
            }
        }

        let now = chrono::Utc::now();
        let mut to_delete = Vec::new();
        let mut skipped_old = 0;
        let default_prefix = ctx.data().config().bot.prefix.clone();
        let prefix = if let Some(guild_id) = ctx.guild_id() {
            ctx.data()
                .database
                .get_guild_prefix(guild_id.get(), &default_prefix)
        } else {
            std::sync::Arc::from(default_prefix.as_str())
        };

        for msg in &messages {
            let is_bot_msg = msg.author.id == bot_id;

            if cleanup_matches_message(&msg.content, is_bot_msg, prefix.as_ref()) {
                let age = now.signed_duration_since(msg.timestamp.with_timezone(&chrono::Utc));
                if age.num_days() >= 14 {
                    skipped_old += 1;
                } else {
                    to_delete.push(msg.id);
                }
            }
        }

        let deleted = to_delete.len();

        // Delete in chunks of 100
        for chunk in to_delete.chunks(100) {
            if chunk.len() == 1 {
                channel_id.delete_message(ctx, chunk[0]).await?;
            } else if chunk.len() > 1 {
                channel_id.delete_messages(ctx, chunk).await?;
            }
        }

        (messages.len(), deleted, skipped_old)
    };

    let response = format!(
        "🧹 **Cleanup complete:**\n\
         • Scanned: **{scanned_count}** messages\n\
         • Deleted: **{deleted_count}** bot/command messages\n\
         • Skipped (older than 14 days): **{skipped_old}**",
    );

    let reply_handle = ctx.say(response).await?;
    if let Ok(msg) = reply_handle.into_message().await {
        let http = ctx.serenity_context().http.clone();
        tokio::spawn(async move {
            tokio::time::sleep(std::time::Duration::from_secs(10)).await;
            let _ = msg.delete(&http).await;
        });
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::cleanup_matches_message;

    #[test]
    fn cleanup_uses_the_effective_guild_prefix() {
        assert!(cleanup_matches_message("?play song", false, "?"));
        assert!(!cleanup_matches_message("!play song", false, "?"));
        assert!(cleanup_matches_message("anything", true, "?"));
    }
}
