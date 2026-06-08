//! In-process background jobs + scheduler — ferro's replacement for the bench's `worker`
//! (RQ) and `schedule` processes **and** the `redis_queue` that connected them.
//!
//! In a stock bench, `frappe.enqueue(method, **kwargs)` pickles a call into redis, a separate
//! `rq` worker process pops and runs it, and `bench schedule` periodically enqueues cron jobs.
//! ferro folds all three into the serving process: [`JobQueue::enqueue`] pushes a job onto an
//! in-memory queue, a pool of worker threads runs registered native handlers, and [`Scheduler`]
//! enqueues periodic jobs on a tick. Because a running job holds `Arc`s to the cache and the
//! realtime hub, it can publish progress straight to the browser — no redis pub/sub hop.
//!
//! Job handlers are native Rust closures keyed by Frappe-style dotted method names. The
//! `python` build (ferrod) can register a fallthrough handler that runs the real Python callable
//! in the embedded interpreter; the pure-Rust build runs the native registry and logs anything
//! it doesn't recognize.

use crate::cache::Cache;
use crate::realtime::Realtime;
use serde_json::{json, Value};
use std::collections::HashMap;
use std::collections::VecDeque;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Condvar, Mutex, RwLock};
use std::thread;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

fn now_secs() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0)
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum JobStatus {
    Queued,
    Started,
    Finished,
    Failed,
}

#[derive(Clone)]
pub struct Job {
    pub id: String,
    pub method: String,
    pub kwargs: Value,
    pub queue: String,
    pub site: String,
    pub enqueued_at: u64,
}

/// What a handler receives: the job plus in-process access to cache + realtime so it can read
/// state and push events/progress to connected browsers.
pub struct JobContext {
    pub cache: Arc<Cache>,
    pub realtime: Arc<Realtime>,
    pub queue: Arc<JobQueue>,
}

impl JobContext {
    /// Publish a progress update to the task room, matching Frappe's `publish_progress`.
    pub fn publish_progress(&self, site: &str, task_id: &str, percent: f64, description: &str) {
        self.realtime.emit(
            site,
            &format!("task_progress:{}", task_id),
            "progress",
            &json!({"percent": percent, "description": description, "task_id": task_id}),
        );
    }
}

pub type Handler = Arc<dyn Fn(&JobContext, &Job) -> Result<Value, String> + Send + Sync>;

struct Status {
    status: JobStatus,
    result: Option<Value>,
    error: Option<String>,
}

/// Priority order ferro drains queues in (mirrors RQ's short/default/long intent).
const QUEUE_ORDER: [&str; 3] = ["short", "default", "long"];

pub struct JobQueue {
    queues: Mutex<HashMap<String, VecDeque<Job>>>,
    signal: Condvar,
    handlers: RwLock<HashMap<String, Handler>>,
    statuses: Mutex<HashMap<String, Status>>,
    next_id: AtomicU64,
    running: AtomicBool,
    /// Optional catch-all handler (ferrod registers one that runs the real Python callable).
    fallthrough: RwLock<Option<Handler>>,
}

impl JobQueue {
    pub fn new() -> Arc<JobQueue> {
        Arc::new(JobQueue {
            queues: Mutex::new(HashMap::new()),
            signal: Condvar::new(),
            handlers: RwLock::new(HashMap::new()),
            statuses: Mutex::new(HashMap::new()),
            next_id: AtomicU64::new(1),
            running: AtomicBool::new(true),
            fallthrough: RwLock::new(None),
        })
    }

    /// Register a native handler for a dotted method name (e.g. "frappe.email.queue.flush").
    pub fn register(&self, method: &str, handler: Handler) {
        self.handlers.write().unwrap().insert(method.to_string(), handler);
    }

    /// Register a catch-all handler used when no native handler matches the method.
    pub fn set_fallthrough(&self, handler: Handler) {
        *self.fallthrough.write().unwrap() = Some(handler);
    }

    pub fn registered_methods(&self) -> Vec<String> {
        self.handlers.read().unwrap().keys().cloned().collect()
    }

    /// Enqueue a job. Returns its id. `queue` is one of short/default/long (others allowed).
    pub fn enqueue(self: &Arc<Self>, method: &str, kwargs: Value, queue: &str, site: &str) -> String {
        let id = format!("ferrojob-{}", self.next_id.fetch_add(1, Ordering::SeqCst));
        let job = Job {
            id: id.clone(),
            method: method.to_string(),
            kwargs,
            queue: queue.to_string(),
            site: site.to_string(),
            enqueued_at: now_secs(),
        };
        self.statuses
            .lock()
            .unwrap()
            .insert(id.clone(), Status { status: JobStatus::Queued, result: None, error: None });
        self.queues.lock().unwrap().entry(queue.to_string()).or_default().push_back(job);
        self.signal.notify_one();
        id
    }

