use std::future::Future;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

pub type Phases = Arc<Mutex<Vec<(&'static str, Duration)>>>;

tokio::task_local! {
    pub static PHASES: Phases;
}

pub fn record(name: &'static str, started: Instant) {
    let elapsed = started.elapsed();
    let _ = PHASES.try_with(|phases| {
        if let Ok(mut list) = phases.lock() {
            list.push((name, elapsed));
        }
    });
}

pub async fn timed<F: Future>(name: &'static str, fut: F) -> F::Output {
    let started = Instant::now();
    let out = fut.await;
    record(name, started);
    out
}

pub fn header_value(total: Duration, phases: &[(&'static str, Duration)]) -> String {
    let mut parts = Vec::with_capacity(phases.len() + 1);
    for (name, dur) in phases {
        parts.push(format!("{name};dur={:.1}", dur.as_secs_f64() * 1000.0));
    }
    parts.push(format!("app;dur={:.1}", total.as_secs_f64() * 1000.0));
    parts.join(", ")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn header_lists_phases_then_total() {
        let v = header_value(
            Duration::from_millis(120),
            &[("db1", Duration::from_micros(40_250))],
        );
        assert_eq!(v, "db1;dur=40.2, app;dur=120.0");
    }

    #[tokio::test]
    async fn timed_records_inside_scope_and_is_noop_outside() {
        assert_eq!(timed("outside", async { 7 }).await, 7);
        let phases: Phases = Arc::default();
        PHASES
            .scope(phases.clone(), async {
                timed("inside", async {}).await;
            })
            .await;
        let names: Vec<_> = phases.lock().unwrap().iter().map(|p| p.0).collect();
        assert_eq!(names, vec!["inside"]);
    }
}
