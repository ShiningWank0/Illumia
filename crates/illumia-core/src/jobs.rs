//! SQLite-backed job queue and a small `std::thread` worker pool.

use std::{
    collections::HashMap,
    sync::{
        Arc, Condvar, Mutex,
        atomic::{AtomicBool, Ordering},
    },
    thread::{self, JoinHandle},
    time::Duration,
};

use chrono::Utc;
use rusqlite::{OptionalExtension, TransactionBehavior, params};
use uuid::Uuid;

use crate::{
    assets::timestamp,
    db::{Database, Error, Result},
    settings::Settings,
};

const IDLE_WAIT: Duration = Duration::from_millis(100);

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum JobState {
    Queued,
    Running,
    Done,
    Failed,
    Cancelled,
}

impl JobState {
    fn from_db(value: &str) -> rusqlite::Result<Self> {
        match value {
            "queued" => Ok(Self::Queued),
            "running" => Ok(Self::Running),
            "done" => Ok(Self::Done),
            "failed" => Ok(Self::Failed),
            "cancelled" => Ok(Self::Cancelled),
            _ => Err(rusqlite::Error::InvalidColumnType(
                3,
                "state".to_owned(),
                rusqlite::types::Type::Text,
            )),
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct Job {
    pub id: String,
    pub kind: String,
    pub payload: String,
    pub state: JobState,
    pub priority: i64,
    pub progress: f64,
    pub error: Option<String>,
    pub created_at: String,
    pub started_at: Option<String>,
    pub finished_at: Option<String>,
}

#[derive(Clone, Debug)]
pub struct JobQueue {
    database: Database,
}

impl JobQueue {
    #[must_use]
    pub fn new(database: Database) -> Self {
        Self { database }
    }

    /// Adds a queued job. The payload must be valid JSON.
    pub fn enqueue(&self, kind: &str, payload_json: &str, priority: i64) -> Result<Job> {
        serde_json::from_str::<serde_json::Value>(payload_json)?;
        let id = Uuid::now_v7().to_string();
        let created_at = timestamp(Utc::now());
        self.database.with_connection(|connection| {
            connection.execute(
                "INSERT INTO jobs(
                    id, kind, payload, state, priority, progress, error,
                    created_at, started_at, finished_at
                 ) VALUES (?1, ?2, ?3, 'queued', ?4, 0, NULL, ?5, NULL, NULL)",
                params![id, kind, payload_json, priority, created_at],
            )?;
            job_by_id(connection, &id)?
                .ok_or_else(|| Error::InvalidJobState("newly inserted job disappeared".to_owned()))
        })
    }

    /// Atomically claims the first job in `ix_jobs_queue` order.
    pub fn claim(&self) -> Result<Option<Job>> {
        self.database.with_connection_mut(|connection| {
            let transaction =
                connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
            let id = transaction
                .query_row(
                    "SELECT id
                     FROM jobs
                     WHERE state = 'queued'
                     ORDER BY priority DESC, created_at, id
                     LIMIT 1",
                    [],
                    |row| row.get::<_, String>(0),
                )
                .optional()?;
            let Some(id) = id else {
                transaction.commit()?;
                return Ok(None);
            };

            let changed = transaction.execute(
                "UPDATE jobs
                 SET state = 'running',
                     started_at = ?2,
                     finished_at = NULL,
                     error = NULL
                 WHERE id = ?1 AND state = 'queued'",
                params![id, timestamp(Utc::now())],
            )?;
            let job = if changed == 1 {
                job_by_id(&transaction, &id)?
            } else {
                None
            };
            transaction.commit()?;
            Ok(job)
        })
    }

    pub fn complete(&self, id: &str) -> Result<bool> {
        self.finish(id, "done", None)
    }

    pub fn fail(&self, id: &str, error: &str) -> Result<bool> {
        self.finish(id, "failed", Some(error))
    }

    pub fn cancel(&self, id: &str) -> Result<bool> {
        self.database.with_connection(|connection| {
            let changed = connection.execute(
                "UPDATE jobs
                 SET state = 'cancelled', finished_at = ?2
                 WHERE id = ?1 AND state IN ('queued','running')",
                params![id, timestamp(Utc::now())],
            )?;
            Ok(changed == 1)
        })
    }

    pub fn update_progress(&self, id: &str, progress: f64) -> Result<bool> {
        if !(0.0..=1.0).contains(&progress) {
            return Err(Error::InvalidJobProgress);
        }
        self.database.with_connection(|connection| {
            let changed = connection.execute(
                "UPDATE jobs SET progress = ?2
                 WHERE id = ?1 AND state = 'running'",
                params![id, progress],
            )?;
            Ok(changed == 1)
        })
    }

    pub fn list(&self) -> Result<Vec<Job>> {
        self.database.with_connection(|connection| {
            let mut statement = connection.prepare(
                "SELECT
                    id, kind, payload, state, priority, progress, error,
                    created_at, started_at, finished_at
                 FROM jobs
                 ORDER BY created_at DESC, id DESC",
            )?;
            let jobs = statement
                .query_map([], job_from_row)?
                .collect::<std::result::Result<Vec<_>, _>>()?;
            Ok(jobs)
        })
    }

    /// Resets jobs left `running` by a previous process to the queue.
    pub fn recover(&self) -> Result<usize> {
        self.database.with_connection(|connection| {
            let changed = connection.execute(
                "UPDATE jobs
                 SET state = 'queued',
                     progress = 0,
                     error = NULL,
                     started_at = NULL,
                     finished_at = NULL
                 WHERE state = 'running'",
                [],
            )?;
            Ok(changed)
        })
    }

    fn finish(&self, id: &str, state: &str, error: Option<&str>) -> Result<bool> {
        self.database.with_connection(|connection| {
            let changed = connection.execute(
                "UPDATE jobs
                 SET state = ?2,
                     progress = CASE WHEN ?2 = 'done' THEN 1 ELSE progress END,
                     error = ?3,
                     finished_at = ?4
                 WHERE id = ?1 AND state = 'running'",
                params![id, state, error, timestamp(Utc::now())],
            )?;
            Ok(changed == 1)
        })
    }
}

type JobHandler = dyn Fn(&Database, &Job) -> Result<()> + Send + Sync + 'static;

/// Worker pool that claims SQLite jobs and dispatches them by `kind`.
pub struct JobRunner {
    database: Database,
    handlers: HashMap<String, Arc<JobHandler>>,
    running: Option<RunningPool>,
}

struct RunningPool {
    shutdown: Arc<AtomicBool>,
    wake: Arc<(Mutex<()>, Condvar)>,
    workers: Vec<JoinHandle<()>>,
}

impl JobRunner {
    #[must_use]
    pub fn new(database: Database) -> Self {
        Self {
            database,
            handlers: HashMap::new(),
            running: None,
        }
    }

    pub fn register_handler<F>(&mut self, kind: impl Into<String>, handler: F)
    where
        F: Fn(&Database, &Job) -> Result<()> + Send + Sync + 'static,
    {
        self.handlers.insert(kind.into(), Arc::new(handler));
    }

    /// Recovers interrupted jobs and starts the configured number of workers.
    pub fn start(&mut self) -> Result<()> {
        if self.running.is_some() {
            return Err(Error::JobRunnerAlreadyStarted);
        }
        self.recover()?;

        let worker_count = configured_concurrency(&self.database)?;
        let shutdown = Arc::new(AtomicBool::new(false));
        let wake = Arc::new((Mutex::new(()), Condvar::new()));
        let handlers = Arc::new(self.handlers.clone());
        let mut workers = Vec::with_capacity(worker_count);

        for index in 0..worker_count {
            let worker_database = self.database.clone();
            let worker_shutdown = Arc::clone(&shutdown);
            let worker_wake = Arc::clone(&wake);
            let worker_handlers = Arc::clone(&handlers);
            let handle = thread::Builder::new()
                .name(format!("illumia-job-{index}"))
                .spawn(move || {
                    worker_loop(
                        worker_database,
                        worker_handlers,
                        worker_shutdown,
                        worker_wake,
                    );
                });
            match handle {
                Ok(handle) => workers.push(handle),
                Err(error) => {
                    shutdown.store(true, Ordering::Release);
                    wake.1.notify_all();
                    for worker in workers {
                        let _ = worker.join();
                    }
                    return Err(error.into());
                }
            }
        }

        self.running = Some(RunningPool {
            shutdown,
            wake,
            workers,
        });
        Ok(())
    }

    pub fn recover(&self) -> Result<usize> {
        JobQueue::new(self.database.clone()).recover()
    }

    /// Stops new claims and waits for every in-flight handler to return.
    pub fn shutdown(&mut self) -> Result<()> {
        let Some(running) = self.running.take() else {
            return Ok(());
        };
        running.shutdown.store(true, Ordering::Release);
        running.wake.1.notify_all();

        let mut panicked = false;
        for worker in running.workers {
            if worker.join().is_err() {
                panicked = true;
            }
        }
        if panicked {
            Err(Error::JobWorkerPanicked)
        } else {
            Ok(())
        }
    }
}

impl Drop for JobRunner {
    fn drop(&mut self) {
        let _ = self.shutdown();
    }
}

fn worker_loop(
    database: Database,
    handlers: Arc<HashMap<String, Arc<JobHandler>>>,
    shutdown: Arc<AtomicBool>,
    wake: Arc<(Mutex<()>, Condvar)>,
) {
    let queue = JobQueue::new(database.clone());
    while !shutdown.load(Ordering::Acquire) {
        match queue.claim() {
            Ok(Some(job)) => {
                let result = handlers.get(&job.kind).map_or_else(
                    || Err(Error::InvalidJobState("no handler registered".to_owned())),
                    |handler| {
                        std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                            handler(&database, &job)
                        }))
                        .unwrap_or_else(|_| {
                            Err(Error::InvalidJobState("job handler panicked".to_owned()))
                        })
                    },
                );
                match result {
                    Ok(()) => {
                        let _ = queue.complete(&job.id);
                    }
                    Err(error) => {
                        let _ = queue.fail(&job.id, &error.to_string());
                    }
                }
            }
            Ok(None) | Err(_) => wait_for_work(&wake, &shutdown),
        }
    }
}

