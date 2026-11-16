//! Restarts an actor that panicked.

use std::any::Any;
use std::future::Future;
use std::time::Duration;

const FIRST_DELAY: Duration = Duration::from_secs(1);
const LONGEST_DELAY: Duration = Duration::from_secs(30);

/// The first line of a panic's message, or a fixed word when it has none.
fn first_line(payload: Box<dyn Any + Send>) -> String {
    let text = match payload.downcast::<String>() {
        Ok(s) => *s,
        Err(p) => match p.downcast::<&'static str>() {
            Ok(s) => (*s).to_string(),
            Err(_) => String::new(),
        },
    };
    match text.lines().next() {
        Some(line) if !line.is_empty() => line.to_string(),
        _ => "panicked".to_string(),
    }
}

/// Runs `make()` on its own task. If it panics, calls `failed(name, first line of the
/// message)`, waits (1 s, doubling to 30 s) and runs `make()` again. A normal return, or a
/// cancelled task, ends it.
pub async fn supervise<F, Fut>(
    name: &'static str,
    mut make: F,
    failed: impl Fn(&'static str, String) + Send + 'static,
) where
    F: FnMut() -> Fut + Send + 'static,
    Fut: Future<Output = ()> + Send + 'static,
{
    let mut delay = FIRST_DELAY;
    loop {
        match tokio::spawn(make()).await {
            Ok(()) => return,
            Err(e) if e.is_panic() => {
                failed(name, first_line(e.into_panic()));
                tokio::time::sleep(delay).await;
                delay = (delay * 2).min(LONGEST_DELAY);
            }
            Err(_) => return,
        }
    }
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::{Arc, Mutex};

    use super::*;

    #[tokio::test(start_paused = true)]
    async fn a_panicking_actor_restarts_and_is_reported() {
        let runs = Arc::new(AtomicUsize::new(0));
        let reports: Arc<Mutex<Vec<(&'static str, String)>>> = Arc::default();
        let r = runs.clone();
        let rep = reports.clone();
        tokio::time::timeout(
            Duration::from_secs(5),
            supervise(
                "x",
                move || {
                    let n = r.fetch_add(1, Ordering::SeqCst);
                    async move {
                        if n == 0 {
                            panic!("boom\nsecond line");
                        }
                    }
                },
                move |name, why| rep.lock().unwrap().push((name, why)),
            ),
        )
        .await
        .expect("supervise ends once the actor completes");
        assert_eq!(runs.load(Ordering::SeqCst), 2);
        assert_eq!(*reports.lock().unwrap(), vec![("x", "boom".to_string())]);
    }
}
