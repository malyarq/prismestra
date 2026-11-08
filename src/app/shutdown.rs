//! Nonblocking close barrier: wait for every owned lighting and sensor operation to drain.
use super::runtime::Runtime;
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Phase {
    #[default]
    Open,
    Waiting,
    Ready,
}
#[derive(Default)]
pub struct Shutdown {
    phase: Phase,
}
impl Shutdown {
    pub fn phase(&self) -> Phase {
        self.phase
    }
    pub fn active(&self) -> bool {
        self.phase != Phase::Open
    }
    pub fn message(&self) -> String {
        match self.phase {
            Phase::Open => String::new(),
            Phase::Waiting => {
                "Завершаю операции подсветки и чтения датчиков. Окно закроется автоматически."
                    .into()
            }
            Phase::Ready => "Операции завершены. Закрываю приложение.".into(),
        }
    }
    pub fn request(&mut self, runtime: &mut Runtime) {
        if !self.active() {
            runtime.stop();
            self.phase = Phase::Waiting;
        }
        self.poll(runtime);
    }
    pub fn poll(&mut self, runtime: &mut Runtime) {
        if self.phase == Phase::Waiting && runtime.drained() {
            self.phase = Phase::Ready;
        }
    }
}
