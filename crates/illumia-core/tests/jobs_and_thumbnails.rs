use std::{
    fs,
    io::Cursor,
    sync::{Arc, mpsc},
    thread,
    time::Duration,
};

use illumia_core::{
    PurgeService,
    assets::AssetService,
    db::{Database, Result},
    jobs::{JobQueue, JobRunner, JobState},
    settings::Settings,
    thumbnails::{
        THUMBNAIL_JOB_KIND, enqueue_thumbnail, generate_thumbnails, handle_thumbnail_job,
    },
};
use image::{DynamicImage, GenericImageView, ImageFormat, Rgba, RgbaImage};
use tempfile::TempDir;

struct Fixture {
    _directory: TempDir,
    database: Database,
    assets: AssetService,
}

impl Fixture {
    fn new() -> Result<Self> {
        let directory = tempfile::tempdir()?;
        let database = Database::open(directory.path())?;
        Ok(Self {
            assets: AssetService::new(database.clone()),
            database,
            _directory: directory,
        })
    }

    fn ingest(&self, width: u32, height: u32) -> illumia_core::assets::Asset {
        self.assets
            .ingest(&png(width, height), "thumbnail-source.png", None)
            .expect("asset should be ingested")
            .asset
    }
}

fn png(width: u32, height: u32) -> Vec<u8> {
    let mut pixels = RgbaImage::new(width, height);
    for (x, y, pixel) in pixels.enumerate_pixels_mut() {
        *pixel = Rgba([
            u8::try_from(x % 251).expect("bounded red channel"),
            u8::try_from(y % 241).expect("bounded green channel"),
            u8::try_from((x + y) % 239).expect("bounded blue channel"),
            255,
        ]);
    }
    let mut output = Cursor::new(Vec::new());
    DynamicImage::ImageRgba8(pixels)
        .write_to(&mut output, ImageFormat::Png)
        .expect("PNG should encode");
    output.into_inner()
}