    pub fn status(&self, id: &str) -> Option<JobStatus> {
        self.statuses.lock().unwrap().get(id).map(|s| s.status)
    }
    pub fn result(&self, id: &str) -> Option<Value> {
        self.statuses.lock().unwrap().get(id).and_then(|s| s.result.clone())
    }
    pub fn error(&self, id: &str) -> Option<String> {
        self.statuses.lock().unwrap().get(id).and_then(|s| s.error.clone())
    }
    pub fn pending(&self) -> usize {
        self.queues.lock().unwrap().values().map(|q| q.len()).sum()
    }

    fn pop(&self) -> Option<Job> {
        let mut q = self.queues.lock().unwrap();
        loop {
            // priority order first, then any other named queues
            for name in QUEUE_ORDER.iter() {
                if let Some(dq) = q.get_mut(*name) {
                    if let Some(job) = dq.pop_front() {
                        return Some(job);
                    }
                }
            }
            for dq in q.values_mut() {
                if let Some(job) = dq.pop_front() {
                    return Some(job);
                }
            }
            if !self.running.load(Ordering::SeqCst) {
                return None;
            }
            let (nq, _) = self.signal.wait_timeout(q, Duration::from_millis(500)).unwrap();
            q = nq;
        }
    }

    fn set_status(&self, id: &str, status: JobStatus, result: Option<Value>, error: Option<String>) {
        if let Some(s) = self.statuses.lock().unwrap().get_mut(id) {
            s.status = status;
            if result.is_some() {
                s.result = result;
            }
            if error.is_some() {
                s.error = error;
            }
        }
    }

    pub fn shutdown(&self) {
        self.running.store(false, Ordering::SeqCst);
        self.signal.notify_all();
    }
}

/// Spawn `n` worker threads draining the queue. Returns their join handles.
pub fn start_workers(
    queue: Arc<JobQueue>,
    ctx: Arc<JobContext>,
    n: usize,
) -> Vec<thread::JoinHandle<()>> {
    let mut handles = Vec::new();
    for i in 0..n.max(1) {
        let queue = queue.clone();
        let ctx = ctx.clone();
        let h = thread::Builder::new()
            .name(format!("ferro-worker-{}", i))
            .stack_size(512 * 1024)
            .spawn(move || worker_loop(queue, ctx))
            .expect("spawn worker");
        handles.push(h);
    }
    handles
}

fn worker_loop(queue: Arc<JobQueue>, ctx: Arc<JobContext>) {
    while let Some(job) = queue.pop() {
        queue.set_status(&job.id, JobStatus::Started, None, None);
        let handler = queue.handlers.read().unwrap().get(&job.method).cloned();
        let handler = handler.or_else(|| queue.fallthrough.read().unwrap().clone());
        let outcome = match handler {
            Some(h) => h(&ctx, &job),
            None => {
                eprintln!(
                    "ferro worker: no handler for '{}' (job {}); skipping",
                    job.method, job.id
                );
                Ok(Value::Null)
            }
        };
        match outcome {
            Ok(v) => queue.set_status(&job.id, JobStatus::Finished, Some(v), None),
            Err(e) => {
                eprintln!("ferro worker: job {} ({}) failed: {}", job.id, job.method, e);
                queue.set_status(&job.id, JobStatus::Failed, None, Some(e));
            }
        }
    }
}

// ----------------------------------------------------------------------------------------------
// Scheduler
// ----------------------------------------------------------------------------------------------

/// A named scheduling interval, mirroring Frappe's `scheduler_events` keys.
#[derive(Clone, Copy)]
pub enum Every {
    Seconds(u64),
}

impl Every {
    pub fn all() -> Every {
        Every::Seconds(60)
    } // Frappe runs "all" jobs every scheduler tick (~60s)
    pub fn hourly() -> Every {
        Every::Seconds(3600)
    }
    pub fn daily() -> Every {
        Every::Seconds(86400)
    }
    pub fn weekly() -> Every {
        Every::Seconds(7 * 86400)
    }
    pub fn secs(&self) -> u64 {
        match self {
            Every::Seconds(s) => *s,
        }
    }
}

struct ScheduledTask {
    method: String,
    every: Every,
    queue: String,
    last_run: Instant,
}

pub struct Scheduler {
    queue: Arc<JobQueue>,
    site: String,
    tasks: Mutex<Vec<ScheduledTask>>,
    running: AtomicBool,
}

impl Scheduler {
    pub fn new(queue: Arc<JobQueue>, site: &str) -> Arc<Scheduler> {
        Arc::new(Scheduler {
            queue,
            site: site.to_string(),
            tasks: Mutex::new(Vec::new()),
            running: AtomicBool::new(true),
        })
    }

    /// Register a periodic job. `method` must have a handler registered on the queue.
    pub fn every(&self, every: Every, method: &str, queue: &str) {
        // Stagger first run by setting last_run to now minus a bit so short intervals fire soon
        // but not all at once.
        self.tasks.lock().unwrap().push(ScheduledTask {
            method: method.to_string(),
            every,
            queue: queue.to_string(),
            last_run: Instant::now(),
        });
    }

    pub fn task_count(&self) -> usize {
        self.tasks.lock().unwrap().len()
    }

