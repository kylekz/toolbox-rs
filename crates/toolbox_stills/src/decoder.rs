//! The decode pool (docs/stills.md "The decode pool"): a few threads of the crate's own that
//! run every still and animation decode the app asks for, behind bounded queues. Its own
//! threads, not the caller's executor: on Windows each joins COM's multithreaded apartment once
//! for WIC and stays there, which a borrowed thread pool's threads must not be made to do.
//!
//! One pool serves the whole app, in two lanes: the urgent decodes ([`Decoder::decode`], say a
//! slideshow's next pictures) jump ahead of the thumbnails ([`Decoder::thumbnail`]) still
//! queued, so a scroll through thousands of files never delays what is on screen next. A decode
//! already running finishes first. The urgent lane runs by need, the lowest [`Priority`] first;
//! the thumbnails' in the order asked. A request whose caller stopped waiting before its turn is
//! skipped, not decoded for nobody.
//!
//! [`Decoder::decode`] and [`Decoder::thumbnail`] return futures that need no particular
//! executor (gpui-free).

use std::collections::BinaryHeap;
use std::io;
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::sync::{Arc, Mutex, PoisonError};
use std::thread;

use crate::decode::{DecodeError, Decoded, SourceBuf, Thumbnail, decode, thumbnail};
use crate::sniff::Picture;

/// Thumbnail requests waiting per worker before a request's future waits to queue another.
const QUEUE_PER_WORKER: usize = 2;

/**
 * A request: decodes and sends its reply when run, unless its caller has stopped waiting.
 * Dropped unrun, it drops its reply sender, which resolves the caller's future to
 * [`DecodeError::Closed`].
 */
type Job = Box<dyn FnOnce() + Send>;

/**
 * When an urgent decode is needed, lower first. Requests queued at different times compare, so
 * it is best counted so that it doesn't change as the app moves on: a slideshow's place in play
 * order (its cuts so far plus the distance ahead), not the distance alone.
 */
pub type Priority = u64;

/// An urgent request waiting its turn: by priority, then in the order asked.
struct Queued {
    priority: Priority,
    seq: u64,
    job: Job,
}

impl PartialEq for Queued {
    fn eq(&self, other: &Self) -> bool {
        (self.priority, self.seq) == (other.priority, other.seq)
    }
}

impl Eq for Queued {}

impl PartialOrd for Queued {
    fn partial_cmp(&self, other: &Self) -> Option<std::cmp::Ordering> {
        Some(self.cmp(other))
    }
}

impl Ord for Queued {
    /// Reversed: `BinaryHeap` pops its greatest, which is the most needed.
    fn cmp(&self, other: &Self) -> std::cmp::Ordering {
        (other.priority, other.seq).cmp(&(self.priority, self.seq))
    }
}

/// The urgent lane: requests by need, and how many have been asked.
#[derive(Default)]
struct UrgentLane {
    queued: BinaryHeap<Queued>,
    asked: u64,
}

/// The pool's queues: one per lane, and a token per queued job that wakes one worker.
#[derive(Clone)]
struct Queues {
    urgent: Arc<Mutex<UrgentLane>>,
    background: async_channel::Sender<Job>,
    wake: async_channel::Sender<()>,
}

impl Queues {
    fn close(&self) {
        self.background.close();
        self.wake.close();
    }

    fn urgent(&self) -> std::sync::MutexGuard<'_, UrgentLane> {
        /* A job runs outside the lock, so nothing can poison it mid-change. */
        self.urgent.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// The most needed urgent request, else the oldest thumbnail one.
    fn next(&self, background: &async_channel::Receiver<Job>) -> Option<Job> {
        self.urgent()
            .queued
            .pop()
            .map(|queued| queued.job)
            .or_else(|| background.try_recv().ok())
    }
}

/// Which queue a request waits in.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Lane {
    Urgent(Priority),
    Background,
}

/**
 * A pool of decode threads. Dropping it stops them without waiting: requests still queued
 * resolve to [`DecodeError::Closed`], and the decodes already running finish on their own
 * threads, so a drop never blocks its caller (the UI thread, often).
 */
pub struct Decoder {
    queues: Queues,
}

impl Decoder {
    /**
     * A pool size that suits an app: four decodes in flight decode a screenful of stills well
     * within a tenth of a second (slide's measurement, docs/stills.md "The decode pool").
     */
    pub const WORKERS: usize = 4;

    /// Starts `workers` threads (at least one).
    pub fn new(workers: usize) -> io::Result<Self> {
        let workers = workers.max(1);
        let (background, background_queue) = async_channel::bounded(workers * QUEUE_PER_WORKER);
        let (wake, woken) = async_channel::unbounded();
        let queues = Queues {
            urgent: Arc::default(),
            background,
            wake,
        };
        for index in 0..workers {
            let (queues_here, background, woken) =
                (queues.clone(), background_queue.clone(), woken.clone());
            /* A pool that can't start all its threads closes the ones it started. */
            if let Err(error) = thread::Builder::new()
                .name(format!("stills-decode-{index}"))
                .spawn(move || work(&queues_here, &background, &woken))
            {
                queues.close();
                return Err(error);
            }
        }
        Ok(Self { queues })
    }

