//! The play queue: what is playing, what comes next, shuffle and repeat.
//!
//! Shuffle reorders only what is still to come, so Previous walks back
//! through what was actually heard. The track the player was told comes
//! next is the one that comes next, also when repeat wraps a shuffled queue.

use rand::seq::SliceRandom;
use serde::{Deserialize, Serialize};

use crate::model::Item;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum Repeat {
    #[default]
    Off,
    All,
    One,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Queue {
    pub tracks: Vec<Item>,
    pub index: Option<usize>,
    pub shuffle: bool,
    pub repeat: Repeat,
}

impl Queue {
    pub fn current(&self) -> Option<&Item> {
        self.tracks.get(self.index?)
    }

    pub fn upcoming(&self) -> &[Item] {
        let from = self.index.map_or(0, |i| i + 1).min(self.tracks.len());
        &self.tracks[from..]
    }

    /// Plays `tracks` from `start`; with shuffle on, the rest come shuffled.
    pub fn replace(&mut self, tracks: Vec<Item>, start: usize) {
        if tracks.is_empty() {
            return;
        }
        let start = start.min(tracks.len() - 1);
        self.tracks = tracks;
        self.index = Some(start);
        if self.shuffle {
            self.tracks[start + 1..].shuffle(&mut rand::rng());
        }
    }

    pub fn play_next(&mut self, item: Item) {
        let at = self.index.map_or(0, |i| i + 1);
        self.tracks.insert(at, item);
    }

    pub fn add(&mut self, items: impl IntoIterator<Item = Item>) {
        self.tracks.extend(items);
    }

    /// Removes a row. Returns true when it was the playing one; the row that
    /// moved into its place is then current.
    pub fn remove(&mut self, at: usize) -> bool {
        if at >= self.tracks.len() {
            return false;
        }
        self.tracks.remove(at);
        let Some(index) = self.index else {
            return false;
        };
        if at < index {
            self.index = Some(index - 1);
            false
        } else if at == index {
            self.index = (!self.tracks.is_empty()).then(|| index.min(self.tracks.len() - 1));
            true
        } else {
            false
        }
    }

    pub fn clear_upcoming(&mut self) {
        let keep = self.index.map_or(0, |i| i + 1);
        self.tracks.truncate(keep);
    }

    pub fn set_shuffle(&mut self, on: bool) {
        self.shuffle = on;
        if on {
            let from = self.index.map_or(0, |i| i + 1).min(self.tracks.len());
            self.tracks[from..].shuffle(&mut rand::rng());
        }
    }

    pub fn cycle_repeat(&mut self) {
        self.repeat = match self.repeat {
            Repeat::Off => Repeat::All,
            Repeat::All => Repeat::One,
            Repeat::One => Repeat::Off,
        };
    }

    /// Where playback goes when the current track ends by itself.
    fn next_index(&self, by_user: bool) -> Option<usize> {
        let index = self.index?;
        if self.repeat == Repeat::One && !by_user {
            return Some(index);
        }
        if index + 1 < self.tracks.len() {
            Some(index + 1)
        } else if self.repeat != Repeat::Off && !self.tracks.is_empty() {
            Some(0)
        } else {
            None
        }
    }

    /// The track that follows when the current one ends by itself.
    pub fn peek_next(&self) -> Option<&Item> {
        self.tracks.get(self.next_index(false)?)
    }

    /// Moves on when a track ends by itself (`by_user` false) or on Next.
    /// Next leaves a repeated track for the one after it.
    pub fn advance(&mut self, by_user: bool) -> Option<&Item> {
        let next = self.next_index(by_user)?;
        let wrapped = self
            .index
            .is_some_and(|i| next == 0 && i + 1 >= self.tracks.len());
        if wrapped && self.shuffle && self.tracks.len() > 2 {
            // A new order for the next pass, keeping the first track first:
            // it is the one the player was told comes next.
            self.tracks[1..].shuffle(&mut rand::rng());
        }
        self.index = Some(next);
        self.current()
    }

    pub fn previous(&mut self) -> Option<&Item> {
        let index = self.index?;
        self.index = Some(index.saturating_sub(1));
        self.current()
    }

    pub fn jump(&mut self, at: usize) -> Option<&Item> {
        if at < self.tracks.len() {
            self.index = Some(at);
        }
        self.current()
    }

    pub fn is_last(&self) -> bool {
        self.index.is_some_and(|i| i + 1 >= self.tracks.len())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::Kind;

    fn track(id: &str) -> Item {
        Item {
            kind: Kind::Song,
            id: id.into(),
            title: id.into(),
            subtitle: String::new(),
            thumbnails: Vec::new(),
            artists: Vec::new(),
            album: None,
            duration: None,
            explicit: false,
            play_video_id: None,
        }
    }

    fn ids(queue: &Queue) -> Vec<&str> {
        queue.tracks.iter().map(|t| t.id.as_str()).collect()
    }

    fn queue(names: &[&str], start: usize) -> Queue {
        let mut queue = Queue::default();
        queue.replace(names.iter().map(|n| track(n)).collect(), start);
        queue
    }

    #[test]
    fn plays_through_and_stops() {
        let mut q = queue(&["a", "b"], 0);
        assert_eq!(q.peek_next().map(|t| t.id.as_str()), Some("b"));
        assert_eq!(q.advance(false).map(|t| t.id.as_str()), Some("b"));
        assert!(q.peek_next().is_none());
        assert!(q.advance(false).is_none());
        assert_eq!(q.current().map(|t| t.id.as_str()), Some("b"));
    }

    #[test]
    fn repeat_one_replays_but_next_moves_on() {
        let mut q = queue(&["a", "b"], 0);
        q.repeat = Repeat::One;
        assert_eq!(q.peek_next().map(|t| t.id.as_str()), Some("a"));
        assert_eq!(q.advance(false).map(|t| t.id.as_str()), Some("a"));
        assert_eq!(q.advance(true).map(|t| t.id.as_str()), Some("b"));
        assert_eq!(q.advance(true).map(|t| t.id.as_str()), Some("a"));
    }

    #[test]
    fn repeat_all_wraps_to_the_promised_track() {
        let mut q = queue(&["a", "b", "c", "d", "e"], 4);
        q.repeat = Repeat::All;
        q.shuffle = true;
        let promised = q.peek_next().cloned();
        assert_eq!(q.advance(false).cloned(), promised);
        assert_eq!(q.index, Some(0));
        let mut sorted = ids(&q);
        sorted.sort();
        assert_eq!(sorted, ["a", "b", "c", "d", "e"]);
    }

    #[test]
    fn shuffle_keeps_history_and_current() {
        let names: Vec<String> = (0..20).map(|i| format!("t{i}")).collect();
        let names: Vec<&str> = names.iter().map(String::as_str).collect();
        let mut q = queue(&names, 5);
        q.set_shuffle(true);
        assert_eq!(&ids(&q)[..6], &names[..6]);
        let mut rest = ids(&q)[6..].to_vec();
        rest.sort();
        let mut expected = names[6..].to_vec();
        expected.sort();
        assert_eq!(rest, expected);
    }

    #[test]
    fn replace_with_shuffle_starts_on_the_chosen_track() {
        let mut q = Queue {
            shuffle: true,
            ..Queue::default()
        };
        q.replace(["a", "b", "c", "d"].iter().map(|n| track(n)).collect(), 2);
        assert_eq!(q.current().map(|t| t.id.as_str()), Some("c"));
        assert_eq!(&ids(&q)[..2], ["a", "b"]);
    }

    #[test]
    fn editing_keeps_the_current_track() {
        let mut q = queue(&["a", "b", "c"], 1);
        q.play_next(track("x"));
        assert_eq!(ids(&q), ["a", "b", "x", "c"]);
        assert!(!q.remove(0));
        assert_eq!(q.current().map(|t| t.id.as_str()), Some("b"));
        assert!(q.remove(0));
        assert_eq!(q.current().map(|t| t.id.as_str()), Some("x"));
        q.clear_upcoming();
        assert_eq!(ids(&q), ["x"]);
        assert!(q.remove(0));
        assert_eq!(q.index, None);
    }

    #[test]
    fn previous_stops_at_the_first_track() {
        let mut q = queue(&["a", "b"], 1);
        assert_eq!(q.previous().map(|t| t.id.as_str()), Some("a"));
        assert_eq!(q.previous().map(|t| t.id.as_str()), Some("a"));
    }
}
