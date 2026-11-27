//! Threads that take part of a frame's work off the receiver thread.
//!
//! The sub-channels of a multiplex are decoded each on its own (time
//! deinterleaving, Viterbi, Reed-Solomon, the audio, the texts and slides),
//! so they can go to several cores at once. One core of a Raspberry Pi 3B+
//! cannot decode every station of a multiplex (the texts and slides of all of
//! them in the background) on its own; four can.
//!
//! Each thread gets its share of the jobs, the calling thread too, and the
//! caller hands a job to the same thread from frame to frame, so a decoder's
//! buffers stay in the cache of one core.

use crate::receiver::thread_cpu_time;
use crossbeam_channel::{Receiver, Sender};
use std::panic::{catch_unwind, resume_unwind, AssertUnwindSafe};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::Duration;

/// Threads that do jobs of type `J` with `work`, giving `D` back.
pub(crate) struct Pool<J, D> {
    /// The shares of each thread besides the calling one.
    threads: Vec<Sender<Vec<J>>>,
    done: Receiver<std::thread::Result<Vec<D>>>,
    work: fn(J) -> D,
    /// CPU time of each of the pool's threads (ns), for the statistics.
    cpu: Arc<Vec<AtomicU64>>,
}

impl<J: Send + 'static, D: Send + 'static> Pool<J, D> {
    /// `threads` besides the calling one, named `name`. They end with the
    /// pool (no more jobs can come).
    pub(crate) fn new(threads: usize, name: &str, work: fn(J) -> D) -> Self {
        let (finished, done) = crossbeam_channel::unbounded();
        let cpu: Arc<Vec<AtomicU64>> = Arc::new((0..threads).map(|_| AtomicU64::new(0)).collect());
        let mut senders = Vec::with_capacity(threads);
        for i in 0..threads {
            let (tx, rx) = crossbeam_channel::unbounded::<Vec<J>>();
            let (finished, cpu) = (finished.clone(), cpu.clone());
            // (a thread that cannot start leaves its share to the calling one:
            // the sender finds no receiver)
            let _ = std::thread::Builder::new().name(name.into()).spawn(move || {
                while let Ok(share) = rx.recv() {
                    // (a panic goes to the thread that waits for the share)
                    let out = catch_unwind(AssertUnwindSafe(|| share.into_iter().map(work).collect::<Vec<D>>()));
                    if let Some(t) = thread_cpu_time() {
                        cpu[i].store(t.as_nanos() as u64, Ordering::Relaxed);
                    }
                    if finished.send(out).is_err() {
                        break;
                    }
                }
            });
            senders.push(tx);
        }
        Pool { threads: senders, done, work, cpu }
    }

    /// Threads that share the jobs, the calling one among them.
    pub(crate) fn threads(&self) -> usize {
        self.threads.len() + 1
    }

    /// Does the jobs of `shares`: the first ones each on a thread of the
    /// pool (the same jobs to the same thread from call to call keep their
    /// data in that core's cache), the last one on the calling thread. What
    /// they give comes to `out` in no particular order; a job that panicked
    /// on another thread panics here.
    pub(crate) fn run(&self, mut shares: Vec<Vec<J>>, out: &mut Vec<D>) {
        let mut own = shares.pop().unwrap_or_default();
        let mut waiting = 0;
        for (k, share) in shares.into_iter().enumerate() {
            if share.is_empty() {
                continue;
            }
            // (more shares than threads: the rest on this one)
            let Some(thread) = self.threads.get(k) else {
                own.extend(share);
                continue;
            };
            match thread.send(share) {
                Ok(()) => waiting += 1,
                Err(e) => own.extend(e.into_inner()),
            }
        }
        out.extend(own.into_iter().map(self.work));
        for _ in 0..waiting {
            match self.done.recv() {
                Ok(Ok(d)) => out.extend(d),
                Ok(Err(panic)) => resume_unwind(panic),
                // (no thread left)
                Err(_) => break,
            }
        }
    }

    /// CPU time of the pool's threads so far.
    pub(crate) fn cpu(&self) -> Duration {
        Duration::from_nanos(self.cpu.iter().map(|c| c.load(Ordering::Relaxed)).sum())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn square(x: u64) -> (u64, String) {
        (x * x, std::thread::current().name().unwrap_or_default().to_string())
    }

    #[test]
    fn every_job_is_done_once_its_share_on_its_thread() {
        let pool = Pool::new(3, "pool-test", square);
        assert_eq!(pool.threads(), 4);
        let caller = std::thread::current().name().unwrap_or_default().to_string();
        for _ in 0..20 {
            let shares: Vec<Vec<u64>> = (0..4u64).map(|k| (k * 10..k * 10 + 10).collect()).collect();
            let mut out = Vec::new();
            pool.run(shares, &mut out);
            let mut got: Vec<u64> = out.iter().map(|o| o.0).collect();
            got.sort_unstable();
            assert_eq!(got, (0..40u64).map(|x| x * x).collect::<Vec<_>>());
            for (sq, thread) in &out {
                let x = (*sq as f64).sqrt() as u64;
                // the last share on the calling thread, the others on the pool's
                assert_eq!(thread == &caller, x >= 30, "{x} on {thread}");
            }
        }
    }

    #[test]
    fn without_threads_the_caller_does_everything() {
        let pool = Pool::new(0, "pool-none", square);
        let mut out = Vec::new();
        // (a share for a thread that is not there)
        pool.run(vec![vec![2], vec![3]], &mut out);
        out.sort_unstable();
        assert_eq!(out.iter().map(|o| o.0).collect::<Vec<_>>(), [4, 9]);
        assert_eq!(pool.cpu(), Duration::ZERO);
    }

    fn fails(x: u64) -> u64 {
        if x == 7 {
            panic!("job 7");
        }
        x
    }

    #[test]
    fn a_panic_on_another_thread_comes_to_the_caller() {
        let pool = Pool::new(2, "pool-panic", fails);
        let r = catch_unwind(AssertUnwindSafe(|| {
            let mut out = Vec::new();
            pool.run(vec![vec![5, 6, 7], vec![8], vec![9]], &mut out);
        }));
        assert!(r.is_err());
    }
}
