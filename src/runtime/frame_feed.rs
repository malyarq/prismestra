//! Bounded latest-frame slot. Readers cannot invoke a provider or touch native I/O.
use super::{cadence_wait::Signal, *};
use std::sync::{Arc, Mutex, Weak};
#[derive(Clone, Debug)]
pub struct Route {
    pub leds: usize,
    pub stale_after_ms: u64,
}
#[derive(Clone, Debug)]
pub struct Frame {
    pub colors: Arc<[Rgb]>,
    pub observed_ms: Option<u64>,
}
#[derive(Clone, Debug)]
pub struct Snapshot {
    pub generation: u64,
    pub revision: u64,
    pub closed: bool,
    pub frames: BTreeMap<String, Frame>,
}
struct State {
    latest: Arc<Snapshot>,
    high_water: BTreeMap<String, u64>,
    published_ms: Option<u64>,
    wakes: Vec<Weak<Signal>>,
}
struct Shared {
    routes: BTreeMap<String, Route>,
    state: Mutex<State>,
}
pub struct Publisher(Arc<Shared>);
#[derive(Clone)]
pub struct Reader(Arc<Shared>);
pub fn channel(
    generation: u64,
    routes: BTreeMap<String, Route>,
) -> Result<(Publisher, Reader), String> {
    if generation == 0
        || routes.len() > crate::devices::MAX_DEVICES
        || routes.iter().any(|(id, r)| {
            id.is_empty()
                || id.len() > 64
                || !id
                    .bytes()
                    .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'_' | b'-' | b'.'))
                || r.leds == 0
                || r.leds > 65536
                || !(100..=60000).contains(&r.stale_after_ms)
        })
    {
        return Err("Invalid external frame feed routes/generation".into());
    }
    let shared = Arc::new(Shared {
        routes,
        state: Mutex::new(State {
            latest: Arc::new(Snapshot {
                generation,
                revision: 0,
                closed: false,
                frames: BTreeMap::new(),
            }),
            high_water: BTreeMap::new(),
            published_ms: None,
            wakes: vec![],
        }),
    });
    Ok((Publisher(shared.clone()), Reader(shared)))
}
fn notify(state: &mut State) {
    state.wakes.retain(|wake| {
        if let Some(wake) = wake.upgrade() {
            wake.notify();
            true
        } else {
            false
        }
    });
}
impl Publisher {
    pub fn publish(&self, frames: BTreeMap<String, Frame>, at_ms: u64) -> Result<(), String> {
        if frames.iter().any(|(id, f)| {
            self.0
                .routes
                .get(id)
                .is_none_or(|r| f.colors.len() != r.leds)
                || f.observed_ms.is_some_and(|at| at > at_ms)
        }) {
            return Err("External frame feed dimensions/age rejected".into());
        }
        let mut state = self.0.state.lock().unwrap_or_else(|e| e.into_inner());
        if state.latest.closed {
            return Err("External frame feed is closed".into());
        }
        if state.published_ms.is_some_and(|old| at_ms < old)
            || frames.iter().any(|(id, f)| {
                f.observed_ms
                    .is_some_and(|at| state.high_water.get(id).is_some_and(|old| at < *old))
            })
        {
            return Err("External frame feed observation moved backwards".into());
        }
        let revision = state
            .latest
            .revision
            .checked_add(1)
            .ok_or("External frame feed revision exhausted")?;
        let changed = frames.len() != state.latest.frames.len()
            || frames.iter().any(|(id, f)| {
                state.latest.frames.get(id).is_none_or(|old| {
                    old.observed_ms.is_some() != f.observed_ms.is_some()
                        || (!Arc::ptr_eq(&old.colors, &f.colors) && old.colors != f.colors)
                })
            });
        for (id, f) in &frames {
            if let Some(at) = f.observed_ms {
                state.high_water.insert(id.clone(), at);
            }
        }
        state.published_ms = Some(at_ms);
        state.latest = Arc::new(Snapshot {
            generation: state.latest.generation,
            revision,
            closed: false,
            frames,
        });
        // Heartbeats extend freshness without waking the RGB clock for identical pixels.
        if changed {
            notify(&mut state);
        }
        Ok(())
    }
    pub fn clear(&self) {
        self.invalidate(false);
    }
    pub fn close(&self) {
        self.invalidate(true);
    }
    fn invalidate(&self, closed: bool) {
        let mut state = self.0.state.lock().unwrap_or_else(|e| e.into_inner());
        if state.latest.closed {
            return;
        }
        state.latest = Arc::new(Snapshot {
            generation: state.latest.generation,
            revision: state.latest.revision.saturating_add(1),
            closed,
            frames: BTreeMap::new(),
        });
        notify(&mut state);
    }
}
impl Drop for Publisher {
    fn drop(&mut self) {
        self.close();
    }
}
impl Reader {
    pub fn snapshot(&self) -> Arc<Snapshot> {
        self.0
            .state
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .latest
            .clone()
    }
    pub fn routes(&self) -> &BTreeMap<String, Route> {
        &self.0.routes
    }
    pub(crate) fn subscribe(&self, wake: &Arc<Signal>) -> Result<(), String> {
        let mut state = self.0.state.lock().unwrap_or_else(|e| e.into_inner());
        state.wakes.retain(|w| w.strong_count() > 0);
        if state.wakes.iter().any(|w| w.ptr_eq(&Arc::downgrade(wake))) {
            return Ok(());
        }
        if state.wakes.len() >= crate::devices::MAX_DEVICES {
            return Err("External feed subscriber limit exceeded".into());
        }
        state.wakes.push(Arc::downgrade(wake));
        Ok(())
    }
}
#[cfg(test)]
mod tests;
