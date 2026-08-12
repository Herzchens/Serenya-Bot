use poise::serenity_prelude as serenity;
use songbird::tracks::TrackHandle;
use std::collections::HashSet;
use std::time::Instant;
use tokio_util::sync::CancellationToken;

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

const MAX_TRACK_RETRIES: u8 = 1;
const MAX_CONSECUTIVE_FAILED_TRACKS: usize = 3;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PlaybackFailureAction {
    RetryCurrent,
    Advance,
    Abort,
}

#[derive(Debug, Default)]
pub struct PlaybackFailureState {
    active_handle: Option<uuid::Uuid>,
    terminal_claimed: bool,
    retries_for_current: u8,
    failed_tracks: usize,
    retry_excluded_client: Option<String>,
}

impl PlaybackFailureState {
    pub fn begin_attempt(&mut self, handle_uuid: uuid::Uuid) {
        self.active_handle = Some(handle_uuid);
        self.terminal_claimed = false;
    }

    pub fn matches_active(&self, handle_uuid: uuid::Uuid) -> bool {
        self.active_handle == Some(handle_uuid)
    }

    pub fn set_retry_excluded_client(&mut self, client: Option<String>) {
        self.retry_excluded_client = client;
    }

    pub fn retry_excluded_client(&self) -> Option<&str> {
        self.retry_excluded_client.as_deref()
    }

    pub fn claim_terminal(&mut self, handle_uuid: uuid::Uuid) -> bool {
        if !self.matches_active(handle_uuid) || self.terminal_claimed {
            return false;
        }
        self.terminal_claimed = true;
        true
    }

    pub fn register_failure(&mut self) -> PlaybackFailureAction {
        if self.retries_for_current < MAX_TRACK_RETRIES {
            self.retries_for_current += 1;
            return PlaybackFailureAction::RetryCurrent;
        }

        self.retries_for_current = 0;
        self.retry_excluded_client = None;
        self.failed_tracks += 1;
        if self.failed_tracks >= MAX_CONSECUTIVE_FAILED_TRACKS {
            PlaybackFailureAction::Abort
        } else {
            PlaybackFailureAction::Advance
        }
    }

    pub fn mark_stable_success(&mut self, handle_uuid: uuid::Uuid) {
        if self.matches_active(handle_uuid) {
            self.retries_for_current = 0;
            self.failed_tracks = 0;
            self.retry_excluded_client = None;
        }
    }

    pub fn mark_completed(&mut self, handle_uuid: uuid::Uuid) {
        if self.matches_active(handle_uuid) {
            self.retries_for_current = 0;
            self.failed_tracks = 0;
            self.retry_excluded_client = None;
        }
    }

    pub fn clear_active_attempt(&mut self) {
        self.active_handle = None;
        self.terminal_claimed = false;
        self.retry_excluded_client = None;
    }

    pub fn reset(&mut self) {
        *self = Self::default();
    }

    #[cfg(test)]
    fn retries_for_current(&self) -> u8 {
        self.retries_for_current
    }

    #[cfg(test)]
    fn failed_tracks(&self) -> usize {
        self.failed_tracks
    }
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
    pub failure_state: PlaybackFailureState,
    pub prefetch_cancel: Option<CancellationToken>,
    pub prefetch_generation: u64,
    pub bot_voice_generation: u64,
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
            failure_state: PlaybackFailureState::default(),
            prefetch_cancel: None,
            prefetch_generation: 0,
            bot_voice_generation: 0,
        }
    }

    pub fn cancel_prefetch(&mut self) {
        if let Some(cancel) = self.prefetch_cancel.take() {
            cancel.cancel();
        }
        self.prefetch_generation = self.prefetch_generation.wrapping_add(1);
    }

    pub fn start_prefetch(&mut self) -> (CancellationToken, u64) {
        self.cancel_prefetch();
        let token = CancellationToken::new();
        self.prefetch_cancel = Some(token.clone());
        (token, self.prefetch_generation)
    }

    pub fn clear_skip_votes(&mut self) {
        self.skip_votes.clear();
        self.skip_votes.shrink_to_fit();
        self.requester_absence_timer = None;
    }

    pub fn reset(&mut self) {
        self.cancel_prefetch();
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
        self.failure_state.reset();
    }

    pub fn advance_queue(&mut self) {
        self.cancel_prefetch();
        self.clear_skip_votes();
        self.seek_offset = std::time::Duration::from_secs(0);
        self.is_seeking = false;
        self.failure_state.clear_active_attempt();

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

        self.playback_status = PlaybackStatus::Idle;
    }
}

