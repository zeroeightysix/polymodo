use crate::persistence::StorableState;

#[derive(Debug, bincode::Decode, bincode::Encode)]
pub struct LauncherSettings {
    pub transparency: f32,
}

impl LauncherSettings {
    pub fn sanitize(mut self) -> Self {
        self.transparency = self.transparency.clamp(0.0, 1.0);

        self
    }
}

impl Default for LauncherSettings {
    fn default() -> Self {
        Self { transparency: 0.2 }
    }
}

impl StorableState for LauncherSettings {
    const NAME: &'static str = "settings";
}