    pub fn stop(&self) {
        self.running.store(false, Ordering::SeqCst);
    }
}

/// Spawn the scheduler tick thread. Every second it checks each task and enqueues any that are due.
pub fn start_scheduler(sched: Arc<Scheduler>) -> thread::JoinHandle<()> {
    thread::Builder::new()
        .name("ferro-scheduler".into())
        .stack_size(256 * 1024)
        .spawn(move || {
            eprintln!(
                "ferro scheduler: {} periodic task(s) for site {}",
                sched.task_count(),
                sched.site
            );
            while sched.running.load(Ordering::SeqCst) {
                thread::sleep(Duration::from_secs(1));
                let now = Instant::now();
                let mut due: Vec<(String, String)> = Vec::new();
                {
                    let mut tasks = sched.tasks.lock().unwrap();
                    for t in tasks.iter_mut() {
                        if now.duration_since(t.last_run).as_secs() >= t.every.secs() {
                            t.last_run = now;
                            due.push((t.method.clone(), t.queue.clone()));
                        }
                    }
                }
                for (method, queue) in due {
                    sched.queue.enqueue(&method, json!({"scheduled": true}), &queue, &sched.site);
                }
            }
        })
        .expect("spawn scheduler")
}

// ----------------------------------------------------------------------------------------------
// Built-in native handlers
// ----------------------------------------------------------------------------------------------

/// Register the handful of native handlers ferro ships with. Apps/ferrod can register more.
/// These cover the maintenance jobs Frappe's scheduler enqueues most often; each is a no-op or
/// cache-cleanup in the pure-Rust runtime, but the lifecycle (queued→started→finished) is real.
pub fn register_builtins(queue: &Arc<JobQueue>) {
    // A trivially-verifiable job used by tests/health checks.
    queue.register(
        "ferro.ping",
        Arc::new(|_ctx, job| {
            Ok(json!({"pong": true, "echo": job.kwargs.clone()}))
        }),
    );

    // Clear transient cache entries (Frappe: frappe.sessions.clear_expired_sessions etc).
    queue.register(
        "frappe.sessions.clear_expired_sessions",
        Arc::new(|ctx, _job| {
            let removed = ctx.cache.delete_keys("session:*");
            Ok(json!({"cleared_sessions": removed}))
        }),
    );

    // The scheduler heartbeat — Frappe pings this to know the scheduler is alive.
    queue.register(
        "frappe.utils.scheduler.enqueue_events",
        Arc::new(|ctx, job| {
            ctx.cache.set_str("scheduler:last_heartbeat", &now_secs().to_string());
            // Emit a lightweight site event so connected clients see liveness if subscribed.
            ctx.realtime.emit_all(&job.site, "scheduler_heartbeat", &json!({"ts": now_secs()}));
            Ok(json!({"heartbeat": now_secs()}))
        }),
    );
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::realtime::Realtime;

    fn test_ctx() -> (Arc<JobQueue>, Arc<JobContext>) {
        let cache = Arc::new(Cache::new());
        let resolver: crate::realtime::AuthResolver =
            Arc::new(|_s, _c, _a| ("Administrator".to_string(), "System User".to_string()));
        let realtime = Realtime::new(resolver);
        let queue = JobQueue::new();
        let ctx = Arc::new(JobContext { cache, realtime, queue: queue.clone() });
        (queue, ctx)
    }

    #[test]
    fn enqueue_runs_native_handler() {
        let (queue, ctx) = test_ctx();
        register_builtins(&queue);
        let _workers = start_workers(queue.clone(), ctx, 2);
        let id = queue.enqueue("ferro.ping", json!({"x": 1}), "default", "test");
        // wait for completion
        for _ in 0..200 {
            if queue.status(&id) == Some(JobStatus::Finished) {
                break;
            }
            thread::sleep(Duration::from_millis(10));
        }
        assert_eq!(queue.status(&id), Some(JobStatus::Finished));
        let res = queue.result(&id).unwrap();
        assert_eq!(res["pong"], json!(true));
        assert_eq!(res["echo"], json!({"x": 1}));
        queue.shutdown();
    }

    #[test]
    fn unknown_method_does_not_crash_worker() {
        let (queue, ctx) = test_ctx();
        let _workers = start_workers(queue.clone(), ctx, 1);
        let id = queue.enqueue("does.not.exist", json!({}), "default", "test");
        for _ in 0..200 {
            if queue.status(&id) == Some(JobStatus::Finished) {
                break;
            }
            thread::sleep(Duration::from_millis(10));
        }
        // no handler => treated as a finished no-op, worker stays alive for the next job
        assert_eq!(queue.status(&id), Some(JobStatus::Finished));
        let id2 = queue.enqueue("ferro.ping", json!({}), "default", "test");
        register_builtins(&queue);
        for _ in 0..200 {
            if queue.status(&id2) == Some(JobStatus::Finished) {
                break;
            }
            thread::sleep(Duration::from_millis(10));
        }
        assert_eq!(queue.status(&id2), Some(JobStatus::Finished));
        queue.shutdown();
    }
}
