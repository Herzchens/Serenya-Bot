pub mod add;
pub mod create;
pub mod list;
pub mod manage;
pub mod play;

use crate::utils::{Context, Error};
use add::add;
use create::create;
use list::list;
use manage::{delete, info, remove, rename};
use play::play;

pub async fn autocomplete_playlist(ctx: Context<'_>, partial: &str) -> Vec<String> {
    let user_id = ctx.author().id.get();
    ctx.data()
        .database
        .get_user_playlist_names(user_id)
        .await
        .into_iter()
        .filter(|name| name.to_lowercase().contains(&partial.to_lowercase()))
        .take(25)
        .collect()
}

/// Manage your custom playlists.
#[poise::command(
    slash_command,
    prefix_command,
    subcommands("create", "add", "play", "list", "remove", "delete", "rename", "info"),
    aliases("pl"),
    subcommand_required
)]
pub async fn playlist(_ctx: Context<'_>) -> Result<(), Error> {
    Ok(())
}
