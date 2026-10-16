//! Stdin streaming: read lines on a background thread, hand them over in batches.

use crate::items;
use std::io::{self, BufRead};
use std::sync::{
    Arc, Mutex,
    atomic::{AtomicBool, Ordering},
};

/// Stdin lines arriving from a reader thread. The event loop already polls
/// every 16ms, so the queue only needs a mutex — no extra wakeup source.
pub(crate) struct ItemFeed {
    queue: Mutex<Vec<items::Item>>,
    done: AtomicBool,
}

impl ItemFeed {
    fn new() -> Arc<Self> {
        Arc::new(Self {
            queue: Mutex::new(Vec::new()),
            done: AtomicBool::new(false),
        })
    }

    fn push(&self, batch: Vec<items::Item>) {
        self.queue.lock().unwrap().extend(batch);
    }

    pub(crate) fn drain(&self) -> Vec<items::Item> {
        std::mem::take(&mut *self.queue.lock().unwrap())
    }

    fn mark_done(&self) {
        self.done.store(true, Ordering::Release);
    }

    pub(crate) fn done(&self) -> bool {
        self.done.load(Ordering::Acquire)
    }

    /// Read stdin to EOF in a background thread, handing lines over in batches
    /// so a slow producer never delays the menu appearing.
    pub(crate) fn spawn() -> Arc<Self> {
        Self::spawn_worker(|feed| feed.read_into(io::stdin().lock()))
    }

    /// Collect a one-shot list off the main thread. `--run` scans `.desktop`
    /// files and `$PATH`; that scan must not sit between process start and the
    /// first frame, so it runs here and lands in the same queue stdin uses.
    pub(crate) fn spawn_with(
        producer: impl FnOnce() -> Vec<items::Item> + Send + 'static,
    ) -> Arc<Self> {
        Self::spawn_worker(move |feed| feed.push(producer()))
    }

    /// Run `worker` on a thread and signal EOF when it returns. The feed comes
    /// back immediately, so callers never wait for the worker.
    fn spawn_worker(worker: impl FnOnce(&Self) + Send + 'static) -> Arc<Self> {
        let feed = Self::new();
        let writer = Arc::clone(&feed);
        std::thread::spawn(move || {
            worker(&writer);
            writer.mark_done();
        });
        feed
    }

    /// Drain a reader into batches. A line that is not valid UTF-8 is skipped,
    /// never a reason to stop: truncating the menu at the first bad filename
    /// would silently hide everything after it.
    fn read_into(&self, reader: impl BufRead) {
        const BATCH: usize = 256;
        let mut batch = Vec::with_capacity(BATCH);
        for line in reader.lines() {
            let Ok(line) = line else { continue };
            batch.push(items::parse(&line));
            if batch.len() >= BATCH {
                self.push(std::mem::take(&mut batch));
            }
        }
        if !batch.is_empty() {
            self.push(batch);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::{Duration, Instant};

    /// Spin until `done()`, so the assertion cannot race the worker thread.
    fn wait_until_done(feed: &ItemFeed) {
        let deadline = Instant::now() + Duration::from_secs(5);
        while !feed.done() {
            assert!(Instant::now() < deadline, "producer never finished");
            std::thread::sleep(Duration::from_millis(5));
        }
    }

    #[test]
    fn spawn_with_does_not_block_on_the_producer() {
        // The whole point of the change: a slow scan must not delay the frame.
        let started = Instant::now();
        let feed = ItemFeed::spawn_with(|| {
            std::thread::sleep(Duration::from_millis(150));
            vec![items::parse("slow")]
        });
        assert!(
            started.elapsed() < Duration::from_millis(50),
            "spawn_with returned only after the producer: {:?}",
            started.elapsed()
        );
        wait_until_done(&feed);
        let got: Vec<String> = feed.drain().into_iter().map(|i| i.text).collect();
        assert_eq!(got, vec!["slow"], "the producer's items still arrive");
    }

    #[test]
    fn spawn_with_delivers_items_then_marks_done() {
        let feed = ItemFeed::spawn_with(|| vec![items::parse("a"), items::parse("b")]);
        wait_until_done(&feed);
        assert!(feed.done());
        let got: Vec<String> = feed.drain().into_iter().map(|i| i.text).collect();
        assert_eq!(got, vec!["a", "b"], "order is preserved");
    }

    #[test]
    fn item_feed_drains_batches_in_order_and_signals_eof() {
        let feed = ItemFeed::new();
        feed.push(vec![items::parse("a")]);
        feed.push(vec![items::parse("b"), items::parse("c")]);
        let got: Vec<String> = feed.drain().into_iter().map(|i| i.text).collect();
        assert_eq!(got, vec!["a", "b", "c"]);
        assert!(feed.drain().is_empty(), "drain empties the queue");
        assert!(!feed.done());
        feed.mark_done();
        assert!(feed.done());
    }

    #[test]
    fn item_feed_skips_invalid_utf8_without_truncating() {
        let feed = ItemFeed::new();
        feed.read_into(&b"good\n\xff not utf8\nlast\n"[..]);
        let got: Vec<String> = feed.drain().into_iter().map(|i| i.text).collect();
        assert_eq!(
            got,
            vec!["good", "last"],
            "a bad line must not end the stream"
        );
    }
}