    /**
     * [`crate::decode`] on one of the pool's threads, ahead of every thumbnail still queued and
     * of every urgent request with a later `priority`: the urgent lane. `source` is a file's
     * `PathBuf` or the bytes (`Vec<u8>`, `Arc<[u8]>`). Dropping the future before its turn
     * skips the decode; once it runs, the result is given up, not the work.
     */
    pub fn decode(
        &self,
        source: impl Into<SourceBuf>,
        picture: Picture,
        bounds: (u32, u32),
        animation_budget: usize,
        priority: Priority,
    ) -> impl Future<Output = Result<Decoded, DecodeError>> + Send + 'static {
        let source = source.into();
        self.run(Lane::Urgent(priority), move || {
            decode(source.as_source(), picture, bounds, animation_budget)
        })
    }

    /// [`crate::thumbnail`] on one of the pool's threads, behind the urgent decodes.
    pub fn thumbnail(
        &self,
        source: impl Into<SourceBuf>,
        picture: Picture,
        max_side: u32,
    ) -> impl Future<Output = Result<Thumbnail, DecodeError>> + Send + 'static {
        let source = source.into();
        self.run(Lane::Background, move || {
            thumbnail(source.as_source(), picture, max_side)
        })
    }

    /**
     * Queues `work` in `lane` and resolves with its result. Decoders of untrusted files can
     * panic: the pool keeps its thread and the caller gets an error.
     */
    fn run<T: Send + 'static>(
        &self,
        lane: Lane,
        work: impl FnOnce() -> Result<T, DecodeError> + Send + 'static,
    ) -> impl Future<Output = Result<T, DecodeError>> + Send + 'static {
        let queues = self.queues.clone();
        async move {
            let (reply, result) = async_channel::bounded(1);
            let job: Job = Box::new(move || {
                /* Its caller stopped waiting before its turn: nobody wants it. */
                if reply.is_closed() {
                    return;
                }
                let result = catch_unwind(AssertUnwindSafe(work)).unwrap_or_else(|_| {
                    Err(DecodeError::Decode("the decoder panicked".to_owned()))
                });
                /* The caller may have stopped waiting meanwhile. */
                let _ = reply.try_send(result);
            });
            match lane {
                Lane::Urgent(priority) => {
                    if queues.wake.is_closed() {
                        return Err(DecodeError::Closed);
                    }
                    let mut urgent = queues.urgent();
                    let seq = urgent.asked;
                    urgent.asked += 1;
                    urgent.queued.push(Queued { priority, seq, job });
                }
                Lane::Background => queues
                    .background
                    .send(job)
                    .await
                    .map_err(|_| DecodeError::Closed)?,
            }
            /* Closed meanwhile: a worker drains the queue, dropping the job unrun. */
            queues.wake.try_send(()).map_err(|_| DecodeError::Closed)?;
            result.recv().await.map_err(|_| DecodeError::Closed)?
        }
    }
}

impl Drop for Decoder {
    fn drop(&mut self) {
        self.queues.close();
    }
}

/**
 * A worker: each token is one queued job, taken from the urgent lane first. Once the pool is
 * closed, the jobs left are dropped unrun, which resolves their futures to `Closed`.
 */
fn work(
    queues: &Queues,
    background: &async_channel::Receiver<Job>,
    woken: &async_channel::Receiver<()>,
) {
    #[cfg(windows)]
    crate::decode::os::join_apartment();
    while woken.recv_blocking().is_ok() {
        let Some(job) = queues.next(background) else {
            continue;
        };
        if !woken.is_closed() {
            job();
        }
    }
    while queues.next(background).is_some() {}
}

#[cfg(test)]
mod tests {
    use std::path::{Path, PathBuf};

    use super::*;
    use crate::sniff::{AnimationFormat, ImageFormat};

    fn fixture(name: &str) -> PathBuf {
        Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("tests/fixtures")
            .join(name)
    }

    #[test]
    fn decodes_more_requests_than_threads_at_once() {
        let decoder = Decoder::new(Decoder::WORKERS).unwrap();
        let requests: Vec<_> = (0..12)
            .map(|index| {
                let (name, picture) = if index % 3 == 2 {
                    (
                        "anim-160x90.webp",
                        Picture::Animation(AnimationFormat::Webp),
                    )
                } else {
                    ("still-1920x1080.jpg", Picture::Image(ImageFormat::Jpeg))
                };
                decoder.decode(fixture(name), picture, (160, 160), usize::MAX, 0)
            })
            .collect();
        /* Each waited on by a thread of its own, so all twelve are queued at once. */
        let waiting: Vec<_> = requests
            .into_iter()
            .map(|request| thread::spawn(move || pollster::block_on(request)))
            .collect();
        let results = waiting.into_iter().map(|thread| thread.join().unwrap());
        for (index, result) in results.enumerate() {
            match result.unwrap() {
                Decoded::Still(image) => {
                    assert_ne!(index % 3, 2);
                    assert_eq!((image.width(), image.height()), (160, 90));
                }
                Decoded::Animation(frames) => {
                    assert_eq!(index % 3, 2);
                    assert_eq!(frames.len(), 5);
                }
            }
        }
    }

