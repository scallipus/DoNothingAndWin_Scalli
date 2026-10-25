//! Source-independent view model for the global activity ticker.
//! Producers contribute snapshots; ticker presentation never drives job execution.
#[derive(Clone, Debug)]
pub(super) struct Activity {
    pub source: &'static str,
    pub title: String,
    pub detail: String,
    pub progress: Option<f32>,
}

#[derive(Default)]
pub(super) struct Registry {
    activities: Vec<Activity>,
}

impl Registry {
    pub fn register(
        &mut self,
        source: &'static str,
        title: impl Into<String>,
        detail: impl Into<String>,
        progress: Option<f32>,
    ) {
        // Distinct sources can report simultaneously without overwriting each other.
        let activity = Activity {
            source,
            title: title.into(),
            detail: detail.into(),
            progress: progress.filter(|n| n.is_finite()).map(|n| n.clamp(0., 1.)),
        };
        if let Some(existing) = self.activities.iter_mut().find(|a| a.source == source) {
            *existing = activity;
        } else {
            self.activities.push(activity);
        }
    }
    pub fn all(&self) -> &[Activity] {
        &self.activities
    }
    pub fn current(&self, elapsed: f64) -> Option<&Activity> {
        if self.activities.is_empty() {
            None
        } else {
            let index = (elapsed.max(0.) / 5.0).floor() as usize % self.activities.len();
            self.activities.get(index)
        }
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn distinct_sources_are_not_lost() {
        let mut r = Registry::default();
        r.register("zip", "Zip", "One", None);
        r.register("fab", "Fab", "Two", Some(0.5));
        r.register("zip", "Zip", "Three", None);
        assert_eq!(r.all().len(), 2);
        assert_eq!(r.current(0.).unwrap().detail, "Three");
        assert_eq!(r.current(5.).unwrap().detail, "Two");
    }
}
