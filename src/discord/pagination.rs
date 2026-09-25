use crate::discord::embeds::{QueueTrackSnapshot, queue_embed};
use crate::utils::{Context, Error};
use poise::serenity_prelude as serenity;
use serenity::futures::StreamExt;

fn make_navigation_components(
    ctx_id: u64,
    page: usize,
    total: usize,
) -> Vec<serenity::CreateActionRow> {
    let first_btn = serenity::CreateButton::new(format!("{}_first", ctx_id))
        .label("⏮ First")
        .style(serenity::ButtonStyle::Primary)
        .disabled(page == 0);
    let prev_btn = serenity::CreateButton::new(format!("{}_prev", ctx_id))
        .label("◀ Previous")
        .style(serenity::ButtonStyle::Primary)
        .disabled(page == 0);
    let next_btn = serenity::CreateButton::new(format!("{}_next", ctx_id))
        .label("Next ▶")
        .style(serenity::ButtonStyle::Primary)
        .disabled(page + 1 >= total);
    let last_btn = serenity::CreateButton::new(format!("{}_last", ctx_id))
        .label("Last ⏭")
        .style(serenity::ButtonStyle::Primary)
        .disabled(page + 1 >= total);

    let mut rows = vec![serenity::CreateActionRow::Buttons(vec![
        first_btn, prev_btn, next_btn, last_btn,
    ])];

    if total > 1 {
        let mut page_indices = std::collections::BTreeSet::new();
        page_indices.insert(0);
        page_indices.insert(total - 1);
        let window_start = page.saturating_sub(10);
        let window_end = (page + 10).min(total - 1);
        for p in window_start..=window_end {
            page_indices.insert(p);
        }
        if page_indices.len() < 25 && total > 25 {
            let step = total / 20;
            if step > 0 {
                for i in 0..20 {
                    let p = (i * step).min(total - 1);
                    if page_indices.len() < 25 {
                        page_indices.insert(p);
                    }
                }
            }
        }

        let mut options = Vec::new();
        for p in page_indices {
            options.push(
                serenity::CreateSelectMenuOption::new(format!("Page {}", p + 1), p.to_string())
                    .description(format!("Jump to page {}", p + 1))
                    .default_selection(p == page),
            );
        }

        let select_menu = serenity::CreateSelectMenu::new(
            format!("{}_select", ctx_id),
            serenity::CreateSelectMenuKind::String { options },
        )
        .placeholder("Jump to page...");

        rows.push(serenity::CreateActionRow::SelectMenu(select_menu));
    }

    rows
}

fn make_disabled_components(
    ctx_id: u64,
    page: usize,
    total: usize,
) -> Vec<serenity::CreateActionRow> {
    let first_btn = serenity::CreateButton::new(format!("{}_first", ctx_id))
        .label("⏮ First")
        .style(serenity::ButtonStyle::Primary)
        .disabled(true);
    let prev_btn = serenity::CreateButton::new(format!("{}_prev", ctx_id))
        .label("◀ Previous")
        .style(serenity::ButtonStyle::Primary)
        .disabled(true);
    let next_btn = serenity::CreateButton::new(format!("{}_next", ctx_id))
        .label("Next ▶")
        .style(serenity::ButtonStyle::Primary)
        .disabled(true);
    let last_btn = serenity::CreateButton::new(format!("{}_last", ctx_id))
        .label("Last ⏭")
        .style(serenity::ButtonStyle::Primary)
        .disabled(true);

    let mut rows = vec![serenity::CreateActionRow::Buttons(vec![
        first_btn, prev_btn, next_btn, last_btn,
    ])];

    if total > 1 {
        let mut page_indices = std::collections::BTreeSet::new();
        page_indices.insert(0);
        page_indices.insert(total - 1);
        let window_start = page.saturating_sub(10);
        let window_end = (page + 10).min(total - 1);
        for p in window_start..=window_end {
            page_indices.insert(p);
        }
        if page_indices.len() < 25 && total > 25 {
            let step = total / 20;
            if step > 0 {
                for i in 0..20 {
                    let p = (i * step).min(total - 1);
                    if page_indices.len() < 25 {
                        page_indices.insert(p);
                    }
                }
            }
        }

        let mut options = Vec::new();
        for p in page_indices {
            options.push(
                serenity::CreateSelectMenuOption::new(format!("Page {}", p + 1), p.to_string())
                    .description(format!("Jump to page {}", p + 1))
                    .default_selection(p == page),
            );
        }

        let select_menu = serenity::CreateSelectMenu::new(
            format!("{}_select", ctx_id),
            serenity::CreateSelectMenuKind::String { options },
        )
        .placeholder("Jump to page...")
        .disabled(true);

        rows.push(serenity::CreateActionRow::SelectMenu(select_menu));
    }

    rows
}