    #[test]
    fn urgent_decodes_jump_ahead_of_queued_thumbnails() {
        use std::sync::{Arc, Mutex};
        use std::time::Duration;

        let decoder = Decoder::new(1).unwrap();
        let order = Arc::new(Mutex::new(Vec::new()));
        let (release, released) = async_channel::bounded::<()>(1);
        let (started, running) = async_channel::bounded::<()>(1);
        /* The one worker is busy until released. */
        let busy = decoder.run(Lane::Background, move || {
            started.send_blocking(()).unwrap();
            released.recv_blocking().unwrap();
            Ok(())
        });
        let mut waiting = vec![thread::spawn(move || pollster::block_on(busy))];
        running.recv_blocking().unwrap();
        let queued = |lane: Lane, name: &'static str, decoder: &Decoder| {
            let order = order.clone();
            let request = decoder.run(lane, move || {
                order.lock().unwrap().push(name);
                Ok(())
            });
            thread::spawn(move || pollster::block_on(request))
        };
        let wait_for = |len: fn(&Queues) -> usize, n: usize, decoder: &Decoder| {
            while len(&decoder.queues) < n {
                thread::sleep(Duration::from_millis(1));
            }
        };
        waiting.push(queued(Lane::Background, "thumbnail 1", &decoder));
        wait_for(|queues| queues.background.len(), 1, &decoder);
        waiting.push(queued(Lane::Background, "thumbnail 2", &decoder));
        wait_for(|queues| queues.background.len(), 2, &decoder);
        waiting.push(queued(Lane::Urgent(0), "urgent", &decoder));
        wait_for(|queues| queues.urgent().queued.len(), 1, &decoder);
        release.send_blocking(()).unwrap();
        for request in waiting {
            request.join().unwrap().unwrap();
        }
        assert_eq!(
            *order.lock().unwrap(),
            ["urgent", "thumbnail 1", "thumbnail 2"]
        );
    }

    #[test]
    fn urgent_decodes_run_nearest_first_and_skip_those_given_up() {
        use std::sync::{Arc, Mutex};
        use std::task::{Context, Poll, Waker};

        let decoder = Decoder::new(1).unwrap();
        let order = Arc::new(Mutex::new(Vec::new()));
        let (release, released) = async_channel::bounded::<()>(1);
        let (started, running) = async_channel::bounded::<()>(1);
        let busy = decoder.run(Lane::Background, move || {
            started.send_blocking(()).unwrap();
            released.recv_blocking().unwrap();
            Ok(())
        });
        let busy = thread::spawn(move || pollster::block_on(busy));
        running.recv_blocking().unwrap();
        let request = |priority: Priority, name: &'static str| {
            let order = order.clone();
            decoder.run(Lane::Urgent(priority), move || {
                order.lock().unwrap().push(name);
                Ok(())
            })
        };
        /* Polled once, each is queued; the one dropped then is given up before its turn. */
        let queue = |priority, name| {
            let mut request = Box::pin(request(priority, name));
            let polled = request
                .as_mut()
                .poll(&mut Context::from_waker(Waker::noop()));
            assert!(matches!(polled, Poll::Pending));
            request
        };
        let far = queue(7, "far");
        let near = queue(2, "near");
        drop(queue(1, "given up"));
        let next = queue(3, "next");
        release.send_blocking(()).unwrap();
        busy.join().unwrap().unwrap();
        for request in [near, next, far] {
            pollster::block_on(request).unwrap();
        }
        assert_eq!(*order.lock().unwrap(), ["near", "next", "far"]);
    }

    #[test]
    fn decodes_bytes_as_it_decodes_their_file() {
        let decoder = Decoder::new(1).unwrap();
        let path = fixture("still-1920x1080.jpg");
        let picture = Picture::Image(ImageFormat::Jpeg);
        let bytes = std::fs::read(&path).unwrap();
        let from_file = pollster::block_on(decoder.thumbnail(path, picture, 160)).unwrap();
        let from_bytes = pollster::block_on(decoder.thumbnail(bytes, picture, 160)).unwrap();
        assert_eq!(from_bytes, from_file);
        assert_eq!(from_bytes.source_size, (1920, 1080));
    }

    #[test]
    fn errors_come_back_and_a_dropped_pool_closes() {
        let decoder = Decoder::new(1).unwrap();
        let missing = decoder.decode(
            fixture("missing.jpg"),
            Picture::Image(ImageFormat::Jpeg),
            (10, 10),
            usize::MAX,
            0,
        );
        assert!(matches!(
            pollster::block_on(missing),
            Err(DecodeError::Io { .. })
        ));
        let late = decoder.decode(
            fixture("still-1920x1080.jpg"),
            Picture::Image(ImageFormat::Jpeg),
            (10, 10),
            usize::MAX,
            0,
        );
        drop(decoder);
        assert!(matches!(pollster::block_on(late), Err(DecodeError::Closed)));
    }
}
