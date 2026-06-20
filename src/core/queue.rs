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
        let to_add = tracks.into_iter().take(available);
        let mut added = 0;
        for track in to_add {
            self.tracks.push_back(track);
            added += 1;
        }
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
        let mut vec: Vec<Track> = self.tracks.drain(..).collect();
        vec.shuffle(&mut rng);
        self.tracks = vec.into();
    }

    pub fn clear(&mut self) {
        self.tracks.clear();
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
