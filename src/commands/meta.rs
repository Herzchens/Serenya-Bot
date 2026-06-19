use crate::utils::{Context, Error};

/// Show bot latency.
#[poise::command(slash_command, prefix_command)]
pub async fn ping(ctx: Context<'_>) -> Result<(), Error> {
    let latency = ctx.ping().await;
    let response = format!("🏓 Pong! Latency: {latency:.0?}");
    ctx.say(response).await?;
    Ok(())
}

/// Show bot information.
#[poise::command(slash_command, prefix_command)]
pub async fn about(ctx: Context<'_>) -> Result<(), Error> {
    let config = &ctx.data().config;
    let uptime = ctx.data().start_time.elapsed();
    let hours = uptime.as_secs() / 3600;
    let minutes = (uptime.as_secs() % 3600) / 60;
    let seconds = uptime.as_secs() % 60;

    let embed = poise::serenity_prelude::CreateEmbed::new()
        .title(&config.bot.display_name)
        .description("A multi-guild Discord music bot built with Rust 🦀")
        .field("Instance", &config.bot.instance_id, true)
        .field("Version", env!("CARGO_PKG_VERSION"), true)
        .field("Uptime", format!("{hours}h {minutes}m {seconds}s"), true)
        .field("Tech Stack", "Rust • Poise • Serenity • Songbird", false)
        .color(0x5865F2);

    let reply = poise::CreateReply::default().embed(embed);
    ctx.send(reply).await?;
    Ok(())
}
