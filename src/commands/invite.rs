use poise::serenity_prelude as serenity;

use crate::utils::{Context, Error};

/// Get the bot's invite link.
#[poise::command(slash_command, prefix_command)]
pub async fn invite(ctx: Context<'_>) -> Result<(), Error> {
    let invite_link = if let Some(url) = &ctx.data().config().bot.invite_url {
        url.clone()
    } else {
        let client_id = ctx.cache().current_user().id.get();
        format!(
            "https://discord.com/api/oauth2/authorize?client_id={}&permissions=8&scope=bot%20applications.commands",
            client_id
        )
    };

    let bot_name = ctx.cache().current_user().name.clone();
    let bot_avatar = ctx.cache().current_user().avatar_url().unwrap_or_default();

    let embed = serenity::CreateEmbed::new()
        .title(format!("🎵 Invite {}", bot_name))
        .description(format!(
            "Add me to your server and enjoy high-quality music!\n\n\
             🔗 **[Click here to invite {}]({})**",
            bot_name, invite_link
        ))
        .color(0x5865F2)
        .thumbnail(bot_avatar)
        .footer(serenity::CreateEmbedFooter::new(
            "Thank you for choosing Serenya! 💙",
        ));

    let button = serenity::CreateButton::new_link(invite_link)
        .label("Invite to Server")
        .emoji('🔗');

    let components = vec![serenity::CreateActionRow::Buttons(vec![button])];

    let reply = poise::CreateReply::default()
        .embed(embed)
        .components(components);
    ctx.send(reply).await?;
    Ok(())
}

/// Get support information.
#[poise::command(slash_command, prefix_command)]
pub async fn support(ctx: Context<'_>) -> Result<(), Error> {
    let repo_url = "https://github.com/Herzchens/Serenya-Bot";
    let bot_name = ctx.cache().current_user().name.clone();
    let bot_avatar = ctx.cache().current_user().avatar_url().unwrap_or_default();

    let embed = serenity::CreateEmbed::new()
        .title(format!("⭐ Support {}", bot_name))
        .description(format!(
            "Dự án của chúng tôi là mã nguồn mở trên GitHub! Nếu bạn yêu thích Serenya Bot, hãy tặng cho dự án 1 star để ủng hộ nhé ⭐\n\n\
             🔗 **[GitHub Repository]({})**",
            repo_url
        ))
        .color(0x5865F2)
        .thumbnail(bot_avatar)
        .footer(serenity::CreateEmbedFooter::new(
            "Cảm ơn bạn đã ủng hộ Serenya! 💙",
        ));

    let button = serenity::CreateButton::new_link(repo_url)
        .label("Star GitHub")
        .emoji('⭐');

    let components = vec![serenity::CreateActionRow::Buttons(vec![button])];

    let reply = poise::CreateReply::default()
        .embed(embed)
        .components(components);
    ctx.send(reply).await?;
    Ok(())
}
