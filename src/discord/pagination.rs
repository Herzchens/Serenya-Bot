use crate::core::Track;
use crate::discord::embeds::queue_embed;
use crate::utils::{Context, Error};
use poise::serenity_prelude as serenity;

fn make_navigation_components(
    ctx_id: u64,
    page: usize,
    total: usize,
) -> Vec<serenity::CreateActionRow> {
    let prev_btn = serenity::CreateButton::new(format!("{}_prev", ctx_id))
        .label("◀ Previous")
        .style(serenity::ButtonStyle::Primary)
        .disabled(page == 0);
    let next_btn = serenity::CreateButton::new(format!("{}_next", ctx_id))
        .label("Next ▶")
        .style(serenity::ButtonStyle::Primary)
        .disabled(page + 1 >= total);

    vec![serenity::CreateActionRow::Buttons(vec![prev_btn, next_btn])]
}

fn make_disabled_components(ctx_id: u64) -> Vec<serenity::CreateActionRow> {
    let prev_btn = serenity::CreateButton::new(format!("{}_prev", ctx_id))
        .label("◀ Previous")
        .style(serenity::ButtonStyle::Primary)
        .disabled(true);
    let next_btn = serenity::CreateButton::new(format!("{}_next", ctx_id))
        .label("Next ▶")
        .style(serenity::ButtonStyle::Primary)
        .disabled(true);

    vec![serenity::CreateActionRow::Buttons(vec![prev_btn, next_btn])]
}

fn get_page_slice(tracks: &[Track], page: usize, page_size: usize) -> &[Track] {
    let start_idx = page * page_size;
    let end_idx = (start_idx + page_size).min(tracks.len());
    &tracks[start_idx..end_idx]
}

async fn disable_buttons(
    mut msg: serenity::Message,
    http: &serenity::Http,
    embed: serenity::CreateEmbed,
    ctx_id: u64,
) -> Result<(), Error> {
    let disabled_components = make_disabled_components(ctx_id);
    msg.edit(
        http,
        serenity::EditMessage::new()
            .embed(embed)
            .components(disabled_components),
    )
    .await?;
    Ok(())
}

/// Paginate a list of tracks with next/prev buttons.
pub async fn paginate_queue(ctx: Context<'_>, tracks: &[Track]) -> Result<(), Error> {
    let page_size = 10;
    let total_pages = tracks.len().div_ceil(page_size).max(1);
    let mut current_page: usize = 0;
    let ctx_id = ctx.id();

    let initial_slice = get_page_slice(tracks, 0, page_size);
    let embed = queue_embed(initial_slice, 0, total_pages);
    let components = make_navigation_components(ctx_id, 0, total_pages);

    let reply = poise::CreateReply::default()
        .embed(embed)
        .components(components);
    let msg = ctx.send(reply).await?;
    let msg_inner = msg.into_message().await?;

    let timeout = std::time::Duration::from_secs(60);
    let start_time = std::time::Instant::now();

    while start_time.elapsed() < timeout {
        let remaining = timeout
            .checked_sub(start_time.elapsed())
            .unwrap_or_default();
        if remaining.is_zero() {
            break;
        }

        let collector = serenity::ComponentInteractionCollector::new(ctx.serenity_context())
            .author_id(ctx.author().id)
            .message_id(msg_inner.id)
            .timeout(remaining);

        if let Some(interaction) = collector.next().await {
            if interaction.data.custom_id == format!("{}_prev", ctx_id) {
                current_page = current_page.saturating_sub(1);
            } else if interaction.data.custom_id == format!("{}_next", ctx_id) {
                if current_page + 1 < total_pages {
                    current_page += 1;
                }
            } else {
                continue;
            }

            let slice = get_page_slice(tracks, current_page, page_size);
            let next_embed = queue_embed(slice, current_page, total_pages);
            let next_comps = make_navigation_components(ctx_id, current_page, total_pages);

            let _ = interaction
                .create_response(
                    &ctx.serenity_context().http,
                    serenity::CreateInteractionResponse::UpdateMessage(
                        serenity::CreateInteractionResponseMessage::new()
                            .embed(next_embed)
                            .components(next_comps),
                    ),
                )
                .await;
        } else {
            break;
        }
    }

    let final_slice = get_page_slice(tracks, current_page, page_size);
    let final_embed = queue_embed(final_slice, current_page, total_pages);
    let _ = disable_buttons(msg_inner, &ctx.serenity_context().http, final_embed, ctx_id).await;

    Ok(())
}

/// Paginate lyrics text page by page.
pub async fn paginate_lyrics(
    ctx: Context<'_>,
    title: &str,
    artist: &str,
    pages: &[String],
) -> Result<(), Error> {
    let total_pages = pages.len();
    let mut current_page: usize = 0;
    let ctx_id = ctx.id();

    let make_embed = |page_idx: usize| {
        serenity::CreateEmbed::new()
            .title(format!("🎤 Lyrics: {} - {}", title, artist))
            .description(&pages[page_idx])
            .footer(serenity::CreateEmbedFooter::new(format!(
                "Page {}/{}",
                page_idx + 1,
                total_pages
            )))
            .color(0x5865F2)
    };

    let embed = make_embed(0);
    let components = make_navigation_components(ctx_id, 0, total_pages);

    let reply = poise::CreateReply::default()
        .embed(embed)
        .components(components);
    let msg = ctx.send(reply).await?;
    let msg_inner = msg.into_message().await?;

    let timeout = std::time::Duration::from_secs(60);
    let start_time = std::time::Instant::now();

    while start_time.elapsed() < timeout {
        let remaining = timeout
            .checked_sub(start_time.elapsed())
            .unwrap_or_default();
        if remaining.is_zero() {
            break;
        }

        let collector = serenity::ComponentInteractionCollector::new(ctx.serenity_context())
            .author_id(ctx.author().id)
            .message_id(msg_inner.id)
            .timeout(remaining);

        if let Some(interaction) = collector.next().await {
            if interaction.data.custom_id == format!("{}_prev", ctx_id) {
                current_page = current_page.saturating_sub(1);
            } else if interaction.data.custom_id == format!("{}_next", ctx_id) {
                if current_page + 1 < total_pages {
                    current_page += 1;
                }
            } else {
                continue;
            }

            let next_embed = make_embed(current_page);
            let next_comps = make_navigation_components(ctx_id, current_page, total_pages);

            let _ = interaction
                .create_response(
                    &ctx.serenity_context().http,
                    serenity::CreateInteractionResponse::UpdateMessage(
                        serenity::CreateInteractionResponseMessage::new()
                            .embed(next_embed)
                            .components(next_comps),
                    ),
                )
                .await;
        } else {
            break;
        }
    }

    let final_embed = make_embed(current_page);
    let _ = disable_buttons(msg_inner, &ctx.serenity_context().http, final_embed, ctx_id).await;

    Ok(())
}
