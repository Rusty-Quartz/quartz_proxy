use async_lock::Mutex;
use std::time::Instant;

pub struct Timings(Mutex<TimingsImpl>);

impl Timings {
    pub const fn new(weight: f64) -> Self {
        Timings(Mutex::new(TimingsImpl {
            average: 0.0,
            weight,
        }))
    }

    pub fn spawn_timer(&self) -> Timer<'_> {
        Timer {
            start: Instant::now(),
            owner: self,
        }
    }

    pub async fn average(&self) -> f64 {
        self.0.lock().await.average
    }
}

pub struct Timer<'a> {
    start: Instant,
    owner: &'a Timings,
}

impl<'a> Timer<'a> {
    pub async fn finish(self, num_bytes: usize) {
        let elapsed = self.start.elapsed();
        self.owner
            .0
            .lock()
            .await
            .update((num_bytes * 8) as f64 / (1000000.0 * elapsed.as_secs_f64()));
    }
}

struct TimingsImpl {
    average: f64,
    weight: f64,
}

impl TimingsImpl {
    fn update(&mut self, megabits_per_second: f64) {
        self.average = (self.weight * self.average + megabits_per_second) / (self.weight + 1.0);
    }
}