#[cfg(test)]
mod tests {
    use super::{PlaybackFailureAction, PlaybackFailureState};

    #[test]
    fn terminal_event_can_only_be_claimed_once_per_handle() {
        let mut state = PlaybackFailureState::default();
        let handle = uuid::Uuid::from_u128(1);
        state.begin_attempt(handle);

        assert!(state.claim_terminal(handle));
        assert!(!state.claim_terminal(handle));
    }

    #[test]
    fn error_then_end_only_claims_one_terminal_transition() {
        let mut state = PlaybackFailureState::default();
        let handle = uuid::Uuid::from_u128(10);
        state.begin_attempt(handle);

        assert!(state.claim_terminal(handle));
        assert!(!state.claim_terminal(handle));
    }

    #[test]
    fn end_then_error_only_claims_one_terminal_transition() {
        let mut state = PlaybackFailureState::default();
        let handle = uuid::Uuid::from_u128(11);
        state.begin_attempt(handle);

        assert!(state.claim_terminal(handle));
        assert!(!state.claim_terminal(handle));
    }

    #[test]
    fn reset_invalidates_terminal_events_from_the_old_handle() {
        let mut state = PlaybackFailureState::default();
        let handle = uuid::Uuid::from_u128(12);
        state.begin_attempt(handle);
        state.reset();

        assert!(!state.claim_terminal(handle));
    }

    #[test]
    fn stale_terminal_event_is_rejected() {
        let mut state = PlaybackFailureState::default();
        let old_handle = uuid::Uuid::from_u128(1);
        let current_handle = uuid::Uuid::from_u128(2);
        state.begin_attempt(current_handle);

        assert!(!state.claim_terminal(old_handle));
        assert!(state.claim_terminal(current_handle));
    }

    #[test]
    fn one_track_retries_once_before_advancing() {
        let mut state = PlaybackFailureState::default();

        assert_eq!(
            state.register_failure(),
            PlaybackFailureAction::RetryCurrent
        );
        assert_eq!(state.retries_for_current(), 1);
        assert_eq!(state.register_failure(), PlaybackFailureAction::Advance);
        assert_eq!(state.retries_for_current(), 0);
        assert_eq!(state.failed_tracks(), 1);
    }

    #[test]
    fn three_distinct_failed_tracks_abort_playback() {
        let mut state = PlaybackFailureState::default();

        for expected in [
            PlaybackFailureAction::Advance,
            PlaybackFailureAction::Advance,
            PlaybackFailureAction::Abort,
        ] {
            assert_eq!(
                state.register_failure(),
                PlaybackFailureAction::RetryCurrent
            );
            assert_eq!(state.register_failure(), expected);
            state.clear_active_attempt();
        }

        assert_eq!(state.failed_tracks(), 3);
    }

    #[test]
    fn stable_success_resets_failed_track_streak() {
        let mut state = PlaybackFailureState::default();
        assert_eq!(
            state.register_failure(),
            PlaybackFailureAction::RetryCurrent
        );
        assert_eq!(state.register_failure(), PlaybackFailureAction::Advance);
        assert_eq!(state.failed_tracks(), 1);

        let handle = uuid::Uuid::from_u128(1);
        state.begin_attempt(handle);
        state.mark_stable_success(handle);

        assert_eq!(state.failed_tracks(), 0);
        assert_eq!(state.retries_for_current(), 0);
        assert_eq!(
            state.register_failure(),
            PlaybackFailureAction::RetryCurrent
        );
        assert_eq!(state.register_failure(), PlaybackFailureAction::Advance);
    }
}

#[cfg(test)]
mod retry_client_scope_tests {
    use super::{PlaybackFailureAction, PlaybackFailureState};

    #[test]
    fn failed_client_exclusion_is_scoped_to_one_retry() {
        let mut state = PlaybackFailureState::default();
        assert_eq!(
            state.register_failure(),
            PlaybackFailureAction::RetryCurrent
        );
        state.set_retry_excluded_client(Some("ANDROID_VR".to_owned()));
        assert_eq!(state.retry_excluded_client(), Some("ANDROID_VR"));

        assert_eq!(state.register_failure(), PlaybackFailureAction::Advance);
        assert_eq!(state.retry_excluded_client(), None);
    }

    #[test]
    fn changing_tracks_clears_retry_client_exclusion() {
        let mut state = PlaybackFailureState::default();
        state.set_retry_excluded_client(Some("ANDROID_VR".to_owned()));
        state.clear_active_attempt();
        assert_eq!(state.retry_excluded_client(), None);
    }
}