#[test]
fn queue_claims_by_priority() -> Result<()> {
    let fixture = Fixture::new()?;
    let queue = JobQueue::new(fixture.database);
    let low = queue.enqueue("test", r#"{"priority":"low"}"#, -5)?;
    let high = queue.enqueue("test", r#"{"priority":"high"}"#, 50)?;
    let normal = queue.enqueue("test", r#"{"priority":"normal"}"#, 0)?;

    assert_eq!(queue.claim()?.map(|job| job.id), Some(high.id));
    assert_eq!(queue.claim()?.map(|job| job.id), Some(normal.id));
    assert_eq!(queue.claim()?.map(|job| job.id), Some(low.id));
    assert!(queue.claim()?.is_none());
    Ok(())
}

#[test]
fn parallel_claim_has_mutual_exclusion() -> Result<()> {
    let fixture = Fixture::new()?;
    let queue = JobQueue::new(fixture.database);
    let queued = queue.enqueue("test", "{}", 0)?;
    let barrier = Arc::new(std::sync::Barrier::new(9));
    let mut workers = Vec::new();

    for _ in 0..8 {
        let worker_queue = queue.clone();
        let worker_barrier = Arc::clone(&barrier);
        workers.push(thread::spawn(move || {
            worker_barrier.wait();
            worker_queue.claim()
        }));
    }
    barrier.wait();

    let claimed = workers
        .into_iter()
        .map(|worker| worker.join().expect("claim worker should not panic"))
        .collect::<Result<Vec<_>>>()?
        .into_iter()
        .flatten()
        .collect::<Vec<_>>();
    assert_eq!(claimed.len(), 1);
    assert_eq!(claimed[0].id, queued.id);
    Ok(())
}

#[test]
fn restart_recovery_requeues_running_jobs() -> Result<()> {
    let fixture = Fixture::new()?;
    let queue = JobQueue::new(fixture.database);
    let queued = queue.enqueue("test", "{}", 0)?;
    let running = queue.claim()?.expect("job should be claimable");
    assert_eq!(running.id, queued.id);
    assert!(queue.update_progress(&running.id, 0.75)?);

    assert_eq!(queue.recover()?, 1);
    let recovered = queue.claim()?.expect("recovered job should be claimable");
    assert_eq!(recovered.id, queued.id);
    assert_eq!(recovered.state, JobState::Running);
    assert_eq!(recovered.progress, 0.0);
    assert!(recovered.error.is_none());
    Ok(())
}

#[test]
fn queue_supports_progress_failure_cancellation_and_listing() -> Result<()> {
    let fixture = Fixture::new()?;
    let queue = JobQueue::new(fixture.database);
    let failed = queue.enqueue("test", "{}", 1)?;
    let cancelled = queue.enqueue("test", "{}", 0)?;

    let running = queue.claim()?.expect("job should be claimable");
    assert_eq!(running.id, failed.id);
    assert!(queue.update_progress(&running.id, 0.5)?);
    assert!(queue.fail(&running.id, "expected failure")?);
    assert!(queue.cancel(&cancelled.id)?);
    assert!(!queue.complete(&cancelled.id)?);

    let jobs = queue.list()?;
    let failed = jobs
        .iter()
        .find(|job| job.id == failed.id)
        .expect("failed job should be listed");
    let cancelled = jobs
        .iter()
        .find(|job| job.id == cancelled.id)
        .expect("cancelled job should be listed");
    assert_eq!(failed.state, JobState::Failed);
    assert_eq!(failed.progress, 0.5);
    assert_eq!(failed.error.as_deref(), Some("expected failure"));
    assert!(failed.finished_at.is_some());
    assert_eq!(cancelled.state, JobState::Cancelled);
    assert!(cancelled.finished_at.is_some());
    Ok(())
}

#[test]
fn job_runner_shutdown_waits_for_in_flight_handler() -> Result<()> {
    let fixture = Fixture::new()?;
    Settings::new(fixture.database.clone()).set_thumbnail_concurrency(1)?;
    let queue = JobQueue::new(fixture.database.clone());
    let job = queue.enqueue("wait", "{}", 0)?;
    let (started_tx, started_rx) = mpsc::channel();
    let mut runner = JobRunner::new(fixture.database);
    runner.register_handler("wait", move |_, _| {
        started_tx
            .send(())
            .expect("test receiver should remain connected");
        thread::sleep(Duration::from_millis(50));
        Ok(())
    });

    runner.start()?;
    started_rx
        .recv_timeout(Duration::from_secs(2))
        .expect("worker should start");
    runner.shutdown()?;

    let completed = queue
        .list()?
        .into_iter()
        .find(|candidate| candidate.id == job.id)
        .expect("job should remain listed");
    assert_eq!(completed.state, JobState::Done);
    assert_eq!(completed.progress, 1.0);
    Ok(())
}

#[test]
fn thumbnail_generation_creates_decodable_expected_sizes_and_thumbhash() -> Result<()> {
    let fixture = Fixture::new()?;
    let asset = fixture.ingest(1800, 900);
    let queue = JobQueue::new(fixture.database.clone());
    let enqueued = enqueue_thumbnail(&fixture.database, &asset.id)?;
    assert_eq!(enqueued.kind, THUMBNAIL_JOB_KIND);
    let claimed = queue.claim()?.expect("thumbnail job should be claimable");

    handle_thumbnail_job(&fixture.database, &claimed)?;
    assert!(queue.complete(&claimed.id)?);

    let thumbnail_path = fixture
        .database
        .data_root()
        .join("thumbs")
        .join(format!("{}_t.webp", asset.id));
    let preview_path = fixture
        .database
        .data_root()
        .join("thumbs")
        .join(format!("{}_p.webp", asset.id));
    let thumbnail = image::open(&thumbnail_path)?;
    let preview = image::open(&preview_path)?;
    assert_eq!(thumbnail.dimensions(), (240, 120));
    assert_eq!(preview.dimensions(), (1440, 720));
    assert_eq!(
        image::guess_format(&fs::read(&thumbnail_path)?)?,
        ImageFormat::WebP
    );
    assert_eq!(
        image::guess_format(&fs::read(&preview_path)?)?,
        ImageFormat::WebP
    );
    let thumbhash = fixture
        .assets
        .get(&asset.id)?
        .expect("asset should exist")
        .thumbhash
        .expect("ThumbHash should be populated");
    assert!(!thumbhash.is_empty());
    Ok(())
}

#[test]
fn thumbnail_generation_is_idempotent_without_reading_source_again() -> Result<()> {
    let fixture = Fixture::new()?;
    let asset = fixture.ingest(320, 160);
    let job = enqueue_thumbnail(&fixture.database, &asset.id)?;
    handle_thumbnail_job(&fixture.database, &job)?;

    let thumbnail_path = fixture
        .database
        .data_root()
        .join("thumbs")
        .join(format!("{}_t.webp", asset.id));
    let preview_path = fixture
        .database
        .data_root()
        .join("thumbs")
        .join(format!("{}_p.webp", asset.id));
    let thumbnail_before = fs::read(&thumbnail_path)?;
    let preview_before = fs::read(&preview_path)?;
    assert_eq!(image::open(&thumbnail_path)?.dimensions(), (240, 120));
    assert_eq!(image::open(&preview_path)?.dimensions(), (320, 160));
    let hash_before = fixture
        .assets
        .get(&asset.id)?
        .expect("asset should exist")
        .thumbhash
        .expect("ThumbHash should exist");

    fs::remove_file(fixture.database.data_root().join(&asset.library_path))?;
    handle_thumbnail_job(&fixture.database, &job)?;

    assert_eq!(fs::read(thumbnail_path)?, thumbnail_before);
    assert_eq!(fs::read(preview_path)?, preview_before);
    assert_eq!(
        fixture
            .assets
            .get(&asset.id)?
            .expect("asset should exist")
            .thumbhash
            .expect("ThumbHash should remain"),
        hash_before
    );
    Ok(())
}

#[test]
fn thumbnail_generation_silently_skips_purging_and_deleted_assets() -> Result<()> {
    let fixture = Fixture::new()?;
    let asset = fixture.ingest(320, 160);
    fixture.database.with_connection(|connection| {
        connection.execute(
            "UPDATE assets SET lifecycle = 'purging' WHERE id = ?1",
            [&asset.id],
        )?;
        Ok(())
    })?;

    generate_thumbnails(&fixture.database, &asset.id)?;
    let thumbnail_path = fixture
        .database
        .data_root()
        .join("thumbs")
        .join(format!("{}_t.webp", asset.id));
    assert!(!thumbnail_path.exists());
    assert_eq!(
        PurgeService::new(fixture.database.clone()).resume_purging()?,
        1
    );
    generate_thumbnails(&fixture.database, &asset.id)?;
    assert!(!thumbnail_path.exists());
    Ok(())
}