fn wait_for_work(wake: &(Mutex<()>, Condvar), shutdown: &AtomicBool) {
    if shutdown.load(Ordering::Acquire) {
        return;
    }
    if let Ok(guard) = wake.0.lock() {
        let _wait_result = wake.1.wait_timeout(guard, IDLE_WAIT);
    }
}

fn configured_concurrency(database: &Database) -> Result<usize> {
    let configured = Settings::new(database.clone()).thumbnail_concurrency()?;
    Ok(configured.map_or_else(default_concurrency, |value| {
        usize::try_from(value).unwrap_or(usize::MAX).max(1)
    }))
}

fn default_concurrency() -> usize {
    physical_core_count()
        .or_else(|| thread::available_parallelism().ok().map(usize::from))
        .unwrap_or(1)
        .saturating_sub(1)
        .max(1)
}

#[cfg(target_os = "linux")]
fn physical_core_count() -> Option<usize> {
    let cpu_info = std::fs::read_to_string("/proc/cpuinfo").ok()?;
    let mut cores = std::collections::HashSet::new();
    for block in cpu_info.split("\n\n") {
        let mut physical_id = None;
        let mut core_id = None;
        let mut processor = None;
        for line in block.lines() {
            let Some((key, value)) = line.split_once(':') else {
                continue;
            };
            match key.trim() {
                "physical id" => physical_id = Some(value.trim()),
                "core id" => core_id = Some(value.trim()),
                "processor" => processor = Some(value.trim()),
                _ => {}
            }
        }
        let key = match (physical_id, core_id, processor) {
            (Some(package), Some(core), _) => format!("{package}:{core}"),
            (_, Some(core), _) => format!("core:{core}"),
            (_, _, Some(processor)) => format!("processor:{processor}"),
            _ => continue,
        };
        cores.insert(key);
    }
    (!cores.is_empty()).then_some(cores.len())
}

#[cfg(target_os = "macos")]
fn physical_core_count() -> Option<usize> {
    let output = std::process::Command::new("sysctl")
        .args(["-n", "hw.physicalcpu"])
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    std::str::from_utf8(&output.stdout)
        .ok()?
        .trim()
        .parse()
        .ok()
}

#[cfg(not(any(target_os = "linux", target_os = "macos")))]
fn physical_core_count() -> Option<usize> {
    None
}

fn job_by_id(connection: &rusqlite::Connection, id: &str) -> Result<Option<Job>> {
    connection
        .query_row(
            "SELECT
                id, kind, payload, state, priority, progress, error,
                created_at, started_at, finished_at
             FROM jobs
             WHERE id = ?1",
            [id],
            job_from_row,
        )
        .optional()
        .map_err(Into::into)
}

fn job_from_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<Job> {
    let state = row.get::<_, String>(3)?;
    Ok(Job {
        id: row.get(0)?,
        kind: row.get(1)?,
        payload: row.get(2)?,
        state: JobState::from_db(&state)?,
        priority: row.get(4)?,
        progress: row.get(5)?,
        error: row.get(6)?,
        created_at: row.get(7)?,
        started_at: row.get(8)?,
        finished_at: row.get(9)?,
    })
}
