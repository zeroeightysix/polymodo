use std::collections::HashMap;
use std::path::PathBuf;

pub type LaunchHistory = HashMap<PathBuf, u32>;

#[derive(Debug, Default, bincode::Decode, bincode::Encode)]
pub struct LauncherEntryBiasState {
    pub history: LaunchHistory,
}

#[expect(unused)]
pub fn bump_history_value(value: u32) -> u32 {
    const ALPHA: f32 = 0.5f32;
    const INV_ALPHA: f32 = 1f32 - ALPHA;
    let increment = 100;

    (ALPHA * increment as f32 + INV_ALPHA * value as f32) as u32
}

#[expect(unused)]
pub fn decrement_history_value(value: u32) -> u32 {
    const ALPHA: f32 = 0.1f32;
    const INV_ALPHA: f32 = 1f32 - ALPHA;
    let increment = 0;

    (ALPHA * increment as f32 + INV_ALPHA * value as f32) as u32
}
