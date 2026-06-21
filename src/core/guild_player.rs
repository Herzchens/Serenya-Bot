use poise::serenity_prelude as serenity;
use songbird::tracks::TrackHandle;
use std::collections::HashSet;
use std::time::Instant;

use crate::core::loop_mode::LoopMode;
use crate::core::queue::Queue;
use crate::core::track::Track;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum PlaybackStatus {
    #[default]
    Idle,
    Playing,
    Paused,
    Stopped,
}

pub struct GuildPlayer {
    pub queue: Queue,
    pub now_playing: Option<Track>,
    pub previous_track: Option<Track>,
    pub loop_mode: LoopMode,
    pub announce_channel: Option<serenity::ChannelId>,
    pub voice_channel: Option<serenity::ChannelId>,
    pub playback_status: PlaybackStatus,
    pub current_track_handle: Option<TrackHandle>,
    pub skip_votes: HashSet<serenity::UserId>,
    pub requester_absence_timer: Option<Instant>,
    pub empty_since: Option<Instant>,
    pub seek_offset: std::time::Duration,
    pub is_seeking: bool,
    pub skip_forced: bool,
    pub eight_d_enabled: bool,
    pub consecutive_errors: usize,
}

impl GuildPlayer {
    pub fn new() -> Self {
        Self {
            queue: Queue::new(),
            now_playing: None,
            previous_track: None,
            loop_mode: LoopMode::Off,
            announce_channel: None,
            voice_channel: None,
            playback_status: PlaybackStatus::Idle,
            current_track_handle: None,
            skip_votes: HashSet::new(),
            requester_absence_timer: None,
            empty_since: None,
            seek_offset: std::time::Duration::from_secs(0),
            is_seeking: false,
            skip_forced: false,
            eight_d_enabled: false,
            consecutive_errors: 0,
        }
    }

    pub fn clear_skip_votes(&mut self) {
        self.skip_votes.clear();
        self.requester_absence_timer = None;
    }

    pub fn reset(&mut self) {
        self.queue.clear();
        self.now_playing = None;
        self.previous_track = None;
        self.loop_mode = LoopMode::Off;
        self.playback_status = PlaybackStatus::Idle;
        if let Some(handle) = self.current_track_handle.take() {
            let _ = handle.stop();
        }
        self.clear_skip_votes();
        self.empty_since = None;
        self.seek_offset = std::time::Duration::from_secs(0);
        self.is_seeking = false;
        self.skip_forced = false;
        self.eight_d_enabled = false;
        self.consecutive_errors = 0;
    }

    pub fn advance_queue(&mut self) {
        self.clear_skip_votes();
        self.seek_offset = std::time::Duration::from_secs(0);
        self.is_seeking = false;

        let effective_loop = if self.skip_forced {
            self.skip_forced = false;
            if self.loop_mode == LoopMode::Track {
                LoopMode::Off
            } else {
                self.loop_mode
            }
        } else {
            self.loop_mode
        };

        match effective_loop {
            LoopMode::Track => {
                // Keep now_playing as-is so it can be replayed
                if let Some(ref mut np) = self.now_playing {
                    np.resolved_url = None;
                }
            }
            LoopMode::Queue => {
                if let Some(mut track) = self.now_playing.take() {
                    track.resolved_url = None;
                    self.previous_track = Some(track.clone());
                    let _ = self.queue.push(track, usize::MAX);
                }
                self.now_playing = self.queue.pop_front();
            }
            LoopMode::Off => {
                if let Some(mut track) = self.now_playing.take() {
                    track.resolved_url = None;
                    self.previous_track = Some(track);
                }
                self.now_playing = self.queue.pop_front();
            }
        }

        if self.now_playing.is_none() {
            self.playback_status = PlaybackStatus::Idle;
        } else {
            self.playback_status = PlaybackStatus::Playing;
        }
    }
}