fn get_page_slice(
    tracks: &[QueueTrackSnapshot],
    page: usize,
    page_size: usize,
) -> &[QueueTrackSnapshot] {
    let start_idx = page * page_size;
    let end_idx = (start_idx + page_size).min(tracks.len());
    &tracks[start_idx..end_idx]
}

fn navigation_target(
    ctx_id: u64,
    current_page: usize,
    total_pages: usize,
    custom_id: &str,
    selected_value: Option<&str>,
) -> Option<usize> {
    if total_pages == 0 {
        return None;
    }

    let prefix = format!("{ctx_id}_");
    let action = custom_id.strip_prefix(&prefix)?;
    match action {
        "prev" => Some(current_page.saturating_sub(1)),
        "next" => Some(
            current_page
                .saturating_add(1)
                .min(total_pages.saturating_sub(1)),
        ),
        "first" => Some(0),
        "last" => Some(total_pages.saturating_sub(1)),
        "select" => selected_value?
            .parse::<usize>()
            .ok()
            .filter(|page| *page < total_pages),
        _ => None,
    }
}

async fn disable_buttons(
    mut msg: serenity::Message,
    http: &serenity::Http,
    embed: serenity::CreateEmbed,
    ctx_id: u64,
    page: usize,
    total: usize,
) -> Result<(), Error> {
    let disabled_components = make_disabled_components(ctx_id, page, total);
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
pub async fn paginate_queue(
    ctx: Context<'_>,
    tracks: &[QueueTrackSnapshot],
    title: &str,
) -> Result<(), Error> {
    let page_size = 10;
    let total_pages = tracks.len().div_ceil(page_size).max(1);
    let mut current_page: usize = 0;
    let ctx_id = ctx.id();

    let initial_slice = get_page_slice(tracks, 0, page_size);
    let embed = queue_embed(initial_slice, 0, total_pages, tracks.len(), title);
    let components = make_navigation_components(ctx_id, 0, total_pages);

    let reply = poise::CreateReply::default()
        .embed(embed)
        .components(components);
    let msg = ctx.send(reply).await?;
    let msg_inner = msg.into_message().await?;

    let timeout = std::time::Duration::from_secs(180);
    // Keep one collector alive for the entire paginator lifetime. Re-creating a one-shot
    // collector after every response leaves a gap where a rapid second click can be lost.
    let mut interaction_stream =
        serenity::ComponentInteractionCollector::new(ctx.serenity_context())
            .author_id(ctx.author().id)
            .message_id(msg_inner.id)
            .timeout(timeout)
            .stream();

    while let Some(interaction) = interaction_stream.next().await {
        let selected_value = match &interaction.data.kind {
            serenity::ComponentInteractionDataKind::StringSelect { values } => {
                values.first().map(String::as_str)
            }
            _ => None,
        };

        let Some(target_page) = navigation_target(
            ctx_id,
            current_page,
            total_pages,
            &interaction.data.custom_id,
            selected_value,
        ) else {
            continue;
        };
        current_page = target_page;

        let slice = get_page_slice(tracks, current_page, page_size);
        let next_embed = queue_embed(slice, current_page, total_pages, tracks.len(), title);
        let next_comps = make_navigation_components(ctx_id, current_page, total_pages);

        if let Err(error) = interaction
            .create_response(
                &ctx.serenity_context().http,
                serenity::CreateInteractionResponse::UpdateMessage(
                    serenity::CreateInteractionResponseMessage::new()
                        .embed(next_embed)
                        .components(next_comps),
                ),
            )
            .await
        {
            tracing::warn!(
                error = %error,
                custom_id = %interaction.data.custom_id,
                "failed to acknowledge queue pagination interaction"
            );
        }
    }

    let final_slice = get_page_slice(tracks, current_page, page_size);
    let final_embed = queue_embed(final_slice, current_page, total_pages, tracks.len(), title);
    let _ = disable_buttons(
        msg_inner,
        &ctx.serenity_context().http,
        final_embed,
        ctx_id,
        current_page,
        total_pages,
    )
    .await;

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

    let timeout = std::time::Duration::from_secs(180);
    let mut interaction_stream =
        serenity::ComponentInteractionCollector::new(ctx.serenity_context())
            .author_id(ctx.author().id)
            .message_id(msg_inner.id)
            .timeout(timeout)
            .stream();

    while let Some(interaction) = interaction_stream.next().await {
        let selected_value = match &interaction.data.kind {
            serenity::ComponentInteractionDataKind::StringSelect { values } => {
                values.first().map(String::as_str)
            }
            _ => None,
        };

        let Some(target_page) = navigation_target(
            ctx_id,
            current_page,
            total_pages,
            &interaction.data.custom_id,
            selected_value,
        ) else {
            continue;
        };
        current_page = target_page;

        let next_embed = make_embed(current_page);
        let next_comps = make_navigation_components(ctx_id, current_page, total_pages);

        if let Err(error) = interaction
            .create_response(
                &ctx.serenity_context().http,
                serenity::CreateInteractionResponse::UpdateMessage(
                    serenity::CreateInteractionResponseMessage::new()
                        .embed(next_embed)
                        .components(next_comps),
                ),
            )
            .await
        {
            tracing::warn!(
                error = %error,
                custom_id = %interaction.data.custom_id,
                "failed to acknowledge lyrics pagination interaction"
            );
        }
    }

    let final_embed = make_embed(current_page);
    let _ = disable_buttons(
        msg_inner,
        &ctx.serenity_context().http,
        final_embed,
        ctx_id,
        current_page,
        total_pages,
    )
    .await;

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::navigation_target;

    #[test]
    fn rapid_next_then_previous_round_trip_is_stable() {
        let page = navigation_target(42, 0, 4, "42_next", None).unwrap();
        assert_eq!(page, 1);
        let page = navigation_target(42, page, 4, "42_prev", None).unwrap();
        assert_eq!(page, 0);
    }

    #[test]
    fn page_select_accepts_only_in_range_values() {
        assert_eq!(navigation_target(7, 0, 4, "7_select", Some("3")), Some(3));
        assert_eq!(navigation_target(7, 0, 4, "7_select", Some("4")), None);
        assert_eq!(navigation_target(7, 0, 4, "7_select", Some("bad")), None);
    }

    #[test]
    fn foreign_or_unknown_component_ids_are_ignored() {
        assert_eq!(navigation_target(7, 0, 4, "8_next", None), None);
        assert_eq!(navigation_target(7, 0, 4, "7_unknown", None), None);
    }

    #[test]
    fn navigation_stays_inside_page_bounds() {
        assert_eq!(navigation_target(7, 0, 4, "7_prev", None), Some(0));
        assert_eq!(navigation_target(7, 3, 4, "7_next", None), Some(3));
        assert_eq!(navigation_target(7, 2, 4, "7_first", None), Some(0));
        assert_eq!(navigation_target(7, 1, 4, "7_last", None), Some(3));
    }
}
