pub mod info;
pub mod loop_cmd;
pub mod meta;
pub mod playback;
pub mod playlist;
pub mod queue;
pub mod settings;
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
        info::search(),
        queue::queue(),
        queue::remove(),
        queue::clear(),
        queue::shuffle(),
        voice::join(),
        voice::leave(),
        playlist::playlist(),
        settings::settings(),
    ]
}
