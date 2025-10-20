use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

const DECAY_FACTOR: f32 = 0.95;
const RECENCY_BONUS: f32 = 4.0;
const DAY: Duration = Duration::from_secs(60 * 60 * 24);

#[derive(Debug, Default, Clone, bincode::Decode, bincode::Encode)]
pub struct LaunchHistory {
    inner: HashMap<PathBuf, LaunchStatistic>,
}

#[derive(Debug, Clone, bincode::Decode, bincode::Encode)]
struct LaunchStatistic {
    launch_score: f32,
    last_launched: SystemTime,
}

impl Default for LaunchStatistic {
    fn default() -> Self {
        Self {
            launch_score: 0.0,
            last_launched: SystemTime::UNIX_EPOCH,
        }
    }
}

impl LaunchHistory {
    pub fn score(&self, entry: &Path) -> f32 {
        let Self { inner: map } = &self;
        let Some(stat) = map.get(entry) else {
            return 0.0;
        };

        let Ok(since_last) = SystemTime::now().duration_since(stat.last_launched) else {
            // if we fail to calculate the time since this app has been launched for some reason,
            // just don't account for the recency bonus.
            return stat.launch_score;
        };

        let days_since = since_last.as_secs() / DAY.as_secs();
        // artificial bonus multiplier based on how long it has been since you last launched this
        // entry. This is a rather gradual falloff, with preference for entries launched within the
        // last day.
        let recency_bonus = match days_since {
            (0..=1) => 1.0,
            (2..=4) => 0.6,
            (5..=12) => 0.3,
            _ => 0.0
        } * RECENCY_BONUS;

        stat.launch_score + recency_bonus
    }

    pub fn increment_and_decay(&mut self, entry: PathBuf) {
        self.increment(entry);
        self.decay_all();
    }

    pub fn increment(&mut self, entry: PathBuf) {
        let stat = self.inner.entry(entry).or_default();

        stat.launch_score += 1.0;
        stat.last_launched = SystemTime::now();
    }

    pub fn decay_all(&mut self) {
        self.inner.retain(|_, stat| {
            // decay each value by a certain factor
            stat.launch_score *= DECAY_FACTOR;

            // and retain an entry only if the value hasn't grown too small
            stat.launch_score > 0.5
        });
    }
}
