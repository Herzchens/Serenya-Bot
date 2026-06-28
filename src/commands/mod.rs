pub mod audio;
pub mod cleanup;
pub mod control;
pub mod info;
pub mod invite;
pub mod loop_cmd;
pub mod media;
pub mod meta;
pub mod playback;
pub mod playlist;
pub mod queue;
pub mod settings;
pub mod stats;
pub mod voice;

use crate::utils::Error;

/// Register all bot commands.
pub fn all_commands() -> Vec<poise::Command<crate::Data, Error>> {
    vec![
        meta::ping(),
        meta::about(),
        meta::help(),
        meta::reload(),
        cleanup::cleanup(),
        audio::eight_d(),
        playback::play(),
        playback::pause(),
        playback::resume(),
        playback::stop(),
        playback::skip(),
        loop_cmd::loop_cmd(),
        info::nowplaying(),
        queue::queue(),
        queue::remove(),
        queue::clear(),
        queue::shuffle(),
        voice::join(),
        voice::leave(),
        playlist::playlist(),
        settings::quality(),
        settings::announce_track(),
        settings::prefix(),
        stats::stats(),
        control::seek(),
        control::forward(),
        control::rewind(),
        control::replay(),
        control::previous(),
        control::jump(),
        control::r#move(),
        media::lyrics(),
        media::songinfo(),
        invite::invite(),
        invite::support(),
    ]
}
