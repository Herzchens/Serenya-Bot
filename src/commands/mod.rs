pub mod info;
pub mod loop_cmd;
pub mod meta;
pub mod playback;
pub mod voice;

use crate::utils::Error;

/// Register all bot commands.
pub fn all_commands() -> Vec<poise::Command<crate::Data, Error>> {
    vec![
        meta::ping(),
        meta::about(),
        playback::play(),
        playback::pause(),
        playback::resume(),
        playback::stop(),
        playback::skip(),
        loop_cmd::loop_cmd(),
        info::nowplaying(),
        voice::join(),
        voice::leave(),
    ]
}
