//! Cancellable background jobs with progress reporting.
//!
//! A job runs on its own thread. The caller keeps a [`JobHandle`] to poll
//! progress, request cancellation and collect the result; the job body gets a
//! [`JobContext`] to report progress and to notice cancellation between items.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Receiver, Sender};
use std::sync::Arc;
use std::thread::JoinHandle;

use crate::error::{Error, Result};

/// One progress update from a running job.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Progress {
    pub done: usize,
    pub total: usize,
    pub message: String,
}

impl Progress {
    /// Completed fraction in `0.0..=1.0`.
    pub fn fraction(&self) -> f32 {
        if self.total == 0 {
            0.0
        } else {
            (self.done as f32 / self.total as f32).min(1.0)
        }
    }
}

/// Handed to the job body.
#[derive(Debug, Clone, Default)]
pub struct JobContext {
    cancel: Arc<AtomicBool>,
    progress: Option<Sender<Progress>>,
}

impl JobContext {
    /// A context that is never cancelled and discards progress, for
    /// synchronous callers such as the CLI.
    pub fn detached() -> Self {
        Self::default()
    }

    pub fn is_cancelled(&self) -> bool {
        self.cancel.load(Ordering::Relaxed)
    }

    /// `Err(Error::Cancelled)` once cancellation was requested.
    pub fn check(&self) -> Result<()> {
        if self.is_cancelled() {
            Err(Error::Cancelled)
        } else {
            Ok(())
        }
    }

    pub fn report(&self, done: usize, total: usize, message: impl Into<String>) {
        if let Some(progress) = &self.progress {
            let _ = progress.send(Progress {
                done,
                total,
                message: message.into(),
            });
        }
    }
}

/// A cloneable switch that cancels its job.
#[derive(Debug, Clone)]
pub struct Canceller(Arc<AtomicBool>);

impl Canceller {
    pub fn cancel(&self) {
        self.0.store(true, Ordering::Relaxed);
    }
}

/// The caller's side of a running job.
#[derive(Debug)]
pub struct JobHandle<T> {
    pub name: String,
    cancel: Arc<AtomicBool>,
    progress: Receiver<Progress>,
    latest: Progress,
    thread: Option<JoinHandle<Result<T>>>,
}

impl<T> JobHandle<T> {
    /// Ask the job to stop at its next checkpoint.
    pub fn cancel(&self) {
        self.cancel.store(true, Ordering::Relaxed);
    }

    pub fn canceller(&self) -> Canceller {
        Canceller(self.cancel.clone())
    }

    /// The most recent progress update, draining any queued ones.
    pub fn progress(&mut self) -> &Progress {
        while let Ok(update) = self.progress.try_recv() {
            self.latest = update;
        }
        &self.latest
    }

    pub fn is_finished(&self) -> bool {
        self.thread.as_ref().is_none_or(JoinHandle::is_finished)
    }

    /// Block until the job ends and return its result.
    pub fn join(mut self) -> Result<T> {
        match self.thread.take() {
            Some(thread) => thread
                .join()
                .unwrap_or_else(|_| Err(Error::Other(format!("job `{}` panicked", self.name)))),
            None => Err(Error::Other(format!("job `{}` already joined", self.name))),
        }
    }
}

/// Run `body` on a background thread.
pub fn spawn<T, F>(name: impl Into<String>, body: F) -> JobHandle<T>
where
    T: Send + 'static,
    F: FnOnce(&JobContext) -> Result<T> + Send + 'static,
{
    let name = name.into();
    let cancel = Arc::new(AtomicBool::new(false));
    let (sender, receiver) = mpsc::channel();
    let context = JobContext {
        cancel: cancel.clone(),
        progress: Some(sender),
    };
    let thread = std::thread::Builder::new()
        .name(name.clone())
        .spawn(move || body(&context));
    let thread = match thread {
        Ok(thread) => Some(thread),
        Err(_) => None,
    };
    JobHandle {
        name,
        cancel,
        progress: receiver,
        latest: Progress::default(),
        thread,
    }
}

/// The outcome of one item in a batch: the batch keeps going past failures.
#[derive(Debug)]
pub struct ItemResult<K, T> {
    pub item: K,
    pub result: Result<T>,
}

/// Apply `step` to every item, reporting progress and stopping early (keeping
/// the results so far) when the job is cancelled.
pub fn for_each<K, T>(
    context: &JobContext,
    items: impl IntoIterator<Item = K>,
    label: impl Fn(&K) -> String,
    mut step: impl FnMut(&K) -> Result<T>,
) -> Vec<ItemResult<K, T>> {
    let items: Vec<K> = items.into_iter().collect();
    let total = items.len();
    let mut results = Vec::with_capacity(total);
    for (index, item) in items.into_iter().enumerate() {
        if context.is_cancelled() {
            break;
        }
        context.report(index, total, label(&item));
        let result = step(&item);
        results.push(ItemResult { item, result });
    }
    context.report(results.len(), total, String::new());
    results
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reports_progress_and_result() {
        let mut job = spawn("count", |context| {
            let results = for_each(context, 1..=3, |n| n.to_string(), |n| Ok(n * 2));
            Ok(results
                .into_iter()
                .filter_map(|r| r.result.ok())
                .sum::<i32>())
        });
        while !job.is_finished() {
            std::thread::yield_now();
        }
        assert_eq!(job.progress().done, 3);
        assert_eq!(job.progress().fraction(), 1.0);
        assert_eq!(job.join().unwrap(), 12);
    }

    #[test]
    fn cancellation_stops_between_items_and_keeps_errors_per_item() {
        let (started, wait) = mpsc::channel();
        let (resume, gate) = mpsc::channel::<()>();
        let job = spawn("cancel", move |context| {
            Ok(for_each(
                context,
                0..100,
                |n| n.to_string(),
                |n| {
                    if *n == 0 {
                        started.send(()).ok();
                        gate.recv().ok();
                        return Err(Error::Other("first fails".into()));
                    }
                    Ok(*n)
                },
            )
            .len())
        });
        wait.recv().unwrap();
        job.cancel();
        resume.send(()).unwrap();
        assert_eq!(job.join().unwrap(), 1);
    }
}
