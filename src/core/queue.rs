use rand::seq::SliceRandom;
use std::collections::VecDeque;

use crate::core::track::Track;
use crate::utils::SerenyaError;

#[derive(Debug, Clone, Default)]
pub struct Queue {
    tracks: VecDeque<Track>,
}

impl Queue {
    pub fn new() -> Self {
        Self {
            tracks: VecDeque::new(),
        }
    }

    pub fn push(&mut self, track: Track, max_size: usize) -> Result<(), SerenyaError> {
        if self.tracks.len() >= max_size {
            return Err(SerenyaError::Queue(format!(
                "Queue limit of {} tracks reached.",
                max_size
            )));
        }
        self.tracks.push_back(track);
        Ok(())
    }

    pub fn push_front(&mut self, track: Track) {
        self.tracks.push_front(track);
    }

    pub fn push_batch(
        &mut self,
        tracks: Vec<Track>,
        max_size: usize,
    ) -> Result<usize, SerenyaError> {
        let available = max_size.saturating_sub(self.tracks.len());
        let to_add: Vec<Track> = tracks.into_iter().take(available).collect();
        let added = to_add.len();
        self.tracks.extend(to_add);
        Ok(added)
    }

    pub fn pop_front(&mut self) -> Option<Track> {
        self.tracks.pop_front()
    }

    pub fn remove(&mut self, index: usize) -> Result<Track, SerenyaError> {
        if index >= self.tracks.len() {
            return Err(SerenyaError::Queue(format!(
                "Index {} out of bounds for queue of length {}.",
                index,
                self.tracks.len()
            )));
        }
        self.tracks
            .remove(index)
            .ok_or_else(|| SerenyaError::Queue("Failed to remove track from queue.".into()))
    }

    pub fn move_item(&mut self, from: usize, to: usize) -> Result<(), SerenyaError> {
        let len = self.tracks.len();
        if from >= len || to >= len {
            return Err(SerenyaError::Queue(format!(
                "Invalid move coordinates: {} -> {} in queue of length {}.",
                from, to, len
            )));
        }
        if from == to {
            return Ok(());
        }
        if let Some(track) = self.tracks.remove(from) {
            self.tracks.insert(to, track);
            Ok(())
        } else {
            Err(SerenyaError::Queue("Failed to move track in queue.".into()))
        }
    }

    pub fn shuffle(&mut self) {
        let mut rng = rand::rng();
        self.tracks.make_contiguous().shuffle(&mut rng);
    }

    pub fn clear(&mut self) {
        self.tracks.clear();
        self.tracks.shrink_to_fit();
    }

    pub fn jump(&mut self, index: usize) -> Result<Vec<Track>, SerenyaError> {
        if index >= self.tracks.len() {
            return Err(SerenyaError::Queue(format!(
                "Jump index {} out of bounds for queue of length {}.",
                index,
                self.tracks.len()
            )));
        }
        let skipped: Vec<Track> = self.tracks.drain(0..index).collect();
        Ok(skipped)
    }

    pub fn get(&self, index: usize) -> Option<&Track> {
        self.tracks.get(index)
    }

    pub fn get_mut(&mut self, index: usize) -> Option<&mut Track> {
        self.tracks.get_mut(index)
    }

    pub fn len(&self) -> usize {
        self.tracks.len()
    }

    pub fn is_empty(&self) -> bool {
        self.tracks.is_empty()
    }

    pub fn iter(&self) -> impl Iterator<Item = &Track> {
        self.tracks.iter()
    }

    pub fn page(&self, page: usize, per_page: usize) -> Vec<Track> {
        let start = page * per_page;
        self.tracks
            .iter()
            .skip(start)
            .take(per_page)
            .cloned()
            .collect()
    }
}

#[cfg(test)]
mod acceptance_tests {
    use super::Queue;
    use crate::core::{SourceType, Track};
    use poise::serenity_prelude as serenity;
    use std::sync::Arc;

    fn track(title: &str, id: u64) -> Track {
        Track {
            title: title.into(),
            url: format!("https://example.invalid/{id}").into(),
            duration: None,
            requester_name: None,
            thumbnail: None,
            source_provider: Arc::<str>::from("test"),
            resolved_url: None,
            requester_id: serenity::UserId::new(id),
            source_type: SourceType::Search,
        }
    }

    fn titles(queue: &Queue) -> Vec<&str> {
        queue.iter().map(|track| track.title.as_ref()).collect()
    }

    fn ids(queue: &Queue) -> Vec<u64> {
        queue.iter().map(|track| track.requester_id.get()).collect()
    }

    #[test]
    fn jump_to_front_preserves_target_and_order() {
        let mut queue = Queue::new();
        queue
            .push_batch(vec![track("a", 1), track("b", 2), track("c", 3)], 10)
            .unwrap();

        let skipped = queue.jump(0).unwrap();

        assert!(skipped.is_empty());
        assert_eq!(titles(&queue), vec!["a", "b", "c"]);
        assert_eq!(ids(&queue), vec![1, 2, 3]);
    }

    #[test]
    fn jump_to_middle_drains_exact_prefix_and_keeps_target_front() {
        let mut queue = Queue::new();
        queue
            .push_batch(
                vec![track("a", 1), track("b", 2), track("c", 3), track("d", 4)],
                10,
            )
            .unwrap();

        let skipped = queue.jump(2).unwrap();

        assert_eq!(
            skipped
                .iter()
                .map(|track| track.requester_id.get())
                .collect::<Vec<_>>(),
            vec![1, 2]
        );
        assert_eq!(titles(&queue), vec!["c", "d"]);
        assert_eq!(ids(&queue), vec![3, 4]);
    }

    #[test]
    fn out_of_bounds_jump_is_atomic() {
        let mut queue = Queue::new();
        queue
            .push_batch(vec![track("a", 1), track("b", 2), track("c", 3)], 10)
            .unwrap();

        assert!(queue.jump(3).is_err());
        assert_eq!(ids(&queue), vec![1, 2, 3]);
    }

    #[test]
    fn move_preserves_duplicate_track_identity_and_relative_order() {
        let mut queue = Queue::new();
        queue
            .push_batch(
                vec![
                    track("dup", 11),
                    track("middle", 12),
                    track("dup", 13),
                    track("tail", 14),
                ],
                10,
            )
            .unwrap();

        queue.move_item(2, 0).unwrap();

        assert_eq!(titles(&queue), vec!["dup", "dup", "middle", "tail"]);
        assert_eq!(ids(&queue), vec![13, 11, 12, 14]);
    }

    #[test]
    fn move_forward_uses_destination_position_after_removal() {
        let mut queue = Queue::new();
        queue
            .push_batch(
                vec![track("a", 1), track("b", 2), track("c", 3), track("d", 4)],
                10,
            )
            .unwrap();

        queue.move_item(0, 2).unwrap();

        assert_eq!(ids(&queue), vec![2, 3, 1, 4]);
    }

    #[test]
    fn invalid_move_is_atomic() {
        let mut queue = Queue::new();
        queue
            .push_batch(vec![track("a", 1), track("b", 2), track("c", 3)], 10)
            .unwrap();

        assert!(queue.move_item(1, 3).is_err());
        assert_eq!(ids(&queue), vec![1, 2, 3]);
    }
}
